//! The application: what is on screen, how keys and mouse clicks change it, and which network requests it wants.
//!
//! `App::handle` takes an event and returns the requests (`Cmd`) the caller should run; it never touches the network or the
//! terminal itself, so the whole behaviour is unit tested with synthetic events.

use crate::config::ReadState;
use crate::contrib::{self, PanelRow, PluginCmd, PluginEvent, PluginMeta, PluginState, Projection, Registry};
use crate::editor::Editor;
use crate::list::{self, Layout, ListState, Rendered};
use crate::live::Live;
use crate::markdown::{self, Line};
use crate::model::{Channel, Ev, Section, Store, MESSAGE_KINDS};
use crate::relay::{self, Profile, Window};
use crate::update::InstallSource;
use base64::Engine;
use chrono::TimeZone;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

const FLASH: Duration = Duration::from_secs(5);
const TYPING_FOR: Duration = Duration::from_secs(7);
const CHANNEL_REFRESH: Duration = Duration::from_secs(60);

/// One row of the channel list on screen: a section title or a channel (an index into `App::channels`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SideRow {
    Title(Section),
    Channel(usize),
}

fn sort_channel_list(list: &mut [Channel]) {
    list.sort_by(|a, b| (a.section, a.label.to_lowercase(), &a.id).cmp(&(b.section, b.label.to_lowercase(), &b.id)));
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Channels,
    Messages,
    Input,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum View {
    Channel,
    Thread(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Overlay {
    None,
    Keys,
    Help,
    Search,
    /// Somebody's profile card (`/whois`).
    Whois,
    /// A plugin panel (or the built-in plugin list), drawn from `App::panel`.
    Panel,
}

/// Somebody's profile card: what `/whois NAME` shows. The name and the about line come back from the relay a moment later, so
/// the card opens at once with what is already known and fills in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Whois {
    pub pubkey: String,
    pub name: String,
    pub npub: String,
    pub about: Option<String>,
    pub loading: bool,
}

/// `npub1abcd…wxyz` for a pubkey, or its first characters when it is not a hex key we can encode.
fn short_key(pubkey: &str) -> String {
    match relay::npub_of(pubkey) {
        Some(n) if n.len() > 16 => format!("{}…{}", &n[..12], &n[n.len() - 4..]),
        Some(n) => n,
        None => pubkey.chars().take(8).collect(),
    }
}

/// An open panel: rows and a cursor owned by the host, `Enter` sends the row's action to its plugin.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PanelState {
    /// Empty for the built-in `/plugins` list, which has no plugin behind it.
    pub plugin: String,
    pub id: String,
    pub title: String,
    pub rows: Vec<PanelRow>,
    pub cursor: usize,
}

/// Universal search: channels by name (local) and messages (full-text, from the relay).
pub struct Search {
    pub query: Editor,
    pub messages: Vec<Ev>,
    pub cursor: usize,
    pub seq: u64,
    pub asked: String,
    pub edited_at: Instant,
    pub loading: bool,
}

/// One row of the search results.
pub enum Row {
    Channel(usize),
    /// Somebody we know a name for; Enter opens (or starts) the DM with them.
    Person(String),
    Message(Ev),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Conn {
    Connecting,
    Live,
    Down(String),
}

/// Work for the caller to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Cmd {
    LoadChannels,
    LoadWindow { channel: String, cursor: Option<(u64, String)> },
    /// Count the messages that arrived in a channel after a stored read position, for the unread badge at startup.
    LoadUnread { channel: String, since: u64 },
    LoadThread { channel: String, root: String },
    LoadMembers { channel: String },
    LoadNames(Vec<String>),
    Send { channel: String, text: String, root: Option<String>, mentions: Vec<String> },
    Subscribe(Vec<String>),
    OpenUrl(String),
    /// "I am typing" (ephemeral), in a channel or a thread.
    Typing { channel: String, root: Option<String> },
    Edit { channel: String, id: String, text: String },
    Delete { channel: String, id: String },
    CreateChannel { name: String, private: bool, about: Option<String> },
    /// Ask the relay for the direct message with this person (`kind:41010`); it creates it when it does not exist yet.
    OpenDm { peer: String },
    /// One person's profile card (`kind:0`).
    LoadProfile(String),
    Search { query: String, seq: u64 },
    /// Write the last-read positions (channels and threads) to disk (the loop owns the file).
    SaveRead(ReadState),
    /// Work for the plugin runtime (start, reload, or send a command to a plugin process).
    Plugin(PluginCmd),
}

pub enum AppEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Tick,
    Live(Live),
    Channels(Result<Vec<Channel>, String>),
    Window { channel: String, cursor: Option<(u64, String)>, result: Result<Window, String> },
    Thread { channel: String, root: String, result: Result<Vec<Ev>, String> },
    Members { channel: String, result: Result<Vec<(String, String)>, String> },
    Names(Result<HashMap<String, String>, String>),
    Sent(Result<String, String>),
    /// The id of the channel we asked to create.
    Created(Result<String, String>),
    /// The relay took (or found) the direct message we asked for; the channel list is reloaded to find its id.
    DmOpened(Result<(), String>),
    /// The answer to `Cmd::LoadProfile`.
    Profile { pubkey: String, result: Result<Profile, String> },
    Search { seq: u64, result: Result<Vec<Ev>, String> },
    /// How many unread messages a channel had when we started (see `Cmd::LoadUnread`).
    Unread { channel: String, count: u32 },
    /// From the plugin runtime: ready/status/panel/flash/draft/send/gone, and the plugin list.
    Plugin(PluginEvent),
    /// The release feed found a later version than the one running.
    Update(String),
}

/// Screen rectangles from the last draw, for mouse hit-testing.
#[derive(Clone, Copy, Default, Debug)]
pub struct Areas {
    pub channels: Rect,
    pub msgs: Rect,
    pub input: Rect,
}

pub struct App {
    pub me: String,
    pub read_only: bool,
    pub tz: chrono_tz::Tz,
    pub names: HashMap<String, String>,
    pub channels: Vec<Channel>,
    pub chan_cursor: usize,
    pub cur: Option<usize>,
    pub stores: HashMap<String, Store>,
    pub members: HashMap<String, Vec<(String, String)>>,
    pub view: View,
    pub focus: Focus,
    pub overlay: Overlay,
    pub state: ListState,
    saved: HashMap<(String, Option<String>), ListState>,
    pub editor: Editor,
    pub reply_target: Option<String>,
    pub conn: Conn,
    pub unread: HashMap<String, u32>,
    /// The last-read position per channel: the `created_at` we have read through, loaded from disk at startup.
    pub read: HashMap<String, u64>,
    /// The last-read position per thread, keyed by the id of the message the thread starts from: the `created_at` of the newest
    /// reply we have seen in it. A thread with replies newer than this is marked unread in the message list. A thread never
    /// opened has no entry, and is not marked (unread starts at zero, as it does for a channel).
    pub thread_read: HashMap<String, u64>,
    /// Where each channel's last visit stopped, for the "new messages" line: taken from `read` as a channel is opened and kept
    /// while we stay in it, so reading on does not move the line under the reader.
    new_since: HashMap<String, u64>,
    pub typing: HashMap<(String, String), Instant>,
    flash: Option<(String, Instant)>,
    pub areas: Areas,
    pub view_h: usize,
    layout: Option<(usize, Layout)>,
    fresh_view: bool,
    stick_bottom: bool,
    loading: HashSet<String>,
    asked_names: HashSet<String>,
    /// Channels we have already asked the relay for a startup unread count; asked once per run.
    asked_unread: HashSet<String>,
    ever_up: bool,
    last_channel_refresh: Instant,
    pub osc: Vec<String>,
    pub quit: bool,
    pub tick_count: u64,
    pub last_live: Option<Instant>,
    /// Tell others when we are typing.
    pub send_typing: bool,
    /// Whether the terminal confirmed it reports Shift+Enter separately from Enter (the kitty keyboard
    /// protocol): the loop asks the terminal once at startup and fills this in, tests set it directly.
    /// Terminals without it send the same bytes for Shift+Enter and Enter, so there is nothing to bind;
    /// the footer then names Alt+Enter, which every terminal reports. See `README.md`, "Writing".
    pub keyboard_enhanced: bool,
    typing_sent: Option<(Instant, (String, Option<String>))>,
    /// The message being edited in the input box, and a message waiting for a yes/no before it is deleted.
    pub editing: Option<String>,
    pub confirm_delete: Option<String>,
    pub search: Search,
    /// Somebody's profile card, while `Overlay::Whois` is up.
    pub whois: Option<Whois>,
    /// The person a fresh DM was asked for: the channel is opened as soon as the reloaded list has it.
    pending_dm: Option<String>,
    /// What loaded plugins put on the screen: status items, badges, marks and `/commands`.
    pub plugins: Registry,
    /// How this copy of buzzr was installed, for `/update`; the loop fills it in, tests set it directly.
    pub update_source: InstallSource,
    /// A newer release the feed named, if any; the loop fills it in, tests set it directly.
    pub latest: Option<String>,
    /// Every plugin found on disk and where it is in its life; shown by `/plugins`.
    pub plugin_meta: Vec<PluginMeta>,
    /// The open panel, if any; `Overlay::Panel` is set while it is up.
    pub panel: Option<PanelState>,
    open_after: Option<String>,
    pending_select: Option<String>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The `─── new messages ───` rule drawn above the first message that arrived after the stored read position.
fn new_messages_divider(width: usize) -> Line {
    let label = " new messages ";
    let dashes = width.saturating_sub(label.chars().count()).max(4);
    let left = dashes / 2;
    let text = format!("{}{}{}", "─".repeat(left), label, "─".repeat(dashes - left));
    Line { spans: vec![Span::styled(text, Style::default().fg(crate::theme::get().warn))], links: Vec::new() }
}

impl App {
    pub fn new(me: String, read_only: bool, tz: chrono_tz::Tz) -> App {
        App {
            me,
            read_only,
            tz,
            names: HashMap::new(),
            channels: Vec::new(),
            chan_cursor: 0,
            cur: None,
            stores: HashMap::new(),
            members: HashMap::new(),
            view: View::Channel,
            focus: Focus::Channels,
            overlay: Overlay::None,
            state: ListState::default(),
            saved: HashMap::new(),
            editor: Editor::default(),
            reply_target: None,
            conn: Conn::Connecting,
            unread: HashMap::new(),
            read: HashMap::new(),
            thread_read: HashMap::new(),
            new_since: HashMap::new(),
            typing: HashMap::new(),
            flash: None,
            areas: Areas::default(),
            view_h: 20,
            layout: None,
            fresh_view: true,
            stick_bottom: false,
            loading: HashSet::new(),
            asked_names: HashSet::new(),
            asked_unread: HashSet::new(),
            ever_up: false,
            last_channel_refresh: Instant::now(),
            osc: Vec::new(),
            quit: false,
            tick_count: 0,
            last_live: None,
            send_typing: true,
            keyboard_enhanced: false,
            typing_sent: None,
            editing: None,
            confirm_delete: None,
            search: Search { query: Editor::default(), messages: Vec::new(), cursor: 0, seq: 0, asked: String::new(), edited_at: Instant::now(), loading: false },
            whois: None,
            pending_dm: None,
            plugins: Registry::default(),
            update_source: InstallSource::Other,
            latest: None,
            plugin_meta: Vec::new(),
            panel: None,
            open_after: None,
            pending_select: None,
        }
    }

    pub fn start(&mut self) -> Vec<Cmd> {
        vec![Cmd::LoadChannels]
    }

    // ---- small queries ---------------------------------------------------------------------------------------------------

    pub fn name_of(&self, pubkey: &str) -> String {
        self.names.get(pubkey).cloned().unwrap_or_else(|| pubkey.chars().take(8).collect())
    }

    pub fn channel(&self) -> Option<&Channel> {
        self.cur.and_then(|i| self.channels.get(i))
    }

    pub fn store(&self) -> Option<&Store> {
        self.channel().and_then(|c| self.stores.get(&c.id))
    }

    pub fn in_thread(&self) -> bool {
        matches!(self.view, View::Thread(_))
    }

    /// The channel row the selection bar is on: the keyboard cursor while the channel list has focus, the channel on
    /// screen otherwise. So the bar moves with the cursor and comes back to the open channel on leaving the list
    /// without opening another one (see `set_focus`).
    pub fn channel_bar(&self) -> Option<usize> {
        if self.focus == Focus::Channels {
            Some(self.chan_cursor)
        } else {
            self.cur
        }
    }

    /// The channel list as drawn: `channels` is sorted by section, so a title goes in front of the first channel of each
    /// non-empty section. Titles are not selectable: the cursor and `open_channel` only deal in channel indices.
    pub fn sidebar_rows(&self) -> Vec<SideRow> {
        let mut rows = Vec::with_capacity(self.channels.len() + 4);
        let mut last = None;
        for (i, c) in self.channels.iter().enumerate() {
            if last != Some(c.section) {
                rows.push(SideRow::Title(c.section));
                last = Some(c.section);
            }
            rows.push(SideRow::Channel(i));
        }
        rows
    }

    /// The first row of the channel list shown in `height` rows: the list scrolls just far enough to keep the selection
    /// bar in view. The drawing and mouse clicks both use this, so a click lands on the row that was drawn.
    pub fn sidebar_scroll(&self, rows: &[SideRow], height: usize) -> usize {
        let Some(bar) = self.channel_bar() else { return 0 };
        let at = rows.iter().position(|r| *r == SideRow::Channel(bar)).unwrap_or(0);
        (at + 1).saturating_sub(height)
    }

    /// Move the focus. A channel cursor only means something while the list has focus: entering the list starts the
    /// bar on the channel on screen, and leaving it without opening one brings the bar back to where it was.
    fn set_focus(&mut self, f: Focus) {
        if f != self.focus && (self.focus == Focus::Channels || f == Focus::Channels) {
            self.chan_cursor = self.cur.unwrap_or(0);
        }
        self.focus = f;
    }

    pub fn flash_text(&self) -> Option<&str> {
        self.flash.as_ref().filter(|(_, t)| t.elapsed() < FLASH).map(|(s, _)| s.as_str())
    }

    fn flash(&mut self, text: impl Into<String>) {
        self.flash = Some((text.into(), Instant::now()));
    }

    /// The one line the status bar keeps up while a newer release is known, or nothing. `/update` then says how to get it.
    pub fn update_hint(&self) -> Option<String> {
        self.latest.as_ref().map(|v| format!("{} {v} available · /update", env!("CARGO_PKG_NAME")))
    }

    /// Names of people typing in the open channel right now.
    pub fn typing_now(&self) -> Vec<String> {
        let Some(c) = self.channel() else { return Vec::new() };
        let mut v: Vec<(&String, &Instant)> = self.typing.iter().filter(|((ch, _), t)| ch == &c.id && t.elapsed() < TYPING_FOR).map(|((_, pk), t)| (pk, t)).collect();
        v.sort_by_key(|(pk, _)| pk.to_string());
        v.into_iter().map(|(pk, _)| self.name_of(pk)).collect()
    }

    pub fn total_unread(&self) -> u32 {
        self.unread.values().sum()
    }

    // ---- read state ------------------------------------------------------------------------------------------------------

    /// Take the last-read positions loaded from disk; the loop fills this in before the first event, tests set it directly.
    pub fn seed_read(&mut self, state: ReadState) {
        self.read = state.channels;
        self.thread_read = state.threads;
    }

    /// The whole read state (channels and threads), for `Cmd::SaveRead`.
    fn read_state(&self) -> ReadState {
        ReadState { channels: self.read.clone(), threads: self.thread_read.clone() }
    }

    /// Record that a channel has been read through its newest loaded message, and return the command that writes the new
    /// position to disk when it moved. The unread badge clears either way: whatever we counted, we are looking at the channel.
    fn read_through(&mut self, channel: &str) -> Option<Cmd> {
        self.unread.remove(channel);
        let newest = self.stores.get(channel).map(|s| s.latest).unwrap_or(0);
        if newest <= self.read.get(channel).copied().unwrap_or(0) {
            return None;
        }
        self.read.insert(channel.to_owned(), newest);
        Some(Cmd::SaveRead(self.read_state()))
    }

    /// Record that a thread has been read through its newest reply, and return the command that writes the new position to disk
    /// when it moved. The newest reply is the newest one loaded, or what the relay's summary says when the replies are not
    /// loaded; a thread with no replies yet has nothing to record.
    fn read_through_thread(&mut self, channel: &str, root: &str) -> Option<Cmd> {
        let newest = {
            let store = self.stores.get(channel)?;
            let loaded = store.thread(root).iter().filter(|m| m.id != root).map(|m| m.created_at).max();
            loaded.max(store.summaries.get(root).and_then(|s| s.last_reply_at))
        }?;
        if newest <= self.thread_read.get(root).copied().unwrap_or(0) {
            return None;
        }
        self.thread_read.insert(root.to_owned(), newest);
        Some(Cmd::SaveRead(self.read_state()))
    }

    /// The first message on screen that arrived after the last visit stopped, so the view can draw the divider above it.
    fn first_unread(&self) -> Option<usize> {
        if self.in_thread() {
            return None;
        }
        let frontier = *self.new_since.get(&self.channel()?.id)?;
        self.shown().iter().position(|m| m.pubkey != self.me && m.created_at > frontier)
    }

    /// Every thread root a channel's store knows about: what the relay told us about in a summary, plus any reply we have
    /// loaded. Used to mark them all read at once.
    fn thread_roots(store: &Store) -> HashSet<String> {
        let mut roots: HashSet<String> = store.summaries.keys().cloned().collect();
        for m in store.msgs.values() {
            if m.parent().is_some() {
                roots.insert(store.thread_root(m).to_owned());
            }
        }
        roots
    }

    /// Mark every thread in a channel (or in every channel, when `channel` is `None`) read as of `at`: `/read` clears the thread
    /// marks too, so the message list and the channel badge agree. Returns whether anything moved.
    fn mark_threads_read(&mut self, channel: Option<&str>, at: u64) -> bool {
        let ids: Vec<String> = match channel {
            Some(id) => vec![id.to_owned()],
            None => self.stores.keys().cloned().collect(),
        };
        let mut changed = false;
        for id in ids {
            let roots: Vec<String> = self.stores.get(&id).map(|s| Self::thread_roots(s).into_iter().collect()).unwrap_or_default();
            for root in roots {
                let entry = self.thread_read.entry(root).or_insert(0);
                if *entry < at {
                    *entry = at;
                    changed = true;
                }
            }
        }
        changed
    }

    /// `/read` marks the channel on screen read; `/read all` marks every channel read as of now.
    fn mark_read_command(&mut self, arg: &str) -> Vec<Cmd> {
        let mut cmds = Vec::new();
        if arg.eq_ignore_ascii_case("all") {
            let now = now_secs();
            let ids: Vec<String> = self.channels.iter().map(|c| c.id.clone()).collect();
            for id in ids {
                let at = self.read.get(&id).copied().unwrap_or(0).max(now);
                self.read.insert(id, at);
            }
            self.mark_threads_read(None, now);
            self.unread.clear();
            self.new_since.clear();
            self.content_changed();
            self.flash("all channels marked read");
            cmds.push(Cmd::SaveRead(self.read_state()));
            return cmds;
        }
        let Some(id) = self.channel().map(|c| c.id.clone()) else {
            self.flash("no channel is open");
            return cmds;
        };
        // The threads in this channel are read too: the marks in the message list go with the badge.
        let threads_moved = self.mark_threads_read(Some(&id), now_secs());
        match self.read_through(&id) {
            Some(cmd) => {
                self.new_since.remove(&id);
                self.content_changed();
                self.flash("marked read");
                cmds.push(cmd);
            }
            None => {
                self.unread.remove(&id);
                self.new_since.remove(&id);
                self.content_changed();
                if threads_moved {
                    self.flash("marked read");
                    cmds.push(Cmd::SaveRead(self.read_state()));
                } else {
                    self.flash("already read");
                }
            }
        }
        cmds
    }

    /// `u` on the message list marks the channel unread from the selected message: the badge comes back and the `new messages`
    /// line is drawn above it. The earlier position is written, so the mark survives a restart.
    fn mark_unread_from_selected(&mut self) -> Vec<Cmd> {
        let Some(m) = self.selected_msg() else {
            self.flash("select a message first (Up/Down)");
            return Vec::new();
        };
        let Some(id) = self.channel().map(|c| c.id.clone()) else { return Vec::new() };
        // The position just before the message, so the message itself and everything after it count as unread. The relay's
        // resolution is seconds, so a message sharing this timestamp with an earlier one takes that one with it.
        let at = m.created_at.saturating_sub(1);
        let count = self
            .stores
            .get(&id)
            .map(|s| s.timeline().iter().filter(|e| e.pubkey != self.me && e.created_at > at).count() as u32)
            .unwrap_or(0);
        if count == 0 {
            self.flash("nothing after that message to mark unread");
            return Vec::new();
        }
        self.read.insert(id.clone(), at);
        self.new_since.insert(id.clone(), at);
        self.unread.insert(id, count);
        self.content_changed();
        let n = self.selected_number().unwrap_or(0);
        self.flash(format!("marked unread from #{n}"));
        vec![Cmd::SaveRead(self.read_state())]
    }

    // ---- layout ----------------------------------------------------------------------------------------------------------

    /// The messages the list shows, oldest first.
    fn shown<'a>(&'a self) -> Vec<&'a Ev> {
        match (self.store(), &self.view) {
            (Some(s), View::Channel) => s.timeline(),
            (Some(s), View::Thread(root)) => s.thread(root),
            _ => Vec::new(),
        }
    }

    #[cfg(test)]
    pub fn shown_ids(&self) -> Vec<String> {
        self.shown().iter().map(|m| m.id.clone()).collect()
    }

    fn fmt_time(&self, at: u64) -> String {
        let t = self.tz.timestamp_opt(at as i64, 0).single();
        let Some(t) = t else { return String::new() };
        let age = now_secs().saturating_sub(at);
        if age < 6 * 86400 {
            t.format("%a %H:%M").to_string()
        } else {
            t.format("%b %-d %H:%M").to_string()
        }
    }

    /// The badge the message list puts on a thread row when replies arrived after the last visit to that thread: `● 2 new`
    /// when the replies are loaded and can be counted, `● new` when only the relay's summary says so (they are not loaded yet).
    /// `None` when the thread is read, or has never been opened: with no stored position nothing is marked, as for a channel.
    fn thread_unread_badge(&self, store: &Store, root: &str) -> Option<Span<'static>> {
        let at = *self.thread_read.get(root)?;
        let loaded = store.thread(root).iter().filter(|m| m.id != root && m.pubkey != self.me && m.created_at > at).count();
        let newer = store.summaries.get(root).and_then(|s| s.last_reply_at).map(|t| t > at).unwrap_or(false);
        let text = match (loaded, newer) {
            (0, false) => return None,
            (0, true) => "  ● new".to_owned(),
            (n, _) => format!("  ● {n} new"),
        };
        Some(Span::styled(text, Style::default().fg(crate::theme::get().warn).add_modifier(Modifier::BOLD)))
    }

    fn render_message(&self, store: &Store, m: &Ev, number: usize, width: usize) -> Rendered {
        let th = crate::theme::get();
        let dim = Style::default().fg(th.dim);
        let time = Style::default().fg(th.time);
        let mine = m.pubkey == self.me;
        let (content, edited) = store.content_of(m);
        let mut head: Vec<Span<'static>> = vec![
            Span::styled(format!("#{number} "), dim),
            Span::styled(format!("{} ", self.fmt_time(m.created_at)), time),
            Span::styled(self.name_of(&m.pubkey), Style::default().fg(if mine { crate::theme::get().ok } else { crate::theme::get().accent }).add_modifier(Modifier::BOLD)),
        ];
        if edited {
            head.push(Span::styled(" (edited)", dim));
        }
        if let Some(mark) = self.plugins.message_mark(&m.id) {
            head.insert(0, Span::styled(format!("{} ", mark.text), mark.tone.style()));
        }
        if !self.in_thread() {
            if m.parent().is_some() {
                head.push(Span::styled("  ↳ reply to an older message", dim));
            }
            let (count, last) = store.replies_of(m);
            if count > 0 {
                head.push(Span::styled(format!("  💬 {} {}", count, if count == 1 { "reply" } else { "replies" }), Style::default().fg(crate::theme::get().warn)));
                if let Some(at) = last {
                    head.push(Span::styled(format!(" · last {}", self.fmt_time(at)), dim));
                }
                if let Some(badge) = self.thread_unread_badge(store, &m.id) {
                    head.push(badge);
                }
            }
        }
        // on a narrow screen drop the optional parts (last-reply time, reply count, ...) rather than clip the line mid-word
        while head.iter().map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref())).sum::<usize>() > width + 1 && head.len() > 3 {
            head.pop();
        }
        let mut lines = vec![Line { spans: head, links: Vec::new() }];
        lines.extend(markdown::render(content, width));
        let reactions = store.reaction_summary(&m.id);
        if !reactions.is_empty() {
            let text = reactions.iter().map(|(e, n)| if *n > 1 { format!("{e} {n}") } else { e.clone() }).collect::<Vec<_>>().join("  ");
            lines.push(Line { spans: vec![Span::styled(text, Style::default().fg(crate::theme::get().warn))], links: Vec::new() });
        }
        Rendered { id: m.id.clone(), lines }
    }

    fn build_layout(&self, width: usize) -> Layout {
        let Some(store) = self.store() else { return Layout::default() };
        let first_unread = self.first_unread();
        let items = self
            .shown()
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let mut rendered = self.render_message(store, m, i + 1, width);
                if Some(i) == first_unread {
                    rendered.lines.insert(0, new_messages_divider(width));
                }
                rendered
            })
            .collect();
        Layout::new(items)
    }

    pub fn layout_ref(&self) -> Option<&(usize, Layout)> {
        self.layout.as_ref()
    }

    /// The layout for a content width, rebuilt when the content or the width changed. Rebuilding keeps the reader's place.
    pub fn layout(&mut self, width: usize) -> &Layout {
        let stale = self.layout.as_ref().map(|(w, _)| *w != width).unwrap_or(true);
        if stale {
            self.relayout(width, |_| {});
        }
        &self.layout.as_ref().expect("layout was just built").1
    }

    /// Rebuild the layout after `change`, keeping the same message at the same place on screen, following the bottom if the
    /// reader was there.
    fn relayout(&mut self, width: usize, change: impl FnOnce(&mut App)) {
        let before = self.layout.as_ref().map(|(_, l)| (list::capture(l, &self.state, self.view_h), l.items.is_empty()));
        change(self);
        let new = self.build_layout(width);
        let fresh = std::mem::take(&mut self.fresh_view);
        match before {
            Some((anchor, was_empty)) if !fresh && !was_empty => list::restore(&new, &mut self.state, self.view_h, &anchor, true),
            _ => {
                // first layout of a view: a saved position if there is one, else the bottom
                if fresh && self.state.selected.is_some() && !new.items.is_empty() {
                    list::ensure_selected_visible(&new, &mut self.state, self.view_h);
                } else {
                    self.state.scroll = new.max_scroll(self.view_h);
                }
            }
        }
        if self.stick_bottom {
            self.state.scroll = new.max_scroll(self.view_h);
            if let Some(last) = new.items.last() {
                self.state.selected = Some(last.id.clone());
            }
            self.stick_bottom = false;
        }
        if self.state.selected.is_none() || new.index_of(self.state.selected.as_deref().unwrap_or("")).is_none() {
            self.state.selected = new.items.last().map(|r| r.id.clone());
        }
        if let Some(id) = self.pending_select.clone() {
            if new.index_of(&id).is_some() {
                self.state.selected = Some(id);
                list::ensure_selected_visible(&new, &mut self.state, self.view_h);
                self.pending_select = None;
            }
        }
        self.layout = Some((width, new));
    }

    /// Mark the content as changed: the layout is rebuilt (at the same width) keeping the reader's place.
    fn content_changed(&mut self) {
        match self.layout.as_ref().map(|(w, _)| *w) {
            Some(w) => self.relayout(w, |_| {}),
            None => self.layout = None,
        }
    }

    fn msg_width(&self) -> usize {
        self.layout.as_ref().map(|(w, _)| *w).unwrap_or(78)
    }

    fn selected_msg(&self) -> Option<Ev> {
        let (_, layout) = self.layout.as_ref()?;
        let i = list::selected_index(layout, &self.state)?;
        let id = &layout.items[i].id;
        self.store()?.msgs.get(id).cloned()
    }

    /// Whether the selected message is one the user may edit or delete.
    pub fn selected_is_own(&self) -> bool {
        self.selected_msg().map(|m| m.pubkey == self.me && m.kind == 9).unwrap_or(false)
    }

    fn selected_number(&self) -> Option<usize> {
        let (_, layout) = self.layout.as_ref()?;
        list::selected_index(layout, &self.state).map(|i| i + 1)
    }

    fn message_number(&self, n: usize) -> Option<Ev> {
        let (_, layout) = self.layout.as_ref()?;
        let id = &layout.items.get(n.checked_sub(1)?)?.id;
        self.store()?.msgs.get(id).cloned()
    }

    // ---- views -----------------------------------------------------------------------------------------------------------

    fn view_key(&self) -> Option<(String, Option<String>)> {
        let c = self.channel()?;
        Some((c.id.clone(), match &self.view {
            View::Channel => None,
            View::Thread(r) => Some(r.clone()),
        }))
    }

    fn save_position(&mut self) {
        if let Some(k) = self.view_key() {
            self.saved.insert(k, self.state.clone());
        }
    }

    fn enter_view(&mut self, view: View) {
        self.save_position();
        self.view = view;
        self.reply_target = None;
        self.layout = None;
        self.fresh_view = true;
        self.state = self.view_key().and_then(|k| self.saved.get(&k).cloned()).unwrap_or_default();
    }

    fn open_channel(&mut self, index: usize) -> Vec<Cmd> {
        if index >= self.channels.len() {
            return Vec::new();
        }
        self.save_position();
        self.cur = Some(index);
        self.chan_cursor = index;
        self.view = View::Channel;
        self.reply_target = None;
        self.layout = None;
        self.fresh_view = true;
        self.state = self.view_key().and_then(|k| self.saved.get(&k).cloned()).unwrap_or_default();
        let id = self.channels[index].id.clone();
        // The "new messages" line stays where the last visit left the channel, so it is taken before reading on moves the
        // stored position past the very messages it should mark. A channel never read has no line to draw.
        match self.read.get(&id).copied() {
            Some(at) => {
                self.new_since.insert(id.clone(), at);
            }
            None => {
                self.new_since.remove(&id);
            }
        }
        let mut cmds = Vec::new();
        // opening a channel reads it: whatever we counted is now on screen
        if let Some(cmd) = self.read_through(&id) {
            cmds.push(cmd);
        }
        let store = self.stores.entry(id.clone()).or_default();
        if !store.head_loaded && self.loading.insert(format!("head:{id}")) {
            cmds.push(Cmd::LoadWindow { channel: id.clone(), cursor: None });
        }
        if !self.members.contains_key(&id) && self.loading.insert(format!("members:{id}")) {
            cmds.push(Cmd::LoadMembers { channel: id });
        }
        cmds
    }

    fn open_thread(&mut self, root: String) -> Vec<Cmd> {
        let Some(c) = self.channel() else { return Vec::new() };
        let channel = c.id.clone();
        self.enter_view(View::Thread(root.clone()));
        self.set_focus(Focus::Messages);
        let mut cmds = Vec::new();
        // opening a thread reads it: the replies are on screen, so its row in the channel stops being marked unread
        if let Some(cmd) = self.read_through_thread(&channel, &root) {
            cmds.push(cmd);
        }
        cmds.push(Cmd::LoadThread { channel, root });
        cmds
    }

    fn back_to_channel(&mut self) {
        if self.in_thread() {
            self.enter_view(View::Channel);
        }
    }

    fn load_older(&mut self) -> Vec<Cmd> {
        let Some(c) = self.channel() else { return Vec::new() };
        if self.in_thread() {
            self.flash("older messages are only available in a channel view");
            return Vec::new();
        }
        let id = c.id.clone();
        let store = self.stores.entry(id.clone()).or_default();
        if !store.has_more {
            self.flash("no older messages");
            return Vec::new();
        }
        let Some(cursor) = store.next_cursor.clone() else { return Vec::new() };
        if !self.loading.insert(format!("older:{id}")) {
            return Vec::new();
        }
        vec![Cmd::LoadWindow { channel: id, cursor: Some(cursor) }]
    }

    fn refresh(&mut self) -> Vec<Cmd> {
        let Some(c) = self.channel() else { return vec![Cmd::LoadChannels] };
        let channel = c.id.clone();
        let mut cmds = vec![Cmd::LoadWindow { channel: channel.clone(), cursor: None }];
        if let View::Thread(root) = &self.view {
            cmds.push(Cmd::LoadThread { channel, root: root.clone() });
        }
        cmds
    }

    // ---- events ----------------------------------------------------------------------------------------------------------

    pub fn handle(&mut self, ev: AppEvent) -> Vec<Cmd> {
        match ev {
            AppEvent::Key(k) if k.kind != KeyEventKind::Release => self.key(k),
            AppEvent::Key(_) => Vec::new(),
            AppEvent::Mouse(m) => self.mouse(m),
            AppEvent::Paste(s) => {
                if self.overlay == Overlay::Search {
                    self.search.query.insert_str(&s.replace('\n', " "));
                    self.search.edited_at = Instant::now();
                    return Vec::new();
                }
                self.set_focus(Focus::Input);
                self.editor.insert_str(&s);
                self.maybe_typing()
            }
            AppEvent::Tick => self.tick(),
            AppEvent::Live(l) => self.live(l),
            AppEvent::Channels(r) => self.channels_loaded(r),
            AppEvent::Window { channel, cursor, result } => self.window_loaded(channel, cursor, result),
            AppEvent::Thread { channel, root, result } => self.thread_loaded(channel, root, result),
            AppEvent::Members { channel, result } => {
                self.loading.remove(&format!("members:{channel}"));
                if let Ok(m) = result {
                    let unknown: Vec<String> = m.iter().map(|(p, _)| p.clone()).collect();
                    self.members.insert(channel, m);
                    let mut cmds = self.want_names(unknown);
                    cmds.extend(self.open_pending_dm());
                    return cmds;
                }
                Vec::new()
            }
            AppEvent::Names(r) => {
                match r {
                    Ok(map) => {
                        self.names.extend(map);
                        self.relabel_dms();
                        self.content_changed();
                    }
                    Err(e) => self.flash(format!("could not load names: {e}")),
                }
                Vec::new()
            }
            AppEvent::Sent(r) => {
                match r {
                    Ok(_) => {}
                    Err(e) => self.flash(format!("failed: {e}")),
                }
                Vec::new()
            }
            AppEvent::Created(r) => match r {
                Ok(id) => {
                    self.open_after = Some(id);
                    self.flash("channel created");
                    vec![Cmd::LoadChannels]
                }
                Err(e) => {
                    self.flash(format!("could not create the channel: {e}"));
                    Vec::new()
                }
            },
            AppEvent::DmOpened(Ok(())) => vec![Cmd::LoadChannels],
            AppEvent::DmOpened(Err(e)) => {
                self.pending_dm = None;
                self.flash(format!("could not open the DM: {e}"));
                Vec::new()
            }
            AppEvent::Profile { pubkey, result } => {
                // An answer for a card that has since been replaced by another person is dropped.
                if self.whois.as_ref().map(|w| w.pubkey == pubkey).unwrap_or(false) {
                    match result {
                        Ok(p) => {
                            if let Some(w) = self.whois.as_mut() {
                                if !p.name.is_empty() {
                                    w.name = p.name;
                                }
                                w.about = Some(p.about);
                                w.loading = false;
                            }
                        }
                        Err(e) => {
                            if let Some(w) = self.whois.as_mut() {
                                w.loading = false;
                            }
                            self.flash(format!("could not load the profile: {e}"));
                        }
                    }
                }
                Vec::new()
            }
            AppEvent::Search { seq, result } => self.search_results(seq, result),
            AppEvent::Unread { channel, count } => self.unread_loaded(channel, count),
            AppEvent::Plugin(e) => self.plugin_event(e),
            AppEvent::Update(v) => {
                self.latest = Some(v);
                Vec::new()
            }
        }
    }

    fn tick(&mut self) -> Vec<Cmd> {
        self.tick_count += 1;
        self.typing.retain(|_, t| t.elapsed() < TYPING_FOR);
        if self.last_channel_refresh.elapsed() > CHANNEL_REFRESH {
            self.last_channel_refresh = Instant::now();
            return vec![Cmd::LoadChannels];
        }
        if self.overlay == Overlay::Search {
            let q = self.search.query.text();
            if q.trim().chars().count() >= 2 && q != self.search.asked && self.search.edited_at.elapsed() >= Duration::from_millis(350) {
                return self.run_search();
            }
        }
        Vec::new()
    }

    fn want_names(&mut self, pubkeys: Vec<String>) -> Vec<Cmd> {
        let fresh: Vec<String> = pubkeys.into_iter().filter(|p| !self.names.contains_key(p) && self.asked_names.insert(p.clone())).collect();
        if fresh.is_empty() {
            Vec::new()
        } else {
            vec![Cmd::LoadNames(fresh)]
        }
    }

    fn relabel_dms(&mut self) {
        let me = self.me.clone();
        for i in 0..self.channels.len() {
            if !self.channels[i].is_dm() {
                continue;
            }
            let others: Vec<String> = self.members.get(&self.channels[i].id).map(|m| m.iter().filter(|(p, _)| *p != me).map(|(p, _)| self.name_of(p)).collect()).unwrap_or_default();
            if !others.is_empty() {
                self.channels[i].label = format!("✉ {}", others.join(", "));
            }
        }
        self.sort_channels();
    }

    /// Sections first, then by label. Run after every label change: DMs all arrive named "DM" and only get their
    /// "✉ name" label later, so sorting once on arrival would leave them out of order. The open channel and the keyboard
    /// cursor follow their channel, not their index.
    fn sort_channels(&mut self) {
        let open_id = self.channel().map(|c| c.id.clone());
        let cursor_id = self.channels.get(self.chan_cursor).map(|c| c.id.clone());
        sort_channel_list(&mut self.channels);
        self.cur = open_id.and_then(|id| self.channels.iter().position(|c| c.id == id));
        if let Some(id) = cursor_id {
            self.chan_cursor = self.channels.iter().position(|c| c.id == id).unwrap_or(0);
        }
    }

    /// A startup unread count for a channel. The open channel is read as we look at it, so its count is dropped; elsewhere the
    /// count stands unless live messages have already raised the badge higher.
    fn unread_loaded(&mut self, channel: String, count: u32) -> Vec<Cmd> {
        let current = self.channel().map(|c| c.id == channel).unwrap_or(false);
        if !current && count > 0 {
            let entry = self.unread.entry(channel).or_insert(0);
            *entry = (*entry).max(count);
        }
        Vec::new()
    }

    fn channels_loaded(&mut self, r: Result<Vec<Channel>, String>) -> Vec<Cmd> {
        let mut list = match r {
            Ok(l) => l,
            Err(e) => {
                self.flash(format!("could not load channels: {e}"));
                return Vec::new();
            }
        };
        let open_id = self.channel().map(|c| c.id.clone());
        let old_ids: Vec<String> = self.channels.iter().map(|c| c.id.clone()).collect();
        // keep labels we already resolved
        for c in &mut list {
            if let Some(old) = self.channels.iter().find(|o| o.id == c.id) {
                c.label = old.label.clone();
            }
        }
        sort_channel_list(&mut list);
        let cursor_id = self.channels.get(self.chan_cursor).map(|c| c.id.clone());
        self.channels = list;
        self.cur = open_id.and_then(|id| self.channels.iter().position(|c| c.id == id));
        self.chan_cursor = cursor_id.and_then(|id| self.channels.iter().position(|c| c.id == id)).unwrap_or(0).min(self.channels.len().saturating_sub(1));
        let ids: Vec<String> = self.channels.iter().map(|c| c.id.clone()).collect();
        let mut cmds = Vec::new();
        if ids != old_ids {
            cmds.push(Cmd::Subscribe(ids.clone()));
        }
        for c in self.channels.clone() {
            if c.is_dm() && !self.members.contains_key(&c.id) && self.loading.insert(format!("members:{}", c.id)) {
                cmds.push(Cmd::LoadMembers { channel: c.id });
            }
        }
        self.relabel_dms();
        if let Some(id) = self.open_after.clone() {
            if let Some(i) = self.channels.iter().position(|c| c.id == id) {
                self.open_after = None;
                cmds.extend(self.open_channel(i));
                self.set_focus(Focus::Input);
            }
        }
        if self.cur.is_none() && !self.channels.is_empty() {
            let first = self.channels.iter().position(|c| !c.is_dm()).unwrap_or(0);
            cmds.extend(self.open_channel(first));
            // the first channel is opened so its messages are on screen, but the keyboard stays on the channel list:
            // that is where a fresh start is expected to be, and Escape comes back here from the message list.
            self.set_focus(Focus::Channels);
        }
        // The stored read position says where each channel was left; ask the relay how much arrived since, once per run. The
        // open channel's own position was advanced by opening it, so only the others are asked: its count would be thrown away.
        for c in self.channels.clone() {
            if self.channel().map(|open| open.id == c.id).unwrap_or(false) {
                continue;
            }
            if let Some(at) = self.read.get(&c.id).copied() {
                if self.asked_unread.insert(c.id.clone()) {
                    cmds.push(Cmd::LoadUnread { channel: c.id.clone(), since: at.saturating_add(1) });
                }
            }
        }
        cmds.extend(self.open_pending_dm());
        cmds
    }

    fn apply_events(&mut self, channel: &str, events: &[Ev]) -> Vec<Cmd> {
        let store = self.stores.entry(channel.to_owned()).or_default();
        for e in events {
            store.apply(e);
        }
        let authors: Vec<String> = events.iter().map(|e| e.pubkey.clone()).collect::<HashSet<_>>().into_iter().collect();
        self.want_names(authors)
    }

    fn window_loaded(&mut self, channel: String, cursor: Option<(u64, String)>, result: Result<Window, String>) -> Vec<Cmd> {
        self.loading.remove(&format!("{}:{channel}", if cursor.is_some() { "older" } else { "head" }));
        let w = match result {
            Ok(w) => w,
            Err(e) => {
                self.flash(format!("could not load messages: {e}"));
                return Vec::new();
            }
        };
        let current = self.channel().map(|c| c.id == channel).unwrap_or(false);
        let mut cmds = Vec::new();
        let changed = |app: &mut App, cmds: &mut Vec<Cmd>| {
            let store = app.stores.entry(channel.clone()).or_default();
            let paged = cursor.is_some();
            if paged || !store.head_loaded || store.next_cursor.is_none() {
                store.next_cursor = w.next_cursor.clone();
                store.has_more = w.has_more;
            }
            store.head_loaded = true;
            let _ = paged;
            cmds.extend(app.apply_events(&channel, &w.events));
        };
        if current {
            self.relayout(self.msg_width(), |app| {
                changed(app, &mut cmds);
                // the channel on screen is read as far as what just arrived, so its badge and divider are gone
                if cursor.is_none() {
                    if let Some(cmd) = app.read_through(&channel) {
                        cmds.push(cmd);
                    }
                }
            });
            if cursor.is_some() {
                self.flash("loaded older messages");
            }
        } else {
            changed(self, &mut cmds);
        }
        cmds
    }

    fn thread_loaded(&mut self, channel: String, root: String, result: Result<Vec<Ev>, String>) -> Vec<Cmd> {
        let evs = match result {
            Ok(e) => e,
            Err(e) => {
                self.flash(format!("could not load the thread: {e}"));
                return Vec::new();
            }
        };
        let visible = self.channel().map(|c| c.id == channel).unwrap_or(false);
        let mut cmds = Vec::new();
        if visible {
            self.relayout(self.msg_width(), |app| cmds.extend(app.apply_events(&channel, &evs)));
        } else {
            cmds.extend(self.apply_events(&channel, &evs));
        }
        // the thread we have open is read as far as what just arrived, so going back does not mark it unread
        if visible && matches!(&self.view, View::Thread(r) if r == &root) {
            if let Some(cmd) = self.read_through_thread(&channel, &root) {
                cmds.push(cmd);
            }
        }
        cmds
    }

    fn live(&mut self, l: Live) -> Vec<Cmd> {
        match l {
            Live::Up => {
                let reconnect = self.ever_up;
                self.ever_up = true;
                self.conn = Conn::Live;
                if reconnect {
                    // whatever happened while we were away is missing: reload the open view
                    self.flash("reconnected");
                    return self.refresh();
                }
                Vec::new()
            }
            Live::Down(why) => {
                self.conn = Conn::Down(why);
                Vec::new()
            }
            Live::Event(e) => {
                self.last_live = Some(Instant::now());
                self.live_event(e)
            }
        }
    }

    /// The channel an event belongs to: its `h` tag, else the channel whose store holds the message or reaction it points at.
    /// A reaction removal (`kind:5`) carries no `h` tag and points at the reaction event, so the reaction lookup matters here.
    fn route(&self, e: &Ev) -> Option<String> {
        if let Some(h) = e.channel() {
            return Some(h.to_owned());
        }
        let target = e.tag("e")?;
        self.stores.iter().find(|(_, s)| s.msgs.contains_key(target) || s.reaction_ids.contains_key(target)).map(|(id, _)| id.clone())
    }

    fn live_event(&mut self, e: Ev) -> Vec<Cmd> {
        let Some(channel) = self.route(&e) else { return Vec::new() };
        if e.kind == crate::model::KIND_TYPING {
            if e.pubkey != self.me {
                self.typing.insert((channel, e.pubkey.clone()), Instant::now());
            }
            return Vec::new();
        }
        let is_message = MESSAGE_KINDS.contains(&e.kind);
        if is_message {
            self.typing.remove(&(channel.clone(), e.pubkey.clone()));
            let current = self.channel().map(|c| c.id == channel).unwrap_or(false);
            // Only rows the channel timeline lists raise its badge; a thread reply is unread per thread (`thread_read`), not
            // per channel, and there is no channel position it could ever move to clear the badge.
            if !current && e.pubkey != self.me && e.is_top_level() {
                *self.unread.entry(channel.clone()).or_default() += 1;
            }
        }
        let current = self.channel().map(|c| c.id == channel).unwrap_or(false);
        let mut cmds = Vec::new();
        if current {
            self.relayout(self.msg_width(), |app| {
                cmds.extend(app.apply_events(&channel, std::slice::from_ref(&e)));
                // a message in the channel we are looking at is read as it appears
                if let Some(cmd) = app.read_through(&channel) {
                    cmds.push(cmd);
                }
            });
            // a reply in the thread we have open is read as it appears too; one in another thread stays marked unread
            if let View::Thread(root) = &self.view {
                let root = root.clone();
                if let Some(cmd) = self.read_through_thread(&channel, &root) {
                    cmds.push(cmd);
                }
            }
        } else {
            cmds.extend(self.apply_events(&channel, std::slice::from_ref(&e)));
        }
        cmds
    }

    // ---- keys ------------------------------------------------------------------------------------------------------------

    fn key(&mut self, k: KeyEvent) -> Vec<Cmd> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        // keys that work everywhere
        match k.code {
            KeyCode::Char('c') | KeyCode::Char('q') if ctrl => {
                self.quit = true;
                return Vec::new();
            }
            KeyCode::F(1) => {
                self.overlay = if self.overlay == Overlay::Keys { Overlay::None } else { Overlay::Keys };
                return Vec::new();
            }
            KeyCode::F(2) => {
                self.overlay = if self.overlay == Overlay::Help { Overlay::None } else { Overlay::Help };
                return Vec::new();
            }
            _ => {}
        }
        if self.confirm_delete.is_some() {
            return self.answer_delete(k);
        }
        if self.overlay == Overlay::Search {
            return self.key_search(k);
        }
        if self.overlay == Overlay::Panel {
            return self.key_panel(k);
        }
        if self.overlay != Overlay::None {
            // a panel is open: Esc (or Enter) closes it and nothing else happens
            if matches!(k.code, KeyCode::Esc | KeyCode::Enter) {
                self.overlay = Overlay::None;
            }
            return Vec::new();
        }
        match k.code {
            KeyCode::Tab => {
                let next = match self.focus {
                    Focus::Channels => Focus::Messages,
                    Focus::Messages => Focus::Input,
                    Focus::Input => Focus::Channels,
                };
                self.set_focus(next);
                return Vec::new();
            }
            KeyCode::BackTab => {
                let next = match self.focus {
                    Focus::Channels => Focus::Input,
                    Focus::Messages => Focus::Channels,
                    Focus::Input => Focus::Messages,
                };
                self.set_focus(next);
                return Vec::new();
            }
            KeyCode::Char('f') if ctrl => {
                self.open_search(String::new());
                return Vec::new();
            }
            KeyCode::Char('r') if ctrl => return self.refresh(),
            KeyCode::Char('o') if ctrl => return self.load_older(),
            _ => {}
        }
        match self.focus {
            Focus::Channels => self.key_channels(k),
            Focus::Messages => self.key_messages(k),
            Focus::Input => self.key_input(k, ctrl, alt, shift),
        }
    }

    fn key_channels(&mut self, k: KeyEvent) -> Vec<Cmd> {
        match k.code {
            KeyCode::Up => self.chan_cursor = self.chan_cursor.saturating_sub(1),
            KeyCode::Down => self.chan_cursor = (self.chan_cursor + 1).min(self.channels.len().saturating_sub(1)),
            KeyCode::Home => self.chan_cursor = 0,
            KeyCode::End => self.chan_cursor = self.channels.len().saturating_sub(1),
            KeyCode::Enter => {
                let cmds = self.open_channel(self.chan_cursor);
                self.set_focus(Focus::Messages);
                return cmds;
            }
            _ => {}
        }
        Vec::new()
    }

    fn key_messages(&mut self, k: KeyEvent) -> Vec<Cmd> {
        let view_h = self.view_h;
        let Some(layout) = self.layout.take() else { return Vec::new() };
        let (w, layout) = layout;
        let mut cmds = Vec::new();
        match k.code {
            KeyCode::Up => {
                let at_top = list::selected_index(&layout, &self.state).map(|i| i == 0).unwrap_or(true);
                if at_top {
                    self.layout = Some((w, layout));
                    return self.load_older();
                }
                list::move_selection(&layout, &mut self.state, view_h, -1);
            }
            KeyCode::Down => list::move_selection(&layout, &mut self.state, view_h, 1),
            KeyCode::PageUp => {
                list::scroll_by(&layout, &mut self.state, view_h, -(view_h as isize - 2));
                list::select_visible(&layout, &mut self.state, view_h);
            }
            KeyCode::PageDown => {
                list::scroll_by(&layout, &mut self.state, view_h, view_h as isize - 2);
                list::select_visible(&layout, &mut self.state, view_h);
            }
            KeyCode::Home => list::select(&layout, &mut self.state, view_h, 0),
            KeyCode::End => {
                list::select(&layout, &mut self.state, view_h, layout.items.len().saturating_sub(1));
                self.state.scroll = layout.max_scroll(view_h);
            }
            _ => {
                self.layout = Some((w, layout));
                return self.key_messages_action(k);
            }
        }
        self.layout = Some((w, layout));
        cmds.extend(Vec::new());
        cmds
    }

    fn key_messages_action(&mut self, k: KeyEvent) -> Vec<Cmd> {
        match k.code {
            KeyCode::Enter => match self.selected_msg() {
                Some(m) if !self.in_thread() => {
                    let root = self.store().map(|s| s.thread_root(&m).to_owned()).unwrap_or(m.id.clone());
                    self.open_thread(root)
                }
                Some(_) => {
                    self.set_focus(Focus::Input);
                    self.flash("you are in this thread: type a reply below, Esc goes back");
                    Vec::new()
                }
                None => Vec::new(),
            },
            KeyCode::Char('r') => {
                self.start_reply();
                Vec::new()
            }
            KeyCode::Char('e') => {
                if let Some(m) = self.selected_msg() {
                    self.start_edit(&m);
                } else {
                    self.flash("select a message first (Up/Down)");
                }
                Vec::new()
            }
            KeyCode::Char('d') => {
                if let Some(m) = self.selected_msg() {
                    self.ask_delete(&m);
                } else {
                    self.flash("select a message first (Up/Down)");
                }
                Vec::new()
            }
            KeyCode::Char('c') => {
                if let Some(m) = self.selected_msg() {
                    self.copy_message(&m);
                } else {
                    self.flash("select a message first (Up/Down)");
                }
                Vec::new()
            }
            KeyCode::Char('u') => self.mark_unread_from_selected(),
            KeyCode::Esc => {
                // Escape backs out one level: leave a thread first, otherwise there is nothing left to close, so
                // the keyboard goes back to the channel list.
                if self.in_thread() {
                    self.back_to_channel();
                } else {
                    self.set_focus(Focus::Channels);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn start_reply(&mut self) {
        let Some(m) = self.selected_msg() else {
            self.flash("select a message first (Up/Down)");
            return;
        };
        let root = self.store().map(|s| s.thread_root(&m).to_owned()).unwrap_or(m.id.clone());
        self.reply_target = Some(root);
        self.set_focus(Focus::Input);
        let n = self.selected_number().unwrap_or(0);
        self.flash(format!("replying to #{n}: type your reply and press Enter; Esc cancels"));
    }

    fn copy_message(&mut self, m: &Ev) {
        let text = self.store().map(|s| s.content_of(m).0.to_owned()).unwrap_or_else(|| m.content.clone());
        let n = text.chars().count();
        self.copy(&markdown::strip_controls(&text));
        self.flash(format!("copied the message ({n} characters) to the clipboard"));
    }

    /// Put text on the clipboard of the machine the terminal runs on (OSC 52; terminals that do not support it ignore it).
    pub fn copy(&mut self, text: &str) {
        let capped: String = text.chars().take(100_000).collect();
        let b64 = base64::engine::general_purpose::STANDARD.encode(capped.as_bytes());
        self.osc.push(format!("\x1b]52;c;{b64}\x07"));
    }

    pub fn follow_link(&mut self, url: &str) -> Vec<Cmd> {
        let lower = url.to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")) {
            self.flash("not following that link (only http, https and mailto)");
            return Vec::new();
        }
        let url = markdown::strip_controls(url);
        self.copy(&url);
        let remote = std::env::var("SSH_CONNECTION").is_ok() || std::env::var("SSH_TTY").is_ok();
        if remote {
            self.flash(format!("link copied to the clipboard, paste it into your browser: {url}"));
            Vec::new()
        } else {
            self.flash(format!("opened (and copied): {url}"));
            vec![Cmd::OpenUrl(url)]
        }
    }

    fn key_input(&mut self, k: KeyEvent, ctrl: bool, alt: bool, shift: bool) -> Vec<Cmd> {
        let width = self.areas.input.width.saturating_sub(4).max(10) as usize;
        match k.code {
            KeyCode::Enter if shift || alt => self.editor.insert_char('\n'),
            KeyCode::Char('j') if ctrl => self.editor.insert_char('\n'),
            KeyCode::Enter => return self.submit(),
            KeyCode::Esc => {
                if self.editing.take().is_some() {
                    self.editor.clear();
                    self.flash("edit cancelled");
                } else if self.reply_target.take().is_some() {
                    self.flash("reply cancelled");
                } else {
                    self.set_focus(Focus::Messages);
                }
            }
            KeyCode::Backspace if alt => {
                self.editor.kill_word_back();
                return self.maybe_typing();
            }
            KeyCode::Backspace => {
                self.editor.backspace();
                return self.maybe_typing();
            }
            KeyCode::Delete => {
                self.editor.delete();
                return self.maybe_typing();
            }
            KeyCode::Left if ctrl || alt => self.editor.word_left(),
            KeyCode::Right if ctrl || alt => self.editor.word_right(),
            KeyCode::Left => self.editor.left(),
            KeyCode::Right => self.editor.right(),
            KeyCode::Home => self.editor.home(),
            KeyCode::End => self.editor.end(),
            KeyCode::Up => {
                self.editor.up(width);
            }
            KeyCode::Down => {
                self.editor.down(width);
            }
            KeyCode::Char('a') if ctrl => self.editor.home(),
            KeyCode::Char('e') if ctrl => self.editor.end(),
            KeyCode::Char('w') if ctrl => self.editor.kill_word_back(),
            KeyCode::Char('u') if ctrl => self.editor.kill_to_start(),
            KeyCode::Char('k') if ctrl => self.editor.kill_to_end(),
            KeyCode::Char('b') if alt => self.editor.word_left(),
            KeyCode::Char('f') if alt => self.editor.word_right(),
            KeyCode::Char(c) if !ctrl && !alt => {
                self.editor.insert_char(c);
                return self.maybe_typing();
            }
            KeyCode::Tab => self.editor.insert_str("    "),
            _ => {}
        }
        Vec::new()
    }

    // ---- sending and commands --------------------------------------------------------------------------------------------

    fn submit(&mut self) -> Vec<Cmd> {
        let text = self.editor.text().trim().to_owned();
        if let Some(id) = self.editing.clone() {
            return self.finish_edit(id, text);
        }
        if text.is_empty() {
            return Vec::new();
        }
        if let Some(rest) = text.strip_prefix('/') {
            let (cmd, arg) = rest.split_once(char::is_whitespace).map(|(c, a)| (c, a.trim())).unwrap_or((rest, ""));
            self.editor.clear();
            return self.command(cmd, arg);
        }
        let root = self.reply_target.take().or_else(|| match &self.view {
            View::Thread(r) => Some(r.clone()),
            View::Channel => None,
        });
        self.editor.clear();
        self.typing_sent = None;
        self.send(text, root)
    }

    fn command(&mut self, cmd: &str, arg: &str) -> Vec<Cmd> {
        match cmd {
            "q" | "quit" => {
                self.quit = true;
                Vec::new()
            }
            "help" => {
                self.overlay = Overlay::Help;
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
            "search" => {
                self.open_search(arg.to_owned());
                if arg.chars().count() >= 2 {
                    return self.run_search();
                }
                Vec::new()
            }
            "new" => self.create_channel(arg),
            "dm" => {
                if arg.trim().is_empty() {
                    self.flash("usage: /dm NAME (a name from a channel, or an npub)");
                    return Vec::new();
                }
                match self.resolve_person(arg) {
                    Ok(pk) => self.open_dm(&pk),
                    Err(e) => {
                        self.flash(e);
                        Vec::new()
                    }
                }
            }
            "whois" => self.show_whois(arg),
            "edit" | "delete" => match arg.parse::<usize>().ok().and_then(|n| self.message_number(n)) {
                Some(m) if cmd == "edit" => {
                    self.start_edit(&m);
                    Vec::new()
                }
                Some(m) => {
                    self.ask_delete(&m);
                    Vec::new()
                }
                None => {
                    self.flash(format!("usage: /{cmd} N (a message number shown as #N)"));
                    Vec::new()
                }
            },
            "c" => {
                self.back_to_channel();
                Vec::new()
            }
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
                    self.flash(format!("usage: /{cmd} N (a message number shown as #N)"));
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
                        self.flash("usage: /r N text");
                        Vec::new()
                    }
                }
            }
            other => {
                if let Some((plugin, c)) = self.plugins.command(other) {
                    let (plugin, command) = (plugin.to_owned(), c.id.clone());
                    self.flash(format!("{plugin}: /{command}"));
                    return vec![Cmd::Plugin(PluginCmd::Invoke { plugin, command, args: arg.to_owned() })];
                }
                self.flash(format!("unknown command /{other} (try /help)"));
                Vec::new()
            }
        }
    }

    fn send(&mut self, text: String, root: Option<String>) -> Vec<Cmd> {
        if self.read_only {
            self.flash("read-only mode: nothing is sent");
            return Vec::new();
        }
        let Some(c) = self.channel() else {
            self.flash("pick a channel first");
            return Vec::new();
        };
        let channel = c.id.clone();
        let mentions = self.resolve_mentions(&channel, &text);
        self.stick_bottom = true;
        self.content_changed();
        vec![Cmd::Send { channel, text, root, mentions }]
    }

    /// `@name` in the text -> the pubkeys of channel members with that display name.
    fn resolve_mentions(&self, channel: &str, text: &str) -> Vec<String> {
        let lower = text.to_lowercase();
        let mut out = Vec::new();
        for (pk, _) in self.members.get(channel).map(Vec::as_slice).unwrap_or(&[]) {
            if *pk == self.me {
                continue;
            }
            let Some(name) = self.names.get(pk) else { continue };
            let needle = format!("@{}", name.to_lowercase());
            let mut from = 0;
            while let Some(i) = lower[from..].find(&needle) {
                let end = from + i + needle.len();
                if lower[end..].chars().next().map(|c| !c.is_alphanumeric() && c != '_').unwrap_or(true) {
                    out.push(pk.clone());
                    break;
                }
                from = end;
            }
        }
        out
    }

    // ---- mouse -----------------------------------------------------------------------------------------------------------

    fn mouse(&mut self, m: MouseEvent) -> Vec<Cmd> {
        if self.overlay != Overlay::None {
            if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
                self.overlay = Overlay::None;
            }
            return Vec::new();
        }
        let inside = |r: Rect| m.column >= r.x && m.column < r.x + r.width && m.row >= r.y && m.row < r.y + r.height;
        let (cols, rows) = (m.column, m.row);
        let _ = (cols, rows);
        let view_h = self.view_h;
        match m.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if inside(self.areas.msgs) => {
                let delta = if matches!(m.kind, MouseEventKind::ScrollUp) { -3 } else { 3 };
                if let Some((w, layout)) = self.layout.take() {
                    list::scroll_by(&layout, &mut self.state, view_h, delta);
                    self.layout = Some((w, layout));
                }
                Vec::new()
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if inside(self.areas.channels) => {
                if matches!(m.kind, MouseEventKind::ScrollUp) {
                    self.chan_cursor = self.chan_cursor.saturating_sub(1);
                } else {
                    self.chan_cursor = (self.chan_cursor + 1).min(self.channels.len().saturating_sub(1));
                }
                Vec::new()
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if inside(self.areas.channels) {
                    let rows = self.sidebar_rows();
                    let row = (m.row - self.areas.channels.y) as usize + self.sidebar_scroll(&rows, self.areas.channels.height as usize);
                    if let Some(SideRow::Channel(i)) = rows.get(row).copied() {
                        let cmds = self.open_channel(i);
                        self.set_focus(Focus::Messages);
                        return cmds;
                    }
                    Vec::new()
                } else if inside(self.areas.msgs) {
                    self.set_focus(Focus::Messages);
                    self.click_message(m.column - self.areas.msgs.x, m.row - self.areas.msgs.y)
                } else if inside(self.areas.input) {
                    self.set_focus(Focus::Input);
                    Vec::new()
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    /// A click at (`x`, `y`) inside the message area: follow a link under it, else select the message and open its thread.
    fn click_message(&mut self, x: u16, y: u16) -> Vec<Cmd> {
        let Some((w, layout)) = self.layout.take() else { return Vec::new() };
        let line = self.state.scroll + y as usize;
        let hit = layout.at_line(line);
        let mut cmds = Vec::new();
        let mut link = None;
        let mut open = None;
        if let Some((i, within)) = hit {
            list::select(&layout, &mut self.state, self.view_h, i);
            if let Some(l) = within {
                // the first column is the selection gutter
                link = layout.items[i].lines[l].link_at((x as usize).saturating_sub(1)).map(|h| h.url.clone());
            }
            if link.is_none() && !self.in_thread() {
                open = Some(layout.items[i].id.clone());
            }
        }
        self.layout = Some((w, layout));
        if let Some(url) = link {
            cmds.extend(self.follow_link(&url));
        } else if let Some(id) = open {
            let root = self.store().and_then(|s| s.msgs.get(&id).map(|m| s.thread_root(m).to_owned())).unwrap_or(id);
            cmds.extend(self.open_thread(root));
        }
        cmds
    }
}

// ---- typing, editing, deleting, creating channels, search ------------------------------------------------------------------------

impl App {
    /// Tell the others we are typing: at most once every three seconds per channel/thread, never for commands, edits or in
    /// read-only mode.
    fn maybe_typing(&mut self) -> Vec<Cmd> {
        if !self.send_typing || self.read_only || self.editing.is_some() || self.editor.is_empty() || self.editor.text().starts_with('/') {
            return Vec::new();
        }
        let Some(c) = self.channel() else { return Vec::new() };
        let channel = c.id.clone();
        let root = self.reply_target.clone().or_else(|| match &self.view {
            View::Thread(r) => Some(r.clone()),
            View::Channel => None,
        });
        let key = (channel.clone(), root.clone());
        if let Some((at, k)) = &self.typing_sent {
            if *k == key && at.elapsed() < Duration::from_secs(3) {
                return Vec::new();
            }
        }
        self.typing_sent = Some((Instant::now(), key));
        vec![Cmd::Typing { channel, root }]
    }

    fn own(&mut self, m: &Ev, what: &str) -> bool {
        if m.pubkey != self.me || m.kind != 9 {
            self.flash(format!("you can only {what} your own messages"));
            return false;
        }
        true
    }

    fn start_edit(&mut self, m: &Ev) {
        if self.read_only {
            self.flash("read-only mode: nothing is sent");
            return;
        }
        if !self.own(m, "edit") {
            return;
        }
        let text = self.store().map(|s| s.content_of(m).0.to_owned()).unwrap_or_else(|| m.content.clone());
        self.editor.clear();
        self.editor.insert_str(&text);
        self.editing = Some(m.id.clone());
        self.reply_target = None;
        self.set_focus(Focus::Input);
        self.flash("editing your message: Enter saves it, Esc cancels");
    }

    fn finish_edit(&mut self, id: String, text: String) -> Vec<Cmd> {
        let original = self.store().and_then(|s| s.msgs.get(&id).map(|m| s.content_of(m).0.to_owned()));
        if text.is_empty() {
            self.flash("an edited message cannot be empty (Esc cancels; d deletes it)");
            return Vec::new();
        }
        self.editing = None;
        self.editor.clear();
        if original.as_deref() == Some(text.as_str()) {
            self.flash("nothing changed");
            return Vec::new();
        }
        let Some(c) = self.channel() else { return Vec::new() };
        vec![Cmd::Edit { channel: c.id.clone(), id, text }]
    }

    fn ask_delete(&mut self, m: &Ev) {
        if self.read_only {
            self.flash("read-only mode: nothing is sent");
            return;
        }
        if self.own(m, "delete") {
            self.confirm_delete = Some(m.id.clone());
        }
    }

    /// The yes/no after `d`: `y` deletes, anything else cancels.
    fn answer_delete(&mut self, k: KeyEvent) -> Vec<Cmd> {
        let Some(id) = self.confirm_delete.take() else { return Vec::new() };
        if !matches!(k.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
            self.flash("not deleted");
            return Vec::new();
        }
        let Some(channel) = self.channel().map(|c| c.id.clone()) else { return Vec::new() };
        self.flash("deleting…");
        vec![Cmd::Delete { channel, id }]
    }

    /// `/new [private] name [description]`
    fn create_channel(&mut self, arg: &str) -> Vec<Cmd> {
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
            self.flash("usage: /new [private] name [description]");
            return Vec::new();
        }
        self.flash(format!("creating #{name}…"));
        vec![Cmd::CreateChannel { name: name.to_owned(), private, about: (!about.is_empty()).then(|| about.to_owned()) }]
    }

    // ---- people and DMs

    /// Everyone whose display name contains `q`, exact matches first. Names come from the relay's `kind:0` profiles, so this
    /// finds the people seen in the channels we have loaded (members and authors), not the whole community.
    fn people_matching(&self, q: &str) -> Vec<String> {
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

    /// A name, or an npub/hex key, to the one person it names: the reason when it is none or more than one.
    fn resolve_person(&self, who: &str) -> Result<String, String> {
        if let Some(pk) = relay::as_pubkey(who) {
            return Ok(pk);
        }
        let found = self.people_matching(who);
        match found.len() {
            0 => Err(format!("nobody here is called {who} — use a name from a channel, or an npub")),
            1 => Ok(found[0].clone()),
            n => {
                let names: Vec<String> = found.iter().take(4).map(|pk| format!("{} ({})", self.name_of(pk), short_key(pk))).collect();
                Err(format!("{who} could be any of {n}: {} — use more of the name, or an npub", names.join(", ")))
            }
        }
    }

    /// The DM channel whose only other member is `peer`, once we have loaded its members.
    fn dm_with(&self, peer: &str) -> Option<usize> {
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

    /// `/dm NAME`, and Enter on a person in the search: open the DM that is already there, else ask the relay for one.
    fn open_dm(&mut self, peer: &str) -> Vec<Cmd> {
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
    fn open_pending_dm(&mut self) -> Vec<Cmd> {
        let Some(peer) = self.pending_dm.clone() else { return Vec::new() };
        let Some(i) = self.dm_with(&peer) else { return Vec::new() };
        self.pending_dm = None;
        let cmds = self.open_channel(i);
        self.set_focus(Focus::Input);
        cmds
    }

    /// `/whois NAME`: the profile card, filled with the name we already have and the about line from the relay.
    fn show_whois(&mut self, who: &str) -> Vec<Cmd> {
        if who.trim().is_empty() {
            self.flash("usage: /whois NAME (a name from a channel, or an npub)");
            return Vec::new();
        }
        let pk = match self.resolve_person(who) {
            Ok(pk) => pk,
            Err(e) => {
                self.flash(e);
                return Vec::new();
            }
        };
        self.whois = Some(Whois {
            name: self.names.get(&pk).cloned().unwrap_or_default(),
            npub: relay::npub_of(&pk).unwrap_or_else(|| pk.clone()),
            pubkey: pk.clone(),
            about: None,
            loading: true,
        });
        self.overlay = Overlay::Whois;
        vec![Cmd::LoadProfile(pk)]
    }

    // ---- search

    pub fn open_search(&mut self, query: String) {
        self.overlay = Overlay::Search;
        self.search.query.clear();
        self.search.query.insert_str(&query);
        self.search.messages.clear();
        self.search.asked.clear();
        self.search.cursor = 0;
        self.search.loading = false;
        self.search.edited_at = Instant::now();
    }

    fn run_search(&mut self) -> Vec<Cmd> {
        let query = self.search.query.text();
        self.search.seq += 1;
        self.search.asked = query.clone();
        self.search.loading = true;
        vec![Cmd::Search { query: query.trim().to_owned(), seq: self.search.seq }]
    }

    fn search_results(&mut self, seq: u64, result: Result<Vec<Ev>, String>) -> Vec<Cmd> {
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

    fn key_search(&mut self, k: KeyEvent) -> Vec<Cmd> {
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

    fn edit_query(&mut self, f: impl FnOnce(&mut Editor)) {
        f(&mut self.search.query);
        self.search.edited_at = Instant::now();
        self.search.cursor = 0;
    }

    /// Open the selected result: a channel, or a message in its channel (inside its thread if it is a reply), with the message selected.
    fn activate_search(&mut self) -> Vec<Cmd> {
        let rows = self.search_rows();
        let Some(row) = rows.into_iter().nth(self.search.cursor) else { return Vec::new() };
        self.overlay = Overlay::None;
        match row {
            Row::Channel(i) => {
                let cmds = self.open_channel(i);
                self.set_focus(Focus::Messages);
                cmds
            }
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

// ---- plugins ------------------------------------------------------------------------------------------------------------

impl App {
    /// A plugin's display name for a message, falling back to its id when it is not registered.
    fn plugin_name(&self, id: &str) -> String {
        self.plugins.name_of(id).unwrap_or(id).to_owned()
    }

    fn plugin_event(&mut self, ev: PluginEvent) -> Vec<Cmd> {
        match ev {
            PluginEvent::Ready { id, name, contributions } => {
                self.plugins.set(&id, &name, contributions);
                self.content_changed();
                self.refresh_plugin_panel();
                Vec::new()
            }
            PluginEvent::Meta(meta) => {
                self.plugin_meta = meta;
                self.refresh_plugin_panel();
                Vec::new()
            }
            PluginEvent::Status { id, segments } => {
                // A status for a plugin that is not registered (gone, or not ready) is dropped: the plugin list says why.
                self.plugins.set_status(&id, segments);
                Vec::new()
            }
            PluginEvent::Panel { plugin, panel } => {
                self.open_panel(plugin, panel);
                Vec::new()
            }
            PluginEvent::Flash { text } => {
                self.flash(text);
                Vec::new()
            }
            PluginEvent::Draft { plugin, text } => {
                let name = self.plugin_name(&plugin);
                if self.read_only {
                    self.flash(format!("read-only mode: {name} cannot put a draft in the input"));
                } else if !self.editor.is_empty() || self.editing.is_some() {
                    self.flash(format!("{name} sent a draft, but you are already writing; it was dropped"));
                } else {
                    self.editor.insert_str(&text);
                    self.set_focus(Focus::Input);
                    self.flash(format!("{name} put a draft in the input: read it, then Enter sends it"));
                }
                Vec::new()
            }
            PluginEvent::Send { plugin, channel, text } => {
                let name = self.plugin_name(&plugin);
                if self.read_only {
                    self.flash("read-only mode: nothing is sent");
                    return Vec::new();
                }
                let Some(open) = self.channel().map(|c| c.id.clone()) else {
                    self.flash(format!("{name} wanted to send, but no channel is open"));
                    return Vec::new();
                };
                if channel.as_deref().map(|c| c != open).unwrap_or(false) {
                    self.flash(format!("{name} wanted to send in another channel; plugins may only write in the open one"));
                    return Vec::new();
                }
                self.flash(format!("a message from {name}"));
                self.send(text, None)
            }
            PluginEvent::Gone { id, why } => {
                self.plugins.remove(&id);
                self.content_changed();
                self.refresh_plugin_panel();
                self.flash(format!("plugin {id} stopped: {why}"));
                Vec::new()
            }
            PluginEvent::Log { id, level: _, text } => {
                self.flash(format!("plugin {id}: {text}"));
                Vec::new()
            }
        }
    }

    /// The `/plugins` overlay: what is on disk, what is running, what it may do, and the commands it added.
    fn show_plugins(&mut self) {
        let mut rows = Vec::new();
        if self.plugin_meta.is_empty() {
            rows.push(PanelRow { text: "No plugins found. Put a folder with a plugin.json in the plugins directory.".into(), tone: contrib::Tone::Dim, action: None });
        }
        for m in &self.plugin_meta {
            let tone = match m.state {
                PluginState::Running => contrib::Tone::Ok,
                PluginState::Failed => contrib::Tone::Error,
                PluginState::Disabled | PluginState::Discovered => contrib::Tone::Dim,
                PluginState::Starting => contrib::Tone::Warn,
            };
            let perms = if m.permissions.is_empty() { String::new() } else { format!(" · {}", m.permissions.join(", ")) };
            let detail = if m.detail.is_empty() { String::new() } else { format!(" · {}", m.detail) };
            rows.push(PanelRow {
                text: format!("{:<18} {:<9} {} command(s){perms}{detail}", m.id, m.state.label(), m.commands),
                tone,
                action: None,
            });
        }
        let commands = self.plugins.commands();
        if !commands.is_empty() {
            rows.push(PanelRow { text: String::new(), tone: contrib::Tone::Default, action: None });
            rows.push(PanelRow { text: "Commands plugins added".into(), tone: contrib::Tone::Bold, action: None });
            for (plugin, c) in commands {
                let usage = if c.usage.is_empty() { String::new() } else { format!("   {}", c.usage) };
                rows.push(PanelRow {
                    text: format!("/{:<14} {}{usage}   ({plugin})", c.id, if c.title == c.id { "" } else { &c.title }),
                    tone: contrib::Tone::Default,
                    action: None,
                });
            }
        }
        rows.push(PanelRow { text: String::new(), tone: contrib::Tone::Default, action: None });
        rows.push(PanelRow {
            text: "Put a folder with a plugin.json in the plugins directory; /plugins shows what loaded.".into(),
            tone: contrib::Tone::Dim,
            action: None,
        });
        self.panel = Some(PanelState { plugin: String::new(), id: "plugins".into(), title: "Plugins".into(), rows, cursor: 0 });
        self.overlay = Overlay::Panel;
    }

    /// Keep `/plugins` current while it is open.
    fn refresh_plugin_panel(&mut self) {
        let managed = self.panel.as_ref().map(|p| p.plugin.is_empty()).unwrap_or(false);
        if self.overlay != Overlay::Panel || !managed {
            return;
        }
        let cursor = self.panel.as_ref().map(|p| p.cursor).unwrap_or(0);
        self.show_plugins();
        if let Some(p) = self.panel.as_mut() {
            p.cursor = cursor.min(p.rows.len().saturating_sub(1));
        }
    }

    fn open_panel(&mut self, plugin: String, panel: contrib::Panel) {
        let title = if panel.title.is_empty() { plugin.clone() } else { panel.title };
        self.panel = Some(PanelState { plugin, id: panel.id, title, rows: panel.rows, cursor: 0 });
        self.overlay = Overlay::Panel;
        self.flash("panel: Up/Down to move, Enter runs the row, Esc closes");
    }

    fn key_panel(&mut self, k: KeyEvent) -> Vec<Cmd> {
        let n = self.panel.as_ref().map(|p| p.rows.len()).unwrap_or(0);
        match k.code {
            KeyCode::Esc => {
                self.overlay = Overlay::None;
                self.panel = None;
            }
            KeyCode::Up => self.panel_move(-1),
            KeyCode::Down => self.panel_move(1),
            KeyCode::PageUp => self.panel_move(-8),
            KeyCode::PageDown => self.panel_move(8),
            KeyCode::Home => {
                if let Some(p) = self.panel.as_mut() {
                    p.cursor = 0;
                }
            }
            KeyCode::End => {
                if let Some(p) = self.panel.as_mut() {
                    p.cursor = n.saturating_sub(1);
                }
            }
            KeyCode::Enter => {
                let Some(p) = self.panel.as_ref() else { return Vec::new() };
                if p.plugin.is_empty() {
                    return Vec::new(); // the built-in list has nothing to run
                }
                let Some(action) = p.rows.get(p.cursor).and_then(|r| r.action.clone()) else { return Vec::new() };
                return vec![Cmd::Plugin(PluginCmd::PanelAction {
                    plugin: p.plugin.clone(),
                    id: p.id.clone(),
                    row: p.cursor,
                    value: Some(action),
                })];
            }
            _ => {}
        }
        Vec::new()
    }

    fn panel_move(&mut self, delta: isize) {
        let Some(p) = self.panel.as_mut() else { return };
        let n = p.rows.len();
        p.cursor = if delta < 0 { p.cursor.saturating_sub((-delta) as usize) } else { (p.cursor + delta as usize).min(n.saturating_sub(1)) };
    }

    /// What a plugin is told: the open view, the rows on screen, and nothing else (no key, no relay, no other channel).
    pub fn projection(&self) -> Projection {
        let connection = match &self.conn {
            Conn::Live => "live".to_owned(),
            Conn::Connecting => "connecting".to_owned(),
            Conn::Down(why) => format!("offline: {why}"),
        };
        let mut messages: Vec<contrib::MessageView> = Vec::new();
        if let (Some(store), Some((_, layout))) = (self.store(), self.layout.as_ref()) {
            let mut seen = HashSet::new();
            for line in self.state.scroll..self.state.scroll + self.view_h.max(1) {
                let Some((i, _)) = layout.at_line(line) else { continue };
                let id = &layout.items[i].id;
                if !seen.insert(id.clone()) {
                    continue;
                }
                let Some(m) = store.msgs.get(id) else { continue };
                let (text, _) = store.content_of(m);
                let (replies, _) = store.replies_of(m);
                messages.push(contrib::MessageView {
                    id: m.id.clone(),
                    author: self.name_of(&m.pubkey),
                    pubkey: m.pubkey.clone(),
                    at: m.created_at,
                    text: contrib::clean_block(text),
                    replies,
                    selected: self.state.selected.as_deref() == Some(m.id.as_str()),
                });
            }
        }
        Projection {
            me: self.me.clone(),
            connection,
            channel: self.channel().map(|c| contrib::ChannelView { id: c.id.clone(), label: c.label.clone() }),
            view: if self.in_thread() { "thread" } else { "channel" }.to_owned(),
            selected: self.state.selected.clone(),
            messages,
            unread: self.total_unread(),
        }
    }
}

#[cfg(test)]
mod tests;
