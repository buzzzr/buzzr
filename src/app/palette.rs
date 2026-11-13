//! The command line (`/`) and the Ctrl+K palette: parsing a command, listing and running it.
//!
//! A second `impl App` block: the state lives on `App` in `app.rs`, and what is here is only the behaviour that reads and
//! changes it, so `app.rs` stays the one place to see what `App` holds. Private methods are `pub(super)` so `app.rs` and the
//! other feature modules can call them.

use super::*;

impl App {
    pub(super) fn command(&mut self, cmd: &str, arg: &str) -> Vec<Cmd> {
        match cmd {
            "q" | "quit" => {
                self.quit = true;
                Vec::new()
            }
            "plugins" => {
                self.show_plugins();
                Vec::new()
            }
            "update" => {
                self.flash(self.update_source.advice());
                Vec::new()
            }
            "refresh" => self.refresh(),
            "more" => self.load_older(),
            "read" => self.mark_read_command(arg),
            "archive" => match self.cur {
                Some(i) => self.archive(i),
                None => {
                    self.flash("archive takes the channel you are in (open it first)");
                    Vec::new()
                }
            },
            "search" => {
                let mut cmds = self.open_search(arg.to_owned());
                if arg.chars().count() >= 2 {
                    cmds.extend(self.run_search());
                }
                cmds
            }
            "join" => self.join_command(arg),
            "leave" => self.ask_leave(),
            "new" => self.create_channel(arg),
            "dm" => {
                if arg.trim().is_empty() {
                    self.flash("dm needs a name (a name, or an npub)");
                    return Vec::new();
                }
                match self.who_is(arg) {
                    Who::One(pk) => self.open_dm(&pk),
                    Who::Several(why) => {
                        self.flash(why);
                        Vec::new()
                    }
                    Who::Unknown => self.ask_directory(arg, Then::Dm),
                }
            }
            "whois" => self.show_whois(arg),
            "invite" => self.invite(arg),
            "edit" | "delete" => match arg.parse::<usize>().ok().and_then(|n| self.message_number(n)) {
                Some(m) if cmd == "edit" => self.start_edit(&m),
                Some(m) => {
                    self.ask_delete(&m);
                    Vec::new()
                }
                None => {
                    self.flash(format!("{cmd} needs N (the message number shown as #N)"));
                    Vec::new()
                }
            },
            "c" => self.back_to_channel(),
            "t" | "copy" => match arg.parse::<usize>().ok().and_then(|n| self.message_number(n)) {
                Some(m) if cmd == "t" => {
                    let root = self.store().map(|s| s.thread_root(&m).to_owned()).unwrap_or(m.id.clone());
                    self.open_thread(root)
                }
                Some(m) => {
                    self.copy_message(&m);
                    Vec::new()
                }
                None => {
                    self.flash(format!("{cmd} needs N (the message number shown as #N)"));
                    Vec::new()
                }
            },
            "r" => {
                let (n, text) = arg.split_once(char::is_whitespace).map(|(n, t)| (n, t.trim())).unwrap_or((arg, ""));
                match (n.parse::<usize>().ok().and_then(|n| self.message_number(n)), text.is_empty()) {
                    (Some(m), false) => {
                        let root = self.store().map(|s| s.thread_root(&m).to_owned()).unwrap_or(m.id.clone());
                        self.send(text.to_owned(), Some(root))
                    }
                    _ => {
                        self.flash("r needs N and the text");
                        Vec::new()
                    }
                }
            }
            other => {
                if let Some((plugin, c)) = self.plugins.command(other) {
                    let (plugin, command) = (plugin.to_owned(), c.id.clone());
                    // A command a plugin registered — compiled-in or external — is its own to run, through the one
                    // `plugin_input` door both kinds share.
                    return self.plugin_input(&plugin, PluginInput::Command { command, args: arg.to_owned() });
                }
                self.flash(format!("unknown command {other}"));
                Vec::new()
            }
        }
    }

    // ---- command palette (Ctrl+K) ----------------------------------------------------------------------------------------

    /// Open the palette on an empty query, with the first row under the cursor. It is a modal: it owns the keyboard until
    /// a command runs or Esc closes it, so the composer, its draft and the focus are left exactly as they were.
    pub fn open_palette(&mut self) -> Vec<Cmd> {
        self.palette = Palette::default();
        self.overlay = Overlay::Palette;
        Vec::new()
    }

    /// Close the palette and forget its query and its argument step.
    pub(super) fn close_palette(&mut self) {
        self.palette = Palette::default();
        if self.overlay == Overlay::Palette {
            self.overlay = Overlay::None;
        }
    }

    /// The head of the palette's query: the command word, up to the first space. Everything after that space is the
    /// command's argument (`dm @Alice`), so the list is filtered on the word alone.
    pub(super) fn palette_word(&self) -> String {
        let q = self.palette.query.text();
        if let Some((word, _)) = q.split_once(char::is_whitespace) {
            return word.to_owned();
        }
        q
    }

    /// The rows the palette shows: the built-ins whose name starts with the command word at the head of the query, then
    /// the commands loaded plugins added. A plugin names its own command and the argument hint it shows, the same way the
    /// built-in table does.
    pub fn command_rows(&self) -> Vec<CommandRow> {
        let q = self.palette_word().to_lowercase();
        let mut rows: Vec<CommandRow> = COMMANDS
            .iter()
            .filter(|(name, _, _)| name.starts_with(&q))
            .map(|(name, usage, desc)| CommandRow { name: (*name).to_owned(), usage: (*usage).to_owned(), desc: (*desc).to_owned() })
            .collect();
        let mut plugin_rows: Vec<CommandRow> = self
            .plugins
            .commands()
            .into_iter()
            .filter(|(_, c)| c.id.to_lowercase().starts_with(&q))
            .map(|(plugin, c)| {
                let desc = if !c.title.is_empty() { c.title.clone() } else { format!("a command from {plugin}") };
                CommandRow { name: c.id.clone(), usage: c.usage.clone(), desc }
            })
            .collect();
        // A plugin may register anything; sort its commands by name so the list does not depend on the registry's order.
        plugin_rows.sort_by(|a, b| a.name.cmp(&b.name));
        rows.extend(plugin_rows);
        rows
    }

    /// The keys the palette takes while it is up. Its query is a one-line editor: typing filters the list, or — in the
    /// argument step — is the argument itself, and a whole line typed at once (`dm @Alice`) runs on Enter as it stands.
    /// Enter picks or runs, Esc steps back out of the argument step and then closes the palette, and the arrows move the
    /// highlight rather than the query cursor.
    pub(super) fn key_palette(&mut self, k: KeyEvent) -> Vec<Cmd> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        match k.code {
            // Ctrl+K again closes it, the way it opened it.
            KeyCode::Char('k') if ctrl => {
                self.close_palette();
                Vec::new()
            }
            KeyCode::Esc => {
                // One Esc out of the argument step, a second out of the palette.
                if self.palette.arg.take().is_none() {
                    self.close_palette();
                }
                Vec::new()
            }
            KeyCode::Enter => self.palette_run(),
            KeyCode::Up => {
                self.palette_move(-1);
                Vec::new()
            }
            KeyCode::Down => {
                self.palette_move(1);
                Vec::new()
            }
            // Backspace on an empty argument step goes back to the list, so a wrong pick is one key from the right one.
            KeyCode::Backspace if self.palette.arg.is_some() && self.palette.query.is_empty() => {
                self.palette.arg = None;
                Vec::new()
            }
            KeyCode::Backspace if alt => {
                self.palette.query.kill_word_back();
                self.palette.cursor = 0;
                Vec::new()
            }
            KeyCode::Backspace => {
                self.palette.query.backspace();
                self.palette.cursor = 0;
                Vec::new()
            }
            KeyCode::Delete => {
                self.palette.query.delete();
                self.palette.cursor = 0;
                Vec::new()
            }
            KeyCode::Left => {
                self.palette.query.left();
                Vec::new()
            }
            KeyCode::Right => {
                self.palette.query.right();
                Vec::new()
            }
            KeyCode::Home => {
                self.palette.query.home();
                Vec::new()
            }
            KeyCode::End => {
                self.palette.query.end();
                Vec::new()
            }
            KeyCode::Char('a') if ctrl => {
                self.palette.query.home();
                Vec::new()
            }
            KeyCode::Char('e') if ctrl => {
                self.palette.query.end();
                Vec::new()
            }
            KeyCode::Char('w') if ctrl => {
                self.palette.query.kill_word_back();
                self.palette.cursor = 0;
                Vec::new()
            }
            KeyCode::Char('u') if ctrl => {
                self.palette.query.kill_to_start();
                self.palette.cursor = 0;
                Vec::new()
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                self.palette.query.insert_char(c);
                self.palette.cursor = 0;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Move the palette cursor by `delta` rows, staying in range.
    pub(super) fn palette_move(&mut self, delta: isize) {
        let n = self.command_rows().len();
        if n == 0 {
            self.palette.cursor = 0;
            return;
        }
        self.palette.cursor = (self.palette.cursor as isize + delta).clamp(0, n as isize - 1) as usize;
    }

    /// Enter in the palette. A query that already holds a whole command line — a command and its argument, `dm @Alice` —
    /// runs as it stands, with no argument step. Otherwise it runs the command on the cursor row — or, when that command
    /// takes an argument, asks for it first and clears the query for it. In the argument step it runs the command with
    /// what was typed, which may be nothing for a command whose argument is optional. Nothing picked and nothing matching
    /// does nothing.
    pub(super) fn palette_run(&mut self) -> Vec<Cmd> {
        if let Some(arg) = self.palette.arg.clone() {
            let text = self.palette.query.text().trim().to_owned();
            self.close_palette();
            return self.command(&arg.command, &text);
        }
        // A space makes the query a whole line: the word before it has to name a command, and what follows is that
        // command's argument. A bare word is still just a filter, and the row under the cursor is the one that runs.
        if let Some((word, rest)) = self.palette.query.text().split_once(char::is_whitespace) {
            let name = self.command_rows().into_iter().find(|r| r.name.to_lowercase() == word.to_lowercase()).map(|r| r.name);
            if let Some(name) = name {
                let arg = rest.trim().to_owned();
                self.close_palette();
                return self.command(&name, &arg);
            }
        }
        let rows = self.command_rows();
        let Some(row) = rows.get(self.palette.cursor).cloned() else { return Vec::new() };
        if row.usage.is_empty() {
            self.close_palette();
            return self.command(&row.name, "");
        }
        self.palette.arg = Some(PaletteArg { command: row.name, usage: row.usage });
        self.palette.query.clear();
        self.palette.cursor = 0;
        Vec::new()
    }
}
