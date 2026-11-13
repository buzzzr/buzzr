//! Full-text search: the overlay, its debounce and its results.
//!
//! A second `impl App` block: the state lives on `App` in `app.rs`, and what is here is only the behaviour that reads and
//! changes it, so `app.rs` stays the one place to see what `App` holds. Private methods are `pub(super)` so `app.rs` and the
//! other feature modules can call them.

use super::*;

impl App {
    /// Open the search overlay. The channel rows carry what a channel is for, how many people are in it and whether we are one
    /// of them, so the roster is asked for again as it opens — that detail is what search shows about a channel.
    pub fn open_search(&mut self, query: String) -> Vec<Cmd> {
        self.overlay = Overlay::Search;
        self.search.query.clear();
        self.search.query.insert_str(&query);
        self.search.messages.clear();
        self.search.asked.clear();
        self.search.cursor = 0;
        self.search.loading = false;
        self.search.edited_at = Instant::now();
        self.load_roster()
    }

    pub(super) fn run_search(&mut self) -> Vec<Cmd> {
        let query = self.search.query.text();
        self.search.seq += 1;
        self.search.asked = query.clone();
        self.search.loading = true;
        vec![Cmd::Search { query: query.trim().to_owned(), seq: self.search.seq }]
    }

    pub(super) fn search_results(&mut self, seq: u64, result: Result<Vec<Ev>, String>) -> Vec<Cmd> {
        if seq != self.search.seq {
            return Vec::new(); // an answer to a query we have since changed
        }
        self.search.loading = false;
        match result {
            Ok(events) => {
                let known: HashSet<&str> = self.channels.iter().map(|c| c.id.as_str()).collect();
                self.search.messages = events.into_iter().filter(|e| MESSAGE_KINDS.contains(&e.kind) && e.channel().map(|h| known.contains(h)).unwrap_or(false)).collect();
                self.search.cursor = 0;
                let authors: Vec<String> = self.search.messages.iter().map(|e| e.pubkey.clone()).collect();
                self.want_names(authors)
            }
            Err(e) => {
                self.flash(format!("search failed: {e}"));
                Vec::new()
            }
        }
    }

    /// Channel-name matches (local), then people (local), then message matches (from the relay).
    pub fn search_rows(&self) -> Vec<Row> {
        let q = self.search.query.text().trim().to_lowercase();
        let mut rows = Vec::new();
        if !q.is_empty() {
            for (i, c) in self.channels.iter().enumerate() {
                if c.label.to_lowercase().contains(&q) || c.name.to_lowercase().contains(&q) {
                    rows.push(Row::Channel(i));
                }
            }
            rows.extend(self.people_matching(&q).into_iter().map(Row::Person));
        }
        rows.extend(self.search.messages.iter().cloned().map(Row::Message));
        rows
    }

    pub(super) fn key_search(&mut self, k: KeyEvent) -> Vec<Cmd> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let n = self.search_rows().len();
        match k.code {
            KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Up => self.search.cursor = self.search.cursor.saturating_sub(1),
            KeyCode::Down => self.search.cursor = (self.search.cursor + 1).min(n.saturating_sub(1)),
            KeyCode::PageUp => self.search.cursor = self.search.cursor.saturating_sub(8),
            KeyCode::PageDown => self.search.cursor = (self.search.cursor + 8).min(n.saturating_sub(1)),
            KeyCode::Enter => {
                // Enter on a query that has not been searched yet searches now; otherwise it opens the selected row
                let q = self.search.query.text();
                if q != self.search.asked && q.trim().chars().count() >= 2 {
                    return self.run_search();
                }
                return self.activate_search();
            }
            KeyCode::Backspace => self.edit_query(|e| e.backspace()),
            KeyCode::Delete => self.edit_query(|e| e.delete()),
            KeyCode::Left => self.search.query.left(),
            KeyCode::Right => self.search.query.right(),
            KeyCode::Home => self.search.query.home(),
            KeyCode::End => self.search.query.end(),
            KeyCode::Char('u') if ctrl => self.edit_query(|e| e.kill_to_start()),
            KeyCode::Char('w') if ctrl => self.edit_query(|e| e.kill_word_back()),
            KeyCode::Char('f') if ctrl => self.overlay = Overlay::None,
            KeyCode::Char(c) if !ctrl => self.edit_query(|e| e.insert_char(c)),
            _ => {}
        }
        Vec::new()
    }

    pub(super) fn edit_query(&mut self, f: impl FnOnce(&mut Editor)) {
        f(&mut self.search.query);
        self.search.edited_at = Instant::now();
        self.search.cursor = 0;
    }

    /// Open the selected result: a channel, or a message in its channel (inside its thread if it is a reply), with the message selected.
    pub(super) fn activate_search(&mut self) -> Vec<Cmd> {
        let rows = self.search_rows();
        let Some(row) = rows.into_iter().nth(self.search.cursor) else { return Vec::new() };
        self.overlay = Overlay::None;
        match row {
            Row::Channel(i) => self.open_from_list(i),
            Row::Person(pk) => self.open_dm(&pk),
            Row::Message(ev) => {
                let Some(i) = ev.channel().and_then(|h| self.channels.iter().position(|c| c.id == h)) else { return Vec::new() };
                let id = self.channels[i].id.clone();
                self.stores.entry(id).or_default().apply(&ev);
                let root = self.stores.get(&self.channels[i].id).map(|s| s.thread_root(&ev).to_owned()).unwrap_or_else(|| ev.id.clone());
                let mut cmds = self.open_channel(i);
                if root != ev.id {
                    cmds.extend(self.open_thread(root));
                }
                self.pending_select = Some(ev.id.clone());
                self.set_focus(Focus::Messages);
                cmds
            }
        }
    }
}
