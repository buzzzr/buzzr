//! What a plugin may put on the screen, and what the host does with it.
//!
//! Plugins run in their own process and never draw: they send plain data, and the host strips control characters, measures
//! with `unicode_width`, clips to the space that is actually left and maps a small set of tone names through `theme.rs`. A
//! plugin therefore cannot emit an escape sequence, and the message-text rules in `AGENTS.md` apply to it unchanged.

use ratatui::style::{Modifier, Style};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use unicode_width::UnicodeWidthChar;

/// The most text kept from one contribution, in characters.
pub const MAX_TEXT: usize = 2000;
/// The most rows a panel may send.
pub const MAX_ROWS: usize = 500;
/// The most rows a pane may carry. A pane lives under the channel list, so a plugin cannot ask for the whole screen.
pub const MAX_PANE_ROWS: usize = 20;
const MAX_SEGMENTS: usize = 32;
const MAX_MARKS: usize = 500;
const MAX_COMMANDS: usize = 64;

/// How a plugin's text is drawn. Names only: the host owns the colours, so `BUZZR_THEME` works for plugin output too.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    #[default]
    Default,
    Dim,
    Accent,
    Warn,
    Error,
    Ok,
    Bold,
}

impl Tone {
    pub fn style(self) -> Style {
        let th = crate::theme::get();
        match self {
            Tone::Default => Style::default(),
            Tone::Dim => Style::default().fg(th.dim),
            Tone::Accent => Style::default().fg(crate::theme::get().accent),
            Tone::Warn => Style::default().fg(crate::theme::get().warn),
            Tone::Error => Style::default().fg(crate::theme::get().err),
            Tone::Ok => Style::default().fg(crate::theme::get().ok),
            Tone::Bold => Style::default().add_modifier(Modifier::BOLD),
        }
    }
}

/// A piece of one line. A plugin sends text and a tone; nothing else reaches the terminal.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Segment {
    pub text: String,
    #[serde(default)]
    pub tone: Tone,
}

impl Segment {
    pub fn new(text: impl Into<String>, tone: Tone) -> Segment {
        Segment { text: text.into(), tone }
    }

    pub fn clean(mut self) -> Segment {
        self.text = clean_line(&self.text);
        self
    }

    /// Width in terminal cells. Test-only today: the host measures while it clips.
    #[cfg(test)]
    pub fn width(&self) -> usize {
        unicode_width::UnicodeWidthStr::width(self.text.as_str())
    }
}

/// Strip escapes and any other control character, and cap the length. Newlines and tabs become spaces: contributions are
/// single lines.
pub fn clean_line(text: &str) -> String {
    crate::markdown::strip_controls(text).replace(['\n', '\t'], " ").chars().take(MAX_TEXT).collect()
}

/// Message text is multi-line by nature; keep it readable but still capped.
pub fn clean_block(text: &str) -> String {
    crate::markdown::strip_controls(text).chars().take(MAX_TEXT).collect()
}

/// Clip a row to `width` terminal cells without splitting a character.
pub fn clip(segments: Vec<Segment>, width: usize) -> Vec<Segment> {
    let mut left = width;
    let mut out = Vec::new();
    for s in segments {
        if left == 0 {
            break;
        }
        let mut text = String::new();
        let mut used = 0;
        for ch in s.text.chars() {
            let w = UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + w > left {
                break;
            }
            used += w;
            text.push(ch);
        }
        if text.is_empty() {
            if !s.text.is_empty() {
                break; // not even the first character fits
            }
            continue;
        }
        left -= used;
        out.push(Segment { text, tone: s.tone });
    }
    out
}

#[cfg(test)]
pub fn line_width(segments: &[Segment]) -> usize {
    segments.iter().map(Segment::width).sum()
}

/// One command a plugin adds to the input box: `/todo`, `/prs`, ...
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct PluginCommand {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub usage: String,
}

/// Mark one message (`target` is an event id) or one channel with a short suffix.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Mark {
    pub target: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub tone: Tone,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct PanelRow {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub tone: Tone,
    /// What Enter sends back to the plugin, if anything.
    #[serde(default)]
    pub action: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Panel {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub rows: Vec<PanelRow>,
}

/// A small region a plugin owns that is always on screen: buzzr draws it under the channel list, and it is not
/// opened, focused or clicked. One pane per plugin. `rows` are single lines, clipped to the column's width.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Pane {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub rows: Vec<Segment>,
}

impl Pane {
    /// Nothing a plugin sends is trusted to be printable; run this before it reaches `Registry`.
    pub fn clean(mut self) -> Pane {
        self.title = clean_line(&self.title);
        self.rows = self.rows.into_iter().take(MAX_PANE_ROWS).map(Segment::clean).collect();
        self
    }

    /// A pane the host would draw nothing for: an empty title and no rows (what `{"type":"pane"}` means).
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.title.is_empty()
    }

    /// The rows it takes in the channel column: one for the title rule, plus one per line of content.
    pub fn height(&self) -> usize {
        self.rows.len() + 1
    }
}

/// Everything a plugin registers when it says it is ready.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Contributions {
    #[serde(default)]
    pub commands: Vec<PluginCommand>,
    /// Replaces the plugin's status line item (left of the built-in status text).
    #[serde(default)]
    pub status: Vec<Segment>,
    /// A short badge on the right of the header, before the unread count.
    #[serde(default)]
    pub header: Vec<Segment>,
    #[serde(default)]
    pub message_marks: Vec<Mark>,
    #[serde(default)]
    pub channel_marks: Vec<Mark>,
    /// An always-visible region under the channel list; `status`/`header` are one line, this is a block.
    #[serde(default)]
    pub pane: Option<Pane>,
}

impl Contributions {
    /// Nothing a plugin sends is trusted to be printable; run this on everything before it reaches `Registry`.
    pub fn clean(mut self) -> Contributions {
        self.commands.truncate(MAX_COMMANDS);
        for c in &mut self.commands {
            c.title = clean_line(&c.title);
            c.usage = clean_line(&c.usage);
            if c.title.is_empty() {
                c.title = c.id.clone();
            }
        }
        self.commands.retain(|c| valid_id(&c.id));
        self.commands.dedup_by(|a, b| a.id == b.id);
        self.status = clean_segments(self.status);
        self.header = clean_segments(self.header);
        self.message_marks.truncate(MAX_MARKS);
        self.channel_marks.truncate(MAX_MARKS);
        for m in &mut self.message_marks {
            m.text = clean_line(&m.text);
        }
        for m in &mut self.channel_marks {
            m.text = clean_line(&m.text);
        }
        self.message_marks.retain(|m| !m.text.is_empty() && !m.target.is_empty());
        self.channel_marks.retain(|m| !m.text.is_empty() && !m.target.is_empty());
        self.pane = self.pane.map(Pane::clean);
        self
    }
}

fn clean_segments(segments: Vec<Segment>) -> Vec<Segment> {
    let mut out: Vec<Segment> = segments.into_iter().take(MAX_SEGMENTS).map(Segment::clean).filter(|s| !s.text.is_empty()).collect();
    // leading and trailing separator noise is not useful
    while out.first().map(|s| s.text.trim().is_empty()).unwrap_or(false) {
        out.remove(0);
    }
    while out.last().map(|s| s.text.trim().is_empty()).unwrap_or(false) {
        out.pop();
    }
    out
}

/// Ids end up in JSON and in `/commands`, never in a path: keep them to a boring alphabet.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' || c == '.')
}

/// The plugin's contributions as the host holds them. One entry per plugin; removing a plugin removes everything it added.
#[derive(Default, Debug)]
pub struct Registry {
    plugins: BTreeMap<String, Entry>,
}

#[derive(Debug, Default)]
struct Entry {
    name: String,
    commands: Vec<PluginCommand>,
    status: Vec<Segment>,
    header: Vec<Segment>,
    message_marks: BTreeMap<String, Mark>,
    channel_marks: BTreeMap<String, Mark>,
    pane: Option<Pane>,
}

impl Registry {
    pub fn set(&mut self, id: &str, name: &str, c: Contributions) {
        let entry = Entry {
            name: name.to_owned(),
            commands: c.commands,
            status: c.status,
            header: c.header,
            message_marks: c.message_marks.into_iter().map(|m| (m.target.clone(), m)).collect(),
            channel_marks: c.channel_marks.into_iter().map(|m| (m.target.clone(), m)).collect(),
            pane: c.pane,
        };
        self.plugins.insert(id.to_owned(), entry);
    }

    pub fn remove(&mut self, id: &str) -> bool {
        self.plugins.remove(id).is_some()
    }

    /// Replace one plugin's status item (`status` is shorthand for a plugin that only ever sends that).
    pub fn set_status(&mut self, id: &str, segments: Vec<Segment>) -> bool {
        match self.plugins.get_mut(id) {
            Some(e) => {
                e.status = segments;
                true
            }
            None => false,
        }
    }

    pub fn name_of(&self, id: &str) -> Option<&str> {
        self.plugins.get(id).map(|e| e.name.as_str())
    }

    /// The plugin that owns a `/command`. Built-in buzzr commands are checked first by `App::command`.
    pub fn command(&self, id: &str) -> Option<(&str, &PluginCommand)> {
        self.plugins
            .iter()
            .find_map(|(plugin, e)| e.commands.iter().find(|c| c.id == id).map(|c| (plugin.as_str(), c)))
    }

    pub fn commands(&self) -> Vec<(&str, &PluginCommand)> {
        self.plugins.iter().flat_map(|(p, e)| e.commands.iter().map(move |c| (p.as_str(), c))).collect()
    }

    /// All plugins' status items on one line, separated by a dim dot.
    pub fn status(&self) -> Vec<Segment> {
        let mut out = Vec::new();
        for e in self.plugins.values().filter(|e| !e.status.is_empty()) {
            if !out.is_empty() {
                out.push(Segment::new(" · ", Tone::Dim));
            }
            out.extend(e.status.iter().cloned());
        }
        out
    }

    pub fn header(&self) -> Vec<Segment> {
        let mut out = Vec::new();
        for e in self.plugins.values().filter(|e| !e.header.is_empty()) {
            if !out.is_empty() {
                out.push(Segment::new(" · ", Tone::Dim));
            }
            out.extend(e.header.iter().cloned());
        }
        out
    }

    pub fn message_mark(&self, id: &str) -> Option<&Mark> {
        self.plugins.values().find_map(|e| e.message_marks.get(id))
    }

    pub fn channel_mark(&self, id: &str) -> Option<&Mark> {
        self.plugins.values().find_map(|e| e.channel_marks.get(id))
    }

    /// Replace one plugin's pane (`pane` is shorthand for a plugin that only ever sends that); `None` clears it.
    pub fn set_pane(&mut self, id: &str, pane: Option<Pane>) -> bool {
        match self.plugins.get_mut(id) {
            Some(e) => {
                e.pane = pane;
                true
            }
            None => false,
        }
    }

    /// Every plugin's pane, in plugin-id order — the order the channel column stacks them in. Empty panes are skipped.
    pub fn panes(&self) -> Vec<(&str, &Pane)> {
        self.plugins.iter().filter_map(|(id, e)| e.pane.as_ref().filter(|p| !p.is_empty()).map(|p| (id.as_str(), p))).collect()
    }
}

/// Where a discovered plugin is in its life. Shown by `/plugins`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PluginState {
    #[default]
    Discovered,
    Starting,
    Running,
    Failed,
    Disabled,
}

impl PluginState {
    pub fn label(self) -> &'static str {
        match self {
            PluginState::Discovered => "found",
            PluginState::Starting => "starting",
            PluginState::Running => "running",
            PluginState::Failed => "failed",
            PluginState::Disabled => "disabled",
        }
    }
}

/// What `/plugins` shows about one plugin on disk.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginMeta {
    pub id: String,
    pub name: String,
    pub state: PluginState,
    /// Why the plugin is not running: a folder that would not load, or the last runtime error; empty when all is well.
    pub detail: String,
    pub permissions: Vec<String>,
    pub commands: usize,
}

/// Messages the plugin runtime sends to `App`. They arrive like relay results: as `AppEvent::Plugin`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginEvent {
    Ready { id: String, name: String, contributions: Contributions },
    Meta(Vec<PluginMeta>),
    Status { id: String, segments: Vec<Segment> },
    /// A plugin replaced (or cleared) its pane under the channel list.
    Pane { id: String, pane: Option<Pane> },
    Panel { plugin: String, panel: Panel },
    Flash { text: String },
    Draft { plugin: String, text: String },
    Send { plugin: String, channel: Option<String>, text: String },
    /// A plugin asks buzzr to ask the user for a line or a block of text (see `plugin::PluginOutput::Prompt`).
    Prompt { plugin: String, id: String, title: String, hint: String, multiline: bool },
    /// A plugin asks for the local agents (see `plugin::PluginOutput::ListAgents`).
    ListAgents { plugin: String, id: String },
    /// A plugin asks buzzr to start a harness and read what it offers (see `plugin::PluginOutput::ProbeHarness`).
    ProbeHarness { plugin: String, id: String, harness: String, model: Option<String> },
    /// A plugin asks buzzr to make and publish a local agent (see `plugin::PluginOutput::CreateAgent`).
    CreateAgent { plugin: String, id: String, name: String, instructions: String, harness: String, model: String, effort: String },
    Gone { id: String, why: String },
    Log { id: String, level: String, text: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(text: &str) -> Segment {
        Segment::new(text, Tone::Default)
    }

    #[test]
    fn control_characters_never_reach_the_screen() {
        let dirty = Contributions { status: vec![seg("\u{1b}[31mred\u{7}]")], header: vec![seg("a\u{9b}bc")], ..Contributions::default() };
        let clean = dirty.clone().clean();
        assert_eq!(clean.status[0].text, "[31mred]");
        assert_eq!(clean.header[0].text, "abc");
        // the original is untouched: cleaning happens on the way in, once
        assert!(dirty.status[0].text.contains('\u{1b}'));
    }

    #[test]
    fn newlines_become_spaces_in_single_line_items() {
        assert_eq!(clean_line("a\nb\tc"), "a b c");
        assert_eq!(clean_block("a\nb").contains('\n'), true);
    }

    #[test]
    fn a_row_is_clipped_by_terminal_width_not_by_characters() {
        let row = vec![seg("ab"), seg("日本語"), seg("tail")];
        // "日本語" is six cells; with two cells left only the first character fits
        let clipped = clip(row, 4);
        assert_eq!(line_width(&clipped), 4);
        assert_eq!(clipped.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), vec!["ab", "日"]);
        assert!(clip(vec![seg("日本")], 1).is_empty());
    }

    #[test]
    fn empty_segments_are_dropped_and_a_long_status_is_capped() {
        let c = Contributions {
            status: vec![seg("  "), seg("ok"), seg(""), Segment::new("x".repeat(MAX_TEXT + 10), Tone::Dim)],
            ..Contributions::default()
        }
        .clean();
        assert_eq!(c.status.len(), 2);
        assert_eq!(c.status[1].text.chars().count(), MAX_TEXT);
    }

    #[test]
    fn commands_need_a_safe_id() {
        let mut c = Contributions {
            commands: vec![
                PluginCommand { id: "fine".into(), title: "Fine".into(), usage: String::new() },
                PluginCommand { id: "../etc".into(), title: "Bad".into(), usage: String::new() },
                PluginCommand { id: String::new(), title: "Empty".into(), usage: String::new() },
            ],
            ..Contributions::default()
        };
        c = c.clean();
        assert_eq!(c.commands.len(), 1);
        assert_eq!(c.commands[0].id, "fine");
    }

    #[test]
    fn a_pane_is_cleaned_capped_and_owned_by_its_plugin() {
        let dirty = Contributions {
            pane: Some(Pane {
                title: "Clock\u{1b}[31m".into(),
                rows: (0..MAX_PANE_ROWS + 5).map(|i| if i == 0 { seg("12\u{7}:00") } else { seg("x") }).collect(),
            }),
            ..Contributions::default()
        };
        let c = dirty.clean();
        let pane = c.pane.as_ref().expect("a pane survives cleaning");
        assert_eq!(pane.title, "Clock[31m");
        assert_eq!(pane.rows[0].text, "12:00");
        assert_eq!(pane.rows.len(), MAX_PANE_ROWS, "a pane cannot take the whole screen");

        let mut reg = Registry::default();
        reg.set("clock", "Clock", c);
        assert_eq!(reg.panes().len(), 1);
        assert_eq!(reg.panes()[0].0, "clock");
        assert_eq!(reg.panes()[0].1.height(), MAX_PANE_ROWS + 1);

        // a plugin may replace its pane, and clearing it takes the space back
        assert!(reg.set_pane("clock", Some(Pane { title: "Later".into(), rows: vec![seg("x")] })));
        assert_eq!(reg.panes()[0].1.title, "Later");
        assert!(reg.set_pane("clock", None));
        assert!(reg.panes().is_empty());
        // an empty pane is the same as none: it is not drawn, so it takes no height
        reg.set_pane("clock", Some(Pane::default()));
        assert!(reg.panes().is_empty());
        assert!(reg.set_pane("clock", Some(Pane { title: "Clock".into(), rows: vec![seg("x")] })));

        // one pane per plugin; the stack follows the plugin id order
        reg.set("aaa", "A", Contributions { pane: Some(Pane { title: "A".into(), rows: vec![seg("a")] }), ..Contributions::default() });
        reg.set("zzz", "Z", Contributions { pane: Some(Pane { title: "Z".into(), rows: vec![seg("z")] }), ..Contributions::default() });
        assert_eq!(reg.panes().iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec!["aaa", "clock", "zzz"]);
        assert_eq!(reg.panes().iter().map(|(_, p)| p.height()).sum::<usize>(), 6, "a title rule plus one row each");
        assert!(reg.remove("clock"));
        assert_eq!(reg.panes().len(), 2, "a gone plugin takes its pane with it");
        // a status update for a plugin with a pane leaves the pane alone
        assert!(reg.set_status("aaa", vec![seg("up")]));
        assert_eq!(reg.panes()[0].1.title, "A");
    }

    #[test]
    fn a_plugin_can_replace_and_lose_all_of_its_contributions() {
        let mut reg = Registry::default();
        let c = Contributions {
            commands: vec![PluginCommand { id: "prs".into(), title: "Pull requests".into(), usage: String::new() }],
            status: vec![Segment::new("2 PRs", Tone::Accent)],
            header: vec![Segment::new("gh", Tone::Dim)],
            message_marks: vec![Mark { target: "m1".into(), text: "PR".into(), tone: Tone::Accent }],
            channel_marks: vec![Mark { target: "c1".into(), text: "3".into(), tone: Tone::Warn }],
            pane: Some(Pane { title: "Clock".into(), rows: vec![Segment::new("12:00", Tone::Accent)] }),
        };
        reg.set("github", "GitHub", c.clone().clean());
        assert_eq!(reg.command("prs").map(|(p, _)| p), Some("github"));
        assert_eq!(reg.status()[0].text, "2 PRs");
        assert_eq!(reg.message_mark("m1").map(|m| m.text.as_str()), Some("PR"));
        assert_eq!(reg.channel_mark("c1").map(|m| m.text.as_str()), Some("3"));
        assert_eq!(reg.header()[0].text, "gh");
        assert_eq!(reg.panes()[0].1.rows[0].text, "12:00");

        // two plugins: statuses are joined in id order
        reg.set("zzz", "Last", Contributions { status: vec![Segment::new("z", Tone::Default)], ..Contributions::default() });
        let status = reg.status();
        assert_eq!(status.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(""), "2 PRs · z");

        assert!(reg.remove("github"));
        assert!(!reg.remove("github"));
        assert!(reg.command("prs").is_none());
        assert!(reg.message_mark("m1").is_none());
        assert!(reg.panes().is_empty(), "the pane goes with the plugin");
        assert!(reg.status().iter().all(|s| s.text != "2 PRs"));
    }
}

// ---- the two things that cross between `App` and the plugin runtime ------------------------------------------------------

/// What a plugin is told about the open view: only what the screen shows. No key, no relay address, no other channel.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Projection {
    pub me: String,
    /// `live`, `connecting` or `offline: <why>`.
    pub connection: String,
    pub channel: Option<ChannelView>,
    /// `channel` or `thread`.
    pub view: String,
    pub selected: Option<String>,
    pub messages: Vec<MessageView>,
    pub unread: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize)]
pub struct ChannelView {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize)]
pub struct MessageView {
    pub id: String,
    pub author: String,
    pub pubkey: String,
    pub at: u64,
    pub text: String,
    pub replies: u64,
    pub selected: bool,
}

/// Work for the plugin runtime, like `Cmd` is work for the relay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginCmd {
    /// `/command args` from the input box.
    Invoke { plugin: String, command: String, args: String },
    /// Enter on a panel row that has an action.
    PanelAction { plugin: String, id: String, row: usize, value: Option<String> },
    /// The user answered (or, with `None`, cancelled) a prompt the plugin asked for with `Prompt`.
    PromptAnswer { plugin: String, id: String, text: Option<String> },
    /// The local agents, as the answer to the plugin's `ListAgents`.
    Agents { plugin: String, id: String, agents: Vec<crate::plugin::AgentView> },
    /// The answer to a plugin's `CreateAgent`: the agent that was made, or why it was not.
    Created { plugin: String, id: String, result: Result<crate::plugin::CreatedAgent, String> },
    /// The answer to a plugin's `ProbeHarness`: what the harness offers, or why it could not be asked.
    HarnessOffer { plugin: String, id: String, result: Result<crate::plugin::HarnessOffer, String> },
}
