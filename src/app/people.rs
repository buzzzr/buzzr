//! Channel membership, direct messages, invitations and the people directory.
//!
//! A second `impl App` block: the state lives on `App` in `app.rs`, and what is here is only the behaviour that reads and
//! changes it, so `app.rs` stays the one place to see what `App` holds. Private methods are `pub(super)` so `app.rs` and the
//! other feature modules can call them.

use super::*;

impl App {
    /// The status-line question asked before a message that mentions people who are not in the channel.
    pub fn invite_prompt(&self) -> Option<String> {
        let p = self.confirm_invite.as_ref()?;
        let who: Vec<&str> = p.people.iter().map(|(_, n)| n.as_str()).collect();
        Some(format!("Invite {} to {}? y invites and sends, n just sends, Esc cancels", who.join(", "), p.label))
    }

    /// The answer to the invite prompt: `y` invites the people it names and sends the message, `n` (or any other key)
    /// sends it without inviting them, and Esc keeps it in the input.
    pub(super) fn answer_invite(&mut self, k: KeyEvent) -> Vec<Cmd> {
        let Some(p) = self.confirm_invite.take() else { return Vec::new() };
        let names: Vec<&str> = p.people.iter().map(|(_, n)| n.as_str()).collect();
        let names = names.join(", ");
        match k.code {
            KeyCode::Esc => {
                self.editor.insert_str(&p.text);
                self.set_focus(Focus::Input);
                self.flash("not sent");
                Vec::new()
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                // The invited people are named in the message too, so they are told about it as members would be.
                let mut mentions = p.mentions.clone();
                mentions.extend(p.people.iter().map(|(pk, _)| pk.clone()));
                mentions.sort();
                mentions.dedup();
                self.stick_bottom = true;
                self.content_changed();
                let mut cmds = vec![Cmd::Send { channel: p.channel.clone(), text: p.text, root: p.root, mentions }];
                cmds.extend(p.people.iter().map(|(pk, _)| Cmd::Invite { channel: p.channel.clone(), pubkey: pk.clone(), dm: p.dm }));
                self.flash(format!("inviting {names} to {}", p.label));
                cmds
            }
            _ => {
                self.stick_bottom = true;
                self.content_changed();
                self.flash(format!("sent without inviting {names}"));
                vec![Cmd::Send { channel: p.channel, text: p.text, root: p.root, mentions: p.mentions }]
            }
        }
    }

    /// `new [private] name [description]`
    pub(super) fn create_channel(&mut self, arg: &str) -> Vec<Cmd> {
        if self.read_only {
            self.flash("read-only mode: nothing is sent");
            return Vec::new();
        }
        let mut rest = arg.trim();
        let mut private = false;
        for (word, is_private) in [("private ", true), ("open ", false)] {
            if let Some(r) = rest.strip_prefix(word) {
                rest = r.trim_start();
                private = is_private;
            }
        }
        let (name, about) = rest.split_once(char::is_whitespace).map(|(n, a)| (n.trim(), a.trim())).unwrap_or((rest, ""));
        if name.is_empty() {
            self.flash("new takes [private] name [description]");
            return Vec::new();
        }
        self.flash(format!("creating #{name}…"));
        vec![Cmd::CreateChannel { name: name.to_owned(), private, about: (!about.is_empty()).then(|| about.to_owned()) }]
    }

    // ---- joining and leaving channels

    /// Enter on the Join button: join the channel on screen. The relay re-publishes the group's state when membership changes,
    /// so the reloaded roster turns the button back into the composer.
    pub(super) fn join_open_channel(&mut self) -> Vec<Cmd> {
        let Some(index) = self.cur else { return Vec::new() };
        self.join_channel(index)
    }

    /// Ask to join an open channel and open it when the answer comes back: the relay re-publishes the group's state when
    /// membership changes, so the reloaded list and roster have us in it. A private channel is refused here — the relay would
    /// refuse too, and there is nothing the user could type to change that.
    pub(super) fn join_channel(&mut self, index: usize) -> Vec<Cmd> {
        if self.read_only {
            self.flash("read-only mode: nothing is sent");
            return Vec::new();
        }
        let Some(c) = self.channels.get(index) else { return Vec::new() };
        let (id, label) = (c.id.clone(), c.label.clone());
        if c.is_private && !self.is_member(&id) {
            self.flash(format!("{label} is private, so it needs an invitation"));
            return Vec::new();
        }
        self.open_after = Some(id.clone());
        self.flash(format!("joining {label}…"));
        vec![Cmd::JoinChannel { channel: id }]
    }

    /// `join NAME`: the channel `NAME` names. One we are already in is simply opened; an open one we are not in is joined.
    /// A leading `#` and the case are ignored, and a channel id works too.
    pub(super) fn join_command(&mut self, arg: &str) -> Vec<Cmd> {
        let who = arg.trim();
        let q = who.trim_start_matches('#').trim().to_lowercase();
        if q.is_empty() {
            self.flash("join needs NAME (a channel name, or its id) — Ctrl+F finds them");
            return Vec::new();
        }
        let found = self.channel_matching(who, &q);
        match found.len() {
            0 => {
                self.flash(format!("no channel here is called {who} — Ctrl+F lists the ones the relay shows"));
                Vec::new()
            }
            1 => {
                let index = found[0];
                if self.is_member(&self.channels[index].id) {
                    self.open_from_list(index)
                } else {
                    self.join_channel(index)
                }
            }
            n => {
                let names: Vec<String> = found.iter().take(4).map(|i| format!("#{}", self.channels[*i].label)).collect();
                self.flash(format!("{who} could be any of {n}: {} — type more of the name", names.join(", ")));
                Vec::new()
            }
        }
    }

    /// The channels `who` names: its id, else an exact name, else the names it is part of. DMs are left out: they are opened
    /// with `dm`, not joined.
    pub(super) fn channel_matching(&self, who: &str, q: &str) -> Vec<usize> {
        if let Some(i) = self.channels.iter().position(|c| c.id == who) {
            return vec![i];
        }
        let (mut exact, mut partial) = (Vec::new(), Vec::new());
        for (i, c) in self.channels.iter().enumerate() {
            if c.is_dm() {
                continue;
            }
            let label = c.label.to_lowercase();
            if label == q {
                exact.push(i);
            } else if label.contains(q) {
                partial.push(i);
            }
        }
        if exact.is_empty() {
            partial
        } else {
            exact
        }
    }

    /// `leave`: leaving changes membership on the relay and takes the channel out of the list, so it asks first (`y`).
    pub(super) fn ask_leave(&mut self) -> Vec<Cmd> {
        if self.read_only {
            self.flash("read-only mode: nothing is sent");
            return Vec::new();
        }
        let Some(c) = self.channel() else {
            self.flash("leave takes the channel you are in (open it first)");
            return Vec::new();
        };
        let (id, label) = (c.id.clone(), c.label.clone());
        if c.is_dm() {
            self.flash("a DM is not a channel to leave");
            return Vec::new();
        }
        self.confirm_leave = Some(id);
        self.flash(format!("leave {label}? y confirms, any other key cancels"));
        Vec::new()
    }

    /// The yes/no after `leave`: `y` leaves, anything else cancels.
    pub(super) fn answer_leave(&mut self, k: KeyEvent) -> Vec<Cmd> {
        let Some(id) = self.confirm_leave.take() else { return Vec::new() };
        if !matches!(k.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
            self.flash("not leaving");
            return Vec::new();
        }
        let label = self.channels.iter().find(|c| c.id == id).map(|c| c.label.clone()).unwrap_or_default();
        self.flash(format!("leaving {label}…"));
        vec![Cmd::LeaveChannel { channel: id }]
    }

    /// The channel list and the roster both change when membership does: the relay re-publishes a group's state after a join or
    /// a leave, so both are asked for again.
    pub(super) fn reload_membership(&mut self) -> Vec<Cmd> {
        let channels: Vec<String> = self.channels.iter().map(|c| c.id.clone()).collect();
        let mut cmds = vec![Cmd::LoadChannels];
        if !channels.is_empty() {
            cmds.push(Cmd::LoadRoster { channels });
        }
        cmds
    }

    /// Forget what a channel we have left held locally: the membership mark, the count, the members (they are who may be
    /// mentioned in it) and the unread badge. The read position stays, so rejoining picks up where we left off.
    pub(super) fn forget_channel(&mut self, id: &str) -> Option<Cmd> {
        self.joined.remove(id);
        self.member_counts.remove(id);
        self.members.remove(id);
        self.unread.remove(id);
        self.archived.remove(id).then(|| Cmd::SaveArchived(self.archived.clone()))
    }

    // ---- people and DMs

    /// Everyone whose display name contains `q`, exact matches first. Names come from the relay's `kind:0` profiles, so this
    /// finds the people seen in the channels we have loaded (members and authors), not the whole community.
    pub(super) fn people_matching(&self, q: &str) -> Vec<String> {
        let needle = q.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let (mut exact, mut partial) = (Vec::new(), Vec::new());
        for (pk, name) in &self.names {
            if *pk == self.me {
                continue;
            }
            let n = name.to_lowercase();
            if n == needle {
                exact.push(pk.clone());
            } else if n.contains(&needle) {
                partial.push(pk.clone());
            }
        }
        exact.sort();
        partial.sort();
        exact.extend(partial);
        exact
    }

    /// A name, or an npub/hex key, to the person it names: `Several` carries the question to ask back, and `Unknown` means
    /// nobody we have met has the name — the relay's directory is the next place to look (`ask_directory`).
    pub(super) fn who_is(&self, who: &str) -> Who {
        let who = person_name(who);
        if let Some(pk) = relay::as_pubkey(who) {
            return Who::One(pk);
        }
        let found = self.people_matching(who);
        match found.len() {
            0 => Who::Unknown,
            1 => Who::One(found[0].clone()),
            n => {
                let names: Vec<String> = found.iter().take(4).map(|pk| format!("{} ({})", self.name_of(pk), short_key(pk))).collect();
                Who::Several(format!("{who} could be any of {n}: {} — use more of the name, or an npub", names.join(", ")))
            }
        }
    }

    /// A name nothing local matches: ask the relay's directory who it is, and continue `then` with the answer
    /// (`Cmd::FindPeople`). The question replaces any earlier one — only the newest name is waited on.
    pub(super) fn ask_directory(&mut self, who: &str, then: Then) -> Vec<Cmd> {
        let who = person_name(who).to_owned();
        self.flash(format!("looking for {who}…"));
        self.lookup = Some(Lookup { who: who.clone(), then });
        vec![Cmd::FindPeople { query: who }]
    }

    /// The directory's answer: one person continues the command that asked, several are listed rather than guessed, and
    /// none is reported the way a name nobody has is. An answer for a name we are no longer asking about is dropped, and
    /// one that failed to arrive says so rather than leaving the command waiting forever.
    pub(super) fn find_people(&mut self, query: String, result: Result<Vec<(String, String)>, String>) -> Vec<Cmd> {
        let Some(l) = self.lookup.take() else { return Vec::new() };
        if !l.who.eq_ignore_ascii_case(&query) {
            self.lookup = Some(l); // a newer name is still waiting for its own answer
            return Vec::new();
        }
        let people = match result {
            Ok(p) => p,
            Err(e) => {
                self.flash(format!("could not look up {}: {e}", l.who));
                return Vec::new();
            }
        };
        match people.len() {
            0 => {
                self.flash(format!("nobody here is called {} — use a name from a channel, or an npub", l.who));
                Vec::new()
            }
            1 => {
                let Some((pk, name)) = people.into_iter().next() else { return Vec::new() };
                // The name is ours now: the command that asked goes on with the key the directory named, and the card and
                // the roster read the name from the same table. Acting on the key rather than re-resolving the name keeps
                // a name only the directory knows from being looked up a second time — and would otherwise never finish,
                // since the resolver skips this identity itself.
                self.names.insert(pk.clone(), name);
                self.content_changed();
                match l.then {
                    Then::Invite => self.invite_person(&pk),
                    Then::Whois => self.show_whois_person(&pk),
                    Then::Dm => self.open_dm(&pk),
                }
            }
            n => {
                let list: Vec<String> = people.iter().take(4).map(|(pk, name)| format!("{name} ({})", short_key(pk))).collect();
                self.flash(format!("{} could be any of {n}: {} — use more of the name, or an npub", l.who, list.join(", ")));
                Vec::new()
            }
        }
    }

    /// The DM channel whose only other member is `peer`, once we have loaded its members.
    pub(super) fn dm_with(&self, peer: &str) -> Option<usize> {
        let me = self.me.as_str();
        self.channels.iter().position(|c| {
            if !c.is_dm() {
                return false;
            }
            let Some(members) = self.members.get(&c.id) else { return false };
            let others: Vec<&str> = members.iter().map(|(p, _)| p.as_str()).filter(|p| *p != me).collect();
            others == [peer]
        })
    }

    /// `dm NAME`, and Enter on a person in the search: open the DM that is already there, else ask the relay for one.
    pub(super) fn open_dm(&mut self, peer: &str) -> Vec<Cmd> {
        if let Some(i) = self.dm_with(peer) {
            let cmds = self.open_channel(i);
            self.set_focus(Focus::Input);
            return cmds;
        }
        if self.read_only {
            self.flash("read-only mode: nothing is sent");
            return Vec::new();
        }
        // No DM with them in the list: the relay has one or will make one, and either way the channel shows up in the reload.
        self.flash(format!("opening a DM with {}…", self.name_of(peer)));
        self.pending_dm = Some(peer.to_owned());
        vec![Cmd::OpenDm { peer: peer.to_owned() }]
    }

    /// A DM was opened on the relay, or a DM's members arrived: open the channel as soon as the list has it. Until then the
    /// request stays pending, so a channel list that lags behind (the relay's caches, or a slow member list) still ends up on
    /// screen rather than being silently forgotten.
    pub(super) fn open_pending_dm(&mut self) -> Vec<Cmd> {
        let Some(peer) = self.pending_dm.clone() else { return Vec::new() };
        let Some(i) = self.dm_with(&peer) else { return Vec::new() };
        self.pending_dm = None;
        let cmds = self.open_channel(i);
        self.set_focus(Focus::Input);
        cmds
    }

    /// `invite NAME`: add someone to the channel on screen (or to the group DM), by name or npub. The relay takes a
    /// NIP-29 add-member for a channel and a different event for a DM (see `relay::invite_builder`).
    pub(super) fn invite(&mut self, who: &str) -> Vec<Cmd> {
        if who.trim().is_empty() {
            self.flash("invite needs NAME (a name, or an npub)");
            return Vec::new();
        }
        if self.read_only {
            self.flash("read-only mode: nothing is sent");
            return Vec::new();
        }
        match self.who_is(who) {
            Who::One(pk) => self.invite_person(&pk),
            Who::Several(why) => {
                self.flash(why);
                Vec::new()
            }
            Who::Unknown => self.ask_directory(who, Then::Invite),
        }
    }

    /// `/invite` once the person is a known key — the local name table, an npub, or the relay's directory all arrive
    /// here — to the channel on screen (or the group DM you are in). A NIP-29 add-member for a channel and a different
    /// event for a DM (see `relay::invite_builder`).
    pub(super) fn invite_person(&mut self, pubkey: &str) -> Vec<Cmd> {
        let Some(c) = self.channel() else {
            self.flash("pick a channel first");
            return Vec::new();
        };
        let (channel, dm, label) = (c.id.clone(), c.is_dm(), format!("{}{}", c.marker(), c.label));
        if pubkey == self.me {
            self.flash("you are already here");
            return Vec::new();
        }
        let name = self.name_of(pubkey);
        if self.members.get(&channel).map(|m| m.iter().any(|(p, _)| p == pubkey)).unwrap_or(false) {
            self.flash(format!("{name} is already in {label}"));
            return Vec::new();
        }
        self.flash(format!("inviting {name} to {label}…"));
        vec![Cmd::Invite { channel, pubkey: pubkey.to_owned(), dm }]
    }

    /// An invite came back: on success the channel's member list is stale, so drop it and ask for the fresh one (the next
    /// mention in that channel then knows the person is a member). A failure is reported, never swallowed.
    pub(super) fn invited(&mut self, channel: String, pubkey: String, result: Result<(), String>) -> Vec<Cmd> {
        match result {
            Ok(()) => {
                let name = self.name_of(&pubkey);
                self.members.remove(&channel);
                self.flash(format!("invited {name}"));
                if self.channel().map(|c| c.id == channel).unwrap_or(false) {
                    vec![Cmd::LoadMembers { channel }]
                } else {
                    Vec::new()
                }
            }
            Err(e) => {
                self.flash(format!("could not invite: {e}"));
                Vec::new()
            }
        }
    }

    /// `whois NAME`: the profile card, filled with the name we already have and the about line from the relay.
    pub(super) fn show_whois(&mut self, who: &str) -> Vec<Cmd> {
        if who.trim().is_empty() {
            self.flash("whois needs NAME (a name, or an npub)");
            return Vec::new();
        }
        match self.who_is(who) {
            Who::One(pk) => self.show_whois_person(&pk),
            Who::Several(why) => {
                self.flash(why);
                Vec::new()
            }
            Who::Unknown => self.ask_directory(who, Then::Whois),
        }
    }

    /// `/whois` once the person is a known key: the card, filled with the name we already have and the about line from
    /// the relay.
    pub(super) fn show_whois_person(&mut self, pk: &str) -> Vec<Cmd> {
        self.whois = Some(Whois {
            name: self.names.get(pk).cloned().unwrap_or_default(),
            npub: relay::npub_of(pk).unwrap_or_else(|| pk.to_owned()),
            pubkey: pk.to_owned(),
            about: None,
            loading: true,
        });
        self.overlay = Overlay::Whois;
        vec![Cmd::LoadProfile(pk.to_owned())]
    }
}
