//! Reacting, editing and deleting your own messages.
//!
//! A second `impl App` block: the state lives on `App` in `app.rs`, and what is here is only the behaviour that reads and
//! changes it, so `app.rs` stays the one place to see what `App` holds. Private methods are `pub(super)` so `app.rs` and the
//! other feature modules can call them.

use super::*;

impl App {
    pub(super) fn own(&mut self, m: &Ev, what: &str) -> bool {
        if m.pubkey != self.me || m.kind != 9 {
            self.flash(format!("you can only {what} your own messages"));
            return false;
        }
        true
    }

    pub(super) fn start_edit(&mut self, m: &Ev) -> Vec<Cmd> {
        if self.read_only {
            self.flash("read-only mode: nothing is sent");
            return Vec::new();
        }
        if !self.own(m, "edit") {
            return Vec::new();
        }
        let text = self.store().map(|s| s.content_of(m).0.to_owned()).unwrap_or_else(|| m.content.clone());
        // The edit takes the box over, so the draft that was in it goes back to the place it was typed and comes back
        // when the edit is over (see `finish_edit` and the Esc arm in `key_input`).
        let cmds = self.stash_draft();
        self.editor.clear();
        self.editor.insert_str(&text);
        self.editing = Some(m.id.clone());
        self.set_focus(Focus::Input);
        self.flash("editing your message: Enter saves it, Esc cancels");
        cmds
    }

    pub(super) fn finish_edit(&mut self, id: String, text: String) -> Vec<Cmd> {
        let original = self.store().and_then(|s| s.msgs.get(&id).map(|m| s.content_of(m).0.to_owned()));
        if text.is_empty() {
            self.flash("an edited message cannot be empty (Esc cancels; d deletes it)");
            return Vec::new();
        }
        self.editing = None;
        self.editor.clear();
        // the edit is over: the box goes back to the draft of the place we are in
        let mut cmds = self.switch_draft();
        if original.as_deref() == Some(text.as_str()) {
            self.flash("nothing changed");
            return cmds;
        }
        let Some(c) = self.channel() else { return cmds };
        cmds.push(Cmd::Edit { channel: c.id.clone(), id, text });
        cmds
    }

    /// `a`: open the reaction picker on the selected message. It starts on the reaction you already have there, when it is in the
    /// table, so `a` then Enter changes nothing and the row above says how to take it back — see `key_react`/`activate_react`.
    pub(super) fn open_react(&mut self, m: &Ev) {
        if self.read_only {
            self.flash("read-only mode: nothing is sent");
            return;
        }
        self.react = Some(ReactPicker { target: m.id.clone(), query: String::new(), cursor: 0 });
        if let Some((_, mine)) = self.my_reaction(&m.id) {
            if let Some(row) = self.react_rows().iter().position(|r| matches!(r, ReactRow::Emoji(g) if *g == mine)) {
                if let Some(r) = self.react.as_mut() {
                    r.cursor = row;
                }
            }
        }
    }

    /// Your own reaction on a message: the reaction event id (so it can be undone) and the emoji, when you reacted to it.
    pub(super) fn my_reaction(&self, target: &str) -> Option<(String, String)> {
        let store = self.store()?;
        store
            .reaction_ids
            .iter()
            .find(|(_, (msg, _, who))| msg == target && who == &self.me)
            .map(|(id, (_, emoji, _))| (id.clone(), emoji.clone()))
    }

    /// The rows the picker shows for what has been typed: taking your reaction back first (when you have one), then the emoji whose
    /// name matches. With no match, the last resort is what you typed — the picker can send an emoji the table does not have.
    pub fn react_rows(&self) -> Vec<ReactRow> {
        let Some(r) = &self.react else { return Vec::new() };
        let mut rows = Vec::new();
        if let Some((id, _)) = self.my_reaction(&r.target) {
            rows.push(ReactRow::Remove(id));
        }
        let hits = crate::emoji::search(&r.query);
        if hits.is_empty() {
            let typed = emoji_of(&r.query);
            if !typed.is_empty() {
                rows.push(ReactRow::Typed(typed));
            }
        } else {
            rows.extend(hits.into_iter().map(|(glyph, _)| ReactRow::Emoji(glyph.to_owned())));
        }
        rows
    }

    /// Keys in the picker: typing narrows the list, the arrows move the row, Enter sends, Esc drops it.
    pub(super) fn key_react(&mut self, k: KeyEvent) -> Vec<Cmd> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let n = self.react_rows().len();
        match k.code {
            KeyCode::Esc => self.react = None,
            KeyCode::Enter => return self.activate_react(),
            KeyCode::Up => {
                if let Some(r) = self.react.as_mut() {
                    r.cursor = r.cursor.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                if let Some(r) = self.react.as_mut() {
                    r.cursor = (r.cursor + 1).min(n.saturating_sub(1));
                }
            }
            KeyCode::Backspace => self.edit_react(|q| {
                q.pop();
            }),
            KeyCode::Char('u') if ctrl => self.edit_react(|q| q.clear()),
            KeyCode::Char(c) if !ctrl => self.edit_react(|q| q.push(c)),
            _ => {}
        }
        Vec::new()
    }

    pub(super) fn edit_react(&mut self, f: impl FnOnce(&mut String)) {
        if let Some(r) = self.react.as_mut() {
            f(&mut r.query);
        }
        // the rows changed with the query: put the cursor on the first emoji to send, not on the "remove" row
        let first = self.react_rows().iter().position(|row| !matches!(row, ReactRow::Remove(_))).unwrap_or(0);
        if let Some(r) = self.react.as_mut() {
            r.cursor = first;
        }
    }

    /// Enter in the picker: send the emoji on the cursor line, or undo the reaction the row stands for.
    pub(super) fn activate_react(&mut self) -> Vec<Cmd> {
        let Some(r) = self.react.clone() else { return Vec::new() };
        let rows = self.react_rows();
        let Some(row) = rows.get(r.cursor).or_else(|| rows.first()).cloned() else {
            self.flash("type an emoji or a word like \"fire\" (Esc cancels)");
            return Vec::new();
        };
        self.react = None;
        match row {
            ReactRow::Remove(id) => {
                self.flash("reaction removed");
                vec![Cmd::React { target: r.target, emoji: String::new(), remove: Some(id) }]
            }
            ReactRow::Emoji(glyph) | ReactRow::Typed(glyph) => match self.my_reaction(&r.target) {
                Some((_, mine)) if mine == glyph => {
                    self.flash(format!("already reacted {glyph}"));
                    Vec::new()
                }
                Some((id, _)) => {
                    self.flash(format!("reacted {glyph}"));
                    vec![Cmd::React { target: r.target, emoji: glyph, remove: Some(id) }]
                }
                None => {
                    self.flash(format!("reacted {glyph}"));
                    vec![Cmd::React { target: r.target, emoji: glyph, remove: None }]
                }
            },
        }
    }

    pub(super) fn ask_delete(&mut self, m: &Ev) {
        if self.read_only {
            self.flash("read-only mode: nothing is sent");
            return;
        }
        if self.own(m, "delete") {
            self.confirm_delete = Some(m.id.clone());
        }
    }

    /// The yes/no after `d`: `y` deletes, anything else cancels.
    pub(super) fn answer_delete(&mut self, k: KeyEvent) -> Vec<Cmd> {
        let Some(id) = self.confirm_delete.take() else { return Vec::new() };
        if !matches!(k.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
            self.flash("not deleted");
            return Vec::new();
        }
        let Some(channel) = self.channel().map(|c| c.id.clone()) else { return Vec::new() };
        self.flash("deleting…");
        vec![Cmd::Delete { channel, id }]
    }
}
