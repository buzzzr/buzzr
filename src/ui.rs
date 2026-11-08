//! Drawing the screen. Everything here reads `App`; the only thing it changes is the geometry it reports back (so that clicks
//! can be mapped to what is on screen).

use crate::app::{App, Conn, Focus, Overlay, Row, SideRow, View};
use crate::list;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

const CHANNELS_W: u16 = 28;

fn dim() -> Style {
    Style::default().fg(crate::theme::get().dim)
}

fn faint() -> Style {
    Style::default().fg(crate::theme::get().faint)
}

fn key_hint(key: &str, what: &str) -> Vec<Span<'static>> {
    vec![Span::styled(format!(" {key} "), Style::default().add_modifier(Modifier::REVERSED)), Span::styled(format!(" {what}  "), dim())]
}

/// The scheme's own background and text colour; a scheme that leaves both to the terminal gives the default style.
fn base_style(popup: bool) -> Style {
    let th = crate::theme::get();
    let mut style = Style::default();
    if let Some(bg) = if popup { th.popup_bg.or(th.bg) } else { th.bg } {
        style = style.bg(bg);
    }
    if let Some(fg) = th.fg {
        style = style.fg(fg);
    }
    style
}

/// Blank the space under a popup, then give it the popup background (a plain `Clear` would let the terminal's colours through).
fn clear_popup(f: &mut Frame, area: Rect) {
    f.render_widget(Clear, area);
    f.buffer_mut().set_style(area, base_style(true));
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    f.buffer_mut().set_style(area, base_style(false));
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(1)]).split(area);
    let (header, body, footer) = (rows[0], rows[1], rows[2]);
    let cols = Layout::horizontal([Constraint::Length(CHANNELS_W.min(body.width / 3)), Constraint::Min(10)]).split(body);
    let (chans, right) = (cols[0], cols[1]);

    // the input box grows with its text, up to 10 lines
    let input_inner_w = right.width.saturating_sub(4).max(4) as usize;
    let input_rows = app.editor.rows(input_inner_w).len().clamp(1, 10) as u16;
    let rr = Layout::vertical([Constraint::Min(2), Constraint::Length(1), Constraint::Length(1), Constraint::Length(input_rows + 2)]).split(right);
    let (msgs, typing, status, input) = (rr[0], rr[1], rr[2], rr[3]);

    app.areas.channels = chans;
    app.areas.msgs = msgs;
    app.areas.input = input;
    app.view_h = msgs.height as usize;
    let content_w = msgs.width.saturating_sub(2).max(10) as usize;
    app.layout(content_w); // (re)build the layout for this width before reading it

    draw_header(f, app, header);
    draw_channels(f, app, chans);
    draw_messages(f, app, msgs);
    draw_typing(f, app, typing);
    draw_status(f, app, status);
    draw_input(f, app, input, input_inner_w);
    draw_footer(f, app, footer);
    match app.overlay {
        Overlay::Keys => draw_keys(f, body),
        Overlay::Help => draw_help(f, area),
        Overlay::Search => draw_search(f, app, area),
        Overlay::Whois => draw_whois(f, app, area),
        Overlay::Panel => draw_panel(f, app, area),
        Overlay::None => {}
    }
}

/// Contribution segments as ratatui spans, clipped to the space that is actually left.
fn spans(segments: &[crate::contrib::Segment], width: usize) -> Vec<Span<'static>> {
    crate::contrib::clip(segments.to_vec(), width).into_iter().map(|s| Span::styled(s.text, s.tone.style())).collect()
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let title = match (app.channel(), &app.view) {
        (None, _) => "no channel".to_owned(),
        (Some(c), View::Channel) => if c.is_dm() { c.label.clone() } else { format!("#{}", c.label) },
        (Some(c), View::Thread(_)) => format!("{} › thread", if c.is_dm() { c.label.clone() } else { format!("#{}", c.label) }),
    };
    let left = Line::from(vec![Span::styled(" buzzr ", Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED)), Span::raw(format!(" {title}"))]);
    let (dot, text, color) = match &app.conn {
        Conn::Live => ("●", "live".to_owned(), crate::theme::get().ok),
        Conn::Connecting => ("◌", "connecting…".to_owned(), crate::theme::get().warn),
        Conn::Down(why) => ("✕", format!("offline, retrying ({})", why.chars().take(40).collect::<String>()), crate::theme::get().err),
    };
    let mut right = vec![];
    let badges = app.plugins.header();
    if !badges.is_empty() {
        right.extend(spans(&badges, area.width as usize / 2));
        right.push(Span::raw("   "));
    }
    let unread = app.total_unread();
    if unread > 0 {
        right.push(Span::styled(format!("• {unread} unread   "), Style::default().fg(crate::theme::get().warn)));
    }
    right.push(Span::styled(format!("{dot} {text} "), Style::default().fg(color)));
    let right_w: u16 = right.iter().map(|s| s.content.width() as u16).sum();
    let parts = Layout::horizontal([Constraint::Min(1), Constraint::Length(right_w.min(area.width))]).split(area);
    f.render_widget(Paragraph::new(left), parts[0]);
    f.render_widget(Paragraph::new(Line::from(right)), parts[1]);
}

fn draw_channels(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::RIGHT).border_style(faint());
    let inner = block.inner(area);
    f.render_widget(block, area);
    let focused = app.focus == Focus::Channels;
    let mut lines = Vec::new();
    let rows = app.sidebar_rows();
    let top = app.sidebar_scroll(&rows, inner.height as usize);
    for row in rows.into_iter().skip(top).take(inner.height as usize) {
        let i = match row {
            SideRow::Title(section) => {
                lines.push(Line::from(Span::styled(format!(" {}", section.title()), dim().add_modifier(Modifier::BOLD))));
                continue;
            }
            SideRow::Channel(i) => i,
        };
        let c = &app.channels[i];
        let open = app.cur == Some(i);
        let bar = app.channel_bar() == Some(i);
        let unread = app.unread.get(&c.id).copied().unwrap_or(0);
        let mut style = Style::default();
        if open {
            style = style.add_modifier(Modifier::BOLD);
        }
        if unread > 0 {
            style = style.fg(crate::theme::get().warn).add_modifier(Modifier::BOLD);
        }
        let marker = if bar { "▌" } else { " " };
        // `#` for an open channel, `*` for a private one (RFC 2811's private-channel prefix); both are ASCII, so every font
        // has them. DMs are pulled out by `is_dm` and already labelled `✉ name`, so they get no marker.
        let glyph = if c.is_dm() {
            ""
        } else if c.is_private {
            "*"
        } else {
            "#"
        };
        let badge = if unread > 0 { format!(" ({unread})") } else { String::new() };
        let mark = app.plugins.channel_mark(&c.id);
        let mark_text = mark.map(|m| format!(" {}", m.text)).unwrap_or_default();
        let room = (inner.width as usize).saturating_sub(2 + glyph.width() + badge.width() + mark_text.width());
        let mut label: String = c.label.clone();
        while label.width() > room {
            label.pop();
        }
        let mut spans = vec![Span::styled(marker, Style::default().fg(crate::theme::get().bar_on)), Span::styled(format!(" {glyph}{label}{badge}"), style)];
        if let Some(m) = mark {
            spans.push(Span::styled(mark_text, m.tone.style()));
        }
        let mut line = Line::from(spans);
        if bar && focused {
            line = line.style(Style::default().bg(crate::theme::get().sel_bg));
        }
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_messages(f: &mut Frame, app: &App, area: Rect) {
    let Some((_, layout)) = app.layout_ref() else { return };
    let view_h = area.height as usize;
    if layout.items.is_empty() {
        let text = if app.channel().is_none() { "Pick a channel on the left." } else if app.store().map(|s| s.head_loaded).unwrap_or(false) { "No messages yet." } else { "Loading…" };
        f.render_widget(Paragraph::new(Span::styled(text, dim())).alignment(Alignment::Center), Rect::new(area.x, area.y + area.height / 3, area.width, 1));
        return;
    }
    let selected = list::selected_index(layout, &app.state);
    let focused = app.focus == Focus::Messages;
    // One column is reserved for the scrollbar, so the text pane is a column narrower than the area.
    let text_area = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    let pane_w = text_area.width as usize;
    let mut rows: Vec<Line> = Vec::with_capacity(view_h);
    for y in 0..view_h {
        match layout.at_line(app.state.scroll + y) {
            None => rows.push(Line::default()),
            Some((i, within)) => {
                let is_sel = Some(i) == selected && within.is_some();
                let gutter = if is_sel { Span::styled("▌", Style::default().fg(if focused { crate::theme::get().bar_on } else { crate::theme::get().bar_off })) } else { Span::raw(" ") };
                let mut spans = vec![gutter];
                if let Some(l) = within {
                    spans.extend(layout.items[i].lines[l].spans.iter().cloned());
                }
                let mut line = Line::from(spans);
                if is_sel {
                    // A ratatui `Line` only paints the cells its spans cover, so pad the row out first: the background then
                    // runs from the left edge (over the gutter marker) to the right edge of the message pane.
                    let used = line.width();
                    if used < pane_w {
                        line.spans.push(Span::raw(" ".repeat(pane_w - used)));
                    }
                    line = line.style(Style::default().bg(crate::theme::get().sel_bg));
                }
                rows.push(line);
            }
        }
    }
    f.render_widget(Paragraph::new(rows), text_area);
    if layout.total > view_h {
        let mut sb = ScrollbarState::new(layout.max_scroll(view_h)).position(app.state.scroll);
        f.render_stateful_widget(Scrollbar::new(ScrollbarOrientation::VerticalRight).track_symbol(Some("│")).thumb_symbol("█").style(faint()), area, &mut sb);
    }
}

fn draw_typing(f: &mut Frame, app: &App, area: Rect) {
    let who = app.typing_now();
    if who.is_empty() {
        return;
    }
    let dots = ".".repeat(1 + (app.tick_count / 2 % 3) as usize);
    let verb = if who.len() == 1 { "is typing" } else { "are typing" };
    f.render_widget(Paragraph::new(Span::styled(format!(" {} {verb}{dots}", who.join(", ")), dim().add_modifier(Modifier::ITALIC))), area);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    if app.confirm_delete.is_some() {
        let prompt = Span::styled("Delete this message? press y to delete, anything else cancels", Style::default().fg(crate::theme::get().warn).add_modifier(Modifier::BOLD));
        f.render_widget(Paragraph::new(prompt).alignment(Alignment::Right), area);
        return;
    }
    let text = if let Some(t) = app.flash_text() {
        t.to_owned()
    } else if app.reply_target.is_some() {
        "replying in a thread · Esc cancels".to_owned()
    } else if app.in_thread() {
        "thread · Esc: back to the channel".to_owned()
    } else if let Some(s) = app.store() {
        let more = if s.has_more { " · Up at the top loads older" } else { "" };
        format!("{} {} loaded{more}", s.timeline().len(), if s.timeline().len() == 1 { "message" } else { "messages" })
    } else {
        String::new()
    };
    let mut items = app.plugins.status();
    if let Some(hint) = app.update_hint() {
        items.insert(0, crate::contrib::Segment::new(hint, crate::contrib::Tone::Warn));
    }
    if items.is_empty() {
        f.render_widget(Paragraph::new(Span::styled(text, dim())).alignment(Alignment::Right), area);
        return;
    }
    let right_w = (text.width() as u16).min(area.width);
    let parts = Layout::horizontal([Constraint::Min(1), Constraint::Length(right_w)]).split(area);
    f.render_widget(Paragraph::new(Line::from(spans(&items, parts[0].width as usize))), parts[0]);
    f.render_widget(Paragraph::new(Span::styled(text, dim())).alignment(Alignment::Right), parts[1]);
}

/// A plugin panel (or the built-in `/plugins` list): rows, a cursor and the row actions the host owns.
fn draw_panel(f: &mut Frame, app: &App, area: Rect) {
    let Some(panel) = &app.panel else { return };
    let w = 100.min(area.width.saturating_sub(4)).max(24);
    let h = (panel.rows.len() as u16 + 4).min(area.height.saturating_sub(2)).max(6);
    let r = Rect::new(area.x + (area.width.saturating_sub(w)) / 2, area.y + (area.height.saturating_sub(h)) / 2, w, h);
    clear_popup(f, r);
    let title = format!(" {} · Up/Down · Enter runs · Esc closes ", panel.title);
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(crate::theme::get().accent)).title(title);
    let inner = block.inner(r);
    f.render_widget(block, r);
    if inner.height == 0 {
        return;
    }
    let rows: Vec<Line> = panel
        .rows
        .iter()
        .map(|row| {
            let marker = if row.action.is_some() { "▸ " } else { "  " };
            let mut spans = vec![Span::styled(marker, dim())];
            spans.extend(spans_of(row.text.chars().take(crate::contrib::MAX_TEXT).collect::<String>(), row.tone, inner.width as usize));
            Line::from(spans)
        })
        .collect();
    let vis = inner.height as usize;
    let start = panel.cursor.saturating_sub(vis.saturating_sub(1) / 2).min(rows.len().saturating_sub(vis));
    let visible: Vec<Line> = rows.into_iter().skip(start).take(vis).collect();
    f.render_widget(Paragraph::new(visible), inner);
    if panel.cursor >= start && panel.cursor < start + vis {
        f.set_cursor_position(Position::new(inner.x, inner.y + (panel.cursor - start) as u16));
    }
}

fn spans_of(text: String, tone: crate::contrib::Tone, width: usize) -> Vec<Span<'static>> {
    spans(&[crate::contrib::Segment { text, tone }], width)
}

fn draw_input(f: &mut Frame, app: &App, area: Rect, inner_w: usize) {
    let focused = app.focus == Focus::Input;
    let title = if app.editing.is_some() {
        " edit message "
    } else if app.reply_target.is_some() || app.in_thread() {
        " reply in thread "
    } else {
        " message "
    };
    let block = Block::default().borders(Borders::ALL).border_style(if focused { Style::default().fg(crate::theme::get().accent) } else { faint() }).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if app.editor.is_empty() {
        let hint = if app.read_only { "read-only: nothing will be sent" } else { "Type a message · /help for commands · F1 for keys" };
        f.render_widget(Paragraph::new(Span::styled(hint, dim())), inner);
    } else {
        let lines: Vec<Line> = app.editor.rows(inner_w).iter().map(|r| Line::raw(app.editor.row_text(r))).collect();
        let scroll = lines.len().saturating_sub(inner.height as usize) as u16;
        f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), inner);
    }
    if focused && app.overlay == Overlay::None {
        let (row, col) = app.editor.cursor_pos(inner_w);
        let total = app.editor.rows(inner_w).len();
        let visible_row = row.saturating_sub(total.saturating_sub(inner.height as usize));
        f.set_cursor_position(Position::new(inner.x + col as u16, inner.y + visible_row as u16));
    }
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let mut spans: Vec<Span> = Vec::new();
    match app.focus {
        Focus::Channels => spans.extend(key_hint("⏎", "Open channel")),
        Focus::Messages => {
            spans.extend(key_hint("⏎", if app.in_thread() { "Reply" } else { "Open thread" }));
            spans.extend(key_hint("r", "Reply"));
            spans.extend(key_hint("c", "Copy"));
            if app.selected_is_own() {
                spans.extend(key_hint("e", "Edit"));
                spans.extend(key_hint("d", "Delete"));
            }
            if app.in_thread() {
                spans.extend(key_hint("esc", "Back"));
            }
        }
        Focus::Input => {
            spans.extend(key_hint("⏎", "Send"));
            if app.editing.is_some() {
                spans.extend(key_hint("esc", "Cancel edit"));
            } else if app.reply_target.is_some() {
                spans.extend(key_hint("esc", "Cancel reply"));
            }
        }
    }
    spans.extend(key_hint("tab", "Switch pane"));
    spans.extend(key_hint("^F", "Search"));
    spans.extend(key_hint("F1", "Keys"));
    spans.extend(key_hint("F2", "Help"));
    spans.extend(key_hint("^q", "Quit"));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub const KEYS: &[(&str, &str)] = &[
    ("Tab / Shift+Tab", "switch between channels, messages and the input"),
    ("Esc", "close a panel, cancel a reply, leave a thread (in that order)"),
    ("F1", "this list of keys"),
    ("F2", "help: commands and how things work"),
    ("Ctrl+R", "reload the open channel or thread"),
    ("Ctrl+O", "load older messages"),
    ("Ctrl+Q / Ctrl+C", "quit"),
    ("", ""),
    ("Channels", ""),
    ("Up / Down, Home / End", "move"),
    ("Enter", "open the channel"),
    ("", ""),
    ("Messages", ""),
    ("Up / Down", "select a message (Up at the top loads older)"),
    ("PgUp / PgDn, Home / End", "scroll, first, last"),
    ("Enter", "open the thread (in a thread: reply)"),
    ("r", "reply in the thread of the selected message"),
    ("c", "copy the selected message to the clipboard"),
    ("e", "edit the selected message (yours only)"),
    ("d", "delete the selected message (yours only; asks first)"),
    ("Ctrl+F", "search channels, people and every message"),
    ("click", "select; open the thread; on a link: copy it"),
    ("mouse wheel", "scroll"),
    ("", ""),
    ("Panel (from /plugins or a plugin)", ""),
    ("Up / Down, PgUp / PgDn, Home / End", "move"),
    ("Enter", "run the selected row"),
    ("Esc", "close"),
    ("", ""),
    ("Profile card (from /whois)", ""),
    ("Esc", "close"),
    ("", ""),
    ("Writing", ""),
    ("Enter", "send"),
    ("Shift+Enter, Alt+Enter, Ctrl+J", "new line"),
    ("Ctrl+A / E, Ctrl+W / U / K", "line start / end, kill word / to start / to end"),
    ("Ctrl+← →, Alt+B / F", "move by word"),
];

fn draw_keys(f: &mut Frame, body: Rect) {
    let w = 92.min(body.width);
    let area = Rect::new(body.x + body.width - w, body.y, w, body.height);
    clear_popup(f, area);
    let mut lines = vec![Line::from(Span::styled("Keys  (Esc closes)", Style::default().add_modifier(Modifier::BOLD))), Line::default()];
    for (k, d) in KEYS {
        if k.is_empty() && d.is_empty() {
            lines.push(Line::default());
        } else if d.is_empty() {
            lines.push(Line::from(Span::styled(*k, Style::default().fg(crate::theme::get().accent).add_modifier(Modifier::BOLD))));
        } else {
            lines.push(Line::from(vec![Span::styled(format!("{k:<32}"), Style::default().fg(crate::theme::get().warn)), Span::styled((*d).to_owned(), dim())]));
        }
    }
    f.render_widget(Paragraph::new(lines).block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(crate::theme::get().accent))), area);
}

pub const HELP: &[&str] = &[
    "Channels list the top-level messages only; replies live in their thread (Enter or a click opens it).",
    "A message with replies shows how many and when the last one was. Esc returns you to the same place.",
    "",
    "Commands, typed in the input (N is the message number shown as #N):",
    "  /r N text   reply to message N        /t N   open the thread of message N",
    "  /c          back to the channel        /copy N   copy message N",
    "  /more       load older messages        /refresh   reload now",
    "  /help       this screen                /q   quit",
    "  /edit N     edit your message N        /delete N   delete your message N (asks first)",
    "  /whois NAME their profile             /dm NAME   start (or open) a DM",
    "  /search text   search                  /new [private] name [about]   create a channel",
    "  /plugins    what is loaded, and the commands plugins added",
    "  /update     how to upgrade this copy (brew, cargo or the release page)",
    "A newer release shows up in the status line (BUZZR_NO_UPDATE_CHECK=1 turns the once-a-day check off).",
    "  /read       mark this channel read     /read all   mark every channel read",
    "Unread survives a restart: it comes from the last-read position in ~/.config/buzzr/read-state.json, and a",
    "\"new messages\" line marks where new messages start.",
    "Search (Ctrl+F): channel names, people and full-text message search; Enter opens the result — a person starts a DM.",
    "A person is found by the name in their profile, or by their npub (hex or npub1…): /dm and /whois take either.",
    "Others see \"typing…\" while you write (start with --no-typing to turn that off).",
    "",
    "New messages, edits, deletions, reactions and typing indicators arrive live; there is no polling.",
    "Links: clicking one copies its URL to your clipboard (your terminal must allow OSC 52; in tmux:",
    "  set -g set-clipboard on). `c` copies a whole message the same way.",
    "@name in a message mentions that member of the channel.",
    "",
    "Press Esc, Enter or F2 to close.",
];

/// Universal search: a query line, then channels whose name matches, then messages the relay found.
fn draw_search(f: &mut Frame, app: &App, area: Rect) {
    let w = 110.min(area.width.saturating_sub(4)).max(20);
    let h = 26.min(area.height.saturating_sub(2)).max(6);
    let r = Rect::new(area.x + (area.width.saturating_sub(w)) / 2, area.y + (area.height.saturating_sub(h)) / 2, w, h);
    clear_popup(f, r);
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(crate::theme::get().accent)).title(" Search · Enter opens · Esc closes ");
    let inner = block.inner(r);
    f.render_widget(block, r);
    if inner.height < 3 || inner.width < 10 {
        return;
    }
    let query = app.search.query.text();
    let prefix = "Search: ";
    f.render_widget(Paragraph::new(Line::from(vec![Span::styled(prefix, Style::default().add_modifier(Modifier::BOLD)), Span::raw(query.clone())])), Rect::new(inner.x, inner.y, inner.width, 1));
    let (_, col) = app.search.query.cursor_pos(inner.width as usize);
    f.set_cursor_position(Position::new((inner.x + prefix.len() as u16 + col as u16).min(inner.x + inner.width - 1), inner.y));
    let status = if app.search.loading {
        "searching…".to_owned()
    } else if query.trim().chars().count() < 2 {
        "type at least two characters: channel names and all messages are searched".to_owned()
    } else {
        format!("{} messages found", app.search.messages.len())
    };
    f.render_widget(Paragraph::new(Span::styled(status, dim())), Rect::new(inner.x, inner.y + 1, inner.width, 1));

    let list_area = Rect::new(inner.x, inner.y + 3, inner.width, inner.height.saturating_sub(3));
    let rows = app.search_rows();
    let vis = list_area.height as usize;
    let start = app.search.cursor.saturating_sub(vis.saturating_sub(1) / 2).min(rows.len().saturating_sub(vis));
    let mut lines = Vec::new();
    for (i, row) in rows.iter().enumerate().skip(start).take(vis) {
        let mut spans = vec![Span::raw(if i == app.search.cursor { "▌" } else { " " })];
        match row {
            Row::Channel(c) => {
                let ch = &app.channels[*c];
                spans.push(Span::styled(if ch.is_dm() { ch.label.clone() } else { format!("# {}", ch.label) }, Style::default().fg(crate::theme::get().accent).add_modifier(Modifier::BOLD)));
                spans.push(Span::styled("  channel", dim()));
            }
            Row::Person(pk) => {
                spans.push(Span::styled(app.name_of(pk), Style::default().fg(crate::theme::get().accent).add_modifier(Modifier::BOLD)));
                spans.push(Span::styled("  person · Enter opens a DM", dim()));
            }
            Row::Message(e) => {
                let chan = app.channels.iter().find(|c| Some(c.id.as_str()) == e.channel()).map(|c| c.label.clone()).unwrap_or_default();
                let head = format!(" {chan} · {} · ", app.name_of(&e.pubkey));
                let snippet: String = crate::markdown::strip_controls(&e.content).split_whitespace().collect::<Vec<_>>().join(" ");
                spans.push(Span::styled(head.clone(), dim()));
                let room = (list_area.width as usize).saturating_sub(head.width() + 2);
                let mut text = snippet;
                while text.width() > room {
                    text.pop();
                }
                spans.push(Span::raw(text));
            }
        }
        let mut line = Line::from(spans);
        if i == app.search.cursor {
            line = line.style(Style::default().bg(crate::theme::get().sel_bg));
        }
        lines.push(line);
    }
    if rows.is_empty() && query.trim().chars().count() >= 2 && !app.search.loading {
        lines.push(Line::from(Span::styled(" nothing found", dim())));
    }
    f.render_widget(Paragraph::new(lines), list_area);
}

/// Somebody's profile card (`/whois NAME`): the name, the npub, and what they wrote about themselves. Everything from the
/// relay goes through the Markdown renderer (which strips control characters and wraps), so nothing in a profile can reach the
/// terminal as anything but text.
fn draw_whois(f: &mut Frame, app: &App, area: Rect) {
    let Some(w) = &app.whois else { return };
    let width = 72.min(area.width.saturating_sub(4)).max(24);
    let inner_w = width.saturating_sub(2) as usize;
    let text = |s: &str| crate::markdown::render(s, inner_w).into_iter().map(|l| Line::from(l.spans));
    let mut lines: Vec<Line> = Vec::new();
    let name = if w.name.trim().is_empty() { "(no name in their profile)" } else { w.name.as_str() };
    lines.extend(text(name).map(|l| l.style(Style::default().add_modifier(Modifier::BOLD))));
    lines.extend(text(&w.npub).map(|l| l.style(dim())));
    lines.push(Line::default());
    match (w.about.as_deref().map(str::trim), w.loading) {
        (Some(about), _) if !about.is_empty() => lines.extend(text(about)),
        (_, true) => lines.push(Line::from(Span::styled("loading their profile…", dim()))),
        _ => lines.push(Line::from(Span::styled("no about line in their profile", dim()))),
    }
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2)).max(4);
    let r = Rect::new(area.x + (area.width.saturating_sub(width)) / 2, area.y + (area.height.saturating_sub(h)) / 2, width, h);
    clear_popup(f, r);
    let title = format!(" {} · Esc closes ", if w.name.trim().is_empty() { "profile" } else { w.name.as_str() });
    f.render_widget(Paragraph::new(lines).block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(crate::theme::get().accent)).title(title)), r);
}

fn draw_help(f: &mut Frame, area: Rect) {
    let w = 100.min(area.width.saturating_sub(4));
    let h = (HELP.len() as u16 + 4).min(area.height);
    let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    clear_popup(f, r);
    let mut lines = vec![Line::from(Span::styled("Help", Style::default().add_modifier(Modifier::BOLD))), Line::default()];
    lines.extend(HELP.iter().map(|l| Line::raw(*l)));
    f.render_widget(Paragraph::new(lines).block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(crate::theme::get().accent))), r);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{AppEvent, Cmd};
    use crate::live::Live;
    use crate::model::{tests::ev, Channel};
    use crate::relay::Window;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn screen(term: &Terminal<TestBackend>) -> Vec<String> {
        let buf = term.backend().buffer();
        (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_owned()).collect::<String>()).collect()
    }

    fn app() -> App {
        let mut app = App::new("me".into(), false, chrono_tz::UTC);
        let chan = |id: &str, name: &str| Channel { id: id.into(), name: name.into(), about: String::new(), created_at: 1, section: crate::model::Section::Channels, label: name.into(), is_private: false };
        let _: Vec<Cmd> = app.handle(AppEvent::Channels(Ok(vec![chan("c1", "general"), chan("c2", "random")])));
        let mut m1 = ev("a", 1000, "**Hello** there, see [docs](https://example.com)", &[]);
        m1.pubkey = "bob".into();
        let mut m2 = ev("b", 1001, "second\nline", &[]);
        m2.pubkey = "me".into();
        let mut r = ev("r1", 1002, "reply", &[&["e", "a", "", "reply"]]);
        r.pubkey = "bob".into();
        app.handle(AppEvent::Names(Ok([("bob".to_string(), "Bob".to_string())].into())));
        app.handle(AppEvent::Window { channel: "c1".into(), cursor: None, result: Ok(Window { events: vec![m1, m2, r], has_more: true, next_cursor: Some((1, "a".into())) }) });
        app.handle(AppEvent::Live(Live::Up));
        app
    }

    #[test]
    fn a_person_in_the_search_and_a_profile_card_are_drawn() {
        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let alice = "aa".repeat(32);
        let mut app = app();
        app.handle(AppEvent::Names(Ok([("bob".to_string(), "Bob".to_string()), (alice.clone(), "Alice Smith".to_string())].into())));
        app.overlay = Overlay::Search;
        app.search.query.insert_str("ali");
        term.draw(|f| draw(f, &mut app)).unwrap();
        let s = screen(&term).join("\n");
        assert!(s.contains("Alice Smith") && s.contains("person · Enter opens a DM"), "a person in the search says what Enter does\n{s}");

        app.overlay = Overlay::Whois;
        app.whois = Some(crate::app::Whois { pubkey: alice.clone(), name: "Alice Smith".into(), npub: crate::relay::npub_of(&alice).unwrap(), about: Some("I sail. **A lot.**".into()), loading: false });
        term.draw(|f| draw(f, &mut app)).unwrap();
        let s = screen(&term).join("\n");
        assert!(s.contains("Alice Smith") && s.contains("npub1"), "the card names the person and shows their npub\n{s}");
        assert!(s.contains("I sail. A lot."), "the about line is rendered as markdown\n{s}");
        assert!(!s.contains("**"), "and not shown raw");
        assert!(s.contains("Esc closes"), "the card says how to close it\n{s}");
    }

    #[test]
    fn the_screen_shows_channels_top_level_messages_and_reply_counts() {
        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let mut app = app();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let s = screen(&term).join("\n");
        assert!(s.contains("general") && s.contains("random"), "channel list\n{s}");
        assert!(s.contains("Bob") && s.contains("Hello there, see docs"), "message with rendered markdown\n{s}");
        assert!(s.contains("1 reply · last"), "reply count\n{s}");
        assert!(!s.contains("a reply") && !s.contains("\nreply"), "the reply itself is not listed\n{s}");
        assert!(s.contains("● live"), "connection state\n{s}");
        assert!(s.contains("Open thread") && s.contains("Switch pane"), "footer hints\n{s}");
        assert!(!s.contains("**"), "markdown is rendered, not shown");
    }

    #[test]
    fn state_is_never_colour_alone() {
        // The colourless scheme leans on these: every state also has a glyph, a word or a number.
        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let mut app = app();
        app.handle(AppEvent::Unread { channel: "c2".into(), count: 3 });
        app.conn = Conn::Down("boom".into());
        term.draw(|f| draw(f, &mut app)).unwrap();
        let s = screen(&term).join("\n");
        assert!(s.contains("(3)") && s.contains("3 unread"), "the unread count is written out, not only coloured\n{s}");
        assert!(s.contains("✕ offline, retrying (boom)"), "the connection state is a word, not only a colour\n{s}");
        assert!(s.contains("▌ #general"), "the selection is the bar glyph, not only a background\n{s}");
    }

    #[test]
    fn the_selected_message_is_one_band_across_the_whole_pane() {
        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let mut app = app();
        app.focus = Focus::Messages;
        term.draw(|f| draw(f, &mut app)).unwrap();
        let sel = crate::theme::get().sel_bg;
        // the message pane starts after the channel list; the last column is the scrollbar, not part of the band
        let (x0, x1) = (app.areas.msgs.x, app.areas.msgs.x + app.areas.msgs.width - 1);
        let buf = term.backend().buffer();
        let mut bands = 0;
        for y in 0..buf.area.height {
            if !(x0..x1).any(|x| buf[(x, y)].bg == sel) {
                continue;
            }
            bands += 1;
            for x in x0..x1 {
                assert_eq!(buf[(x, y)].bg, sel, "selected row {y} is not a full band: gap at x={x}");
            }
        }
        assert!(bands > 0, "no row was drawn as selected");
    }

    #[test]
    fn the_default_scheme_paints_its_background_everywhere_including_popups() {
        let th = crate::theme::get();
        let (bg, popup) = (th.bg.expect("default scheme paints a background"), th.popup_bg.expect("and a popup background"));
        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let mut app = app();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        // the cell after a double-width character (the 💬) belongs to that character and is never drawn on its own
        let on_bg = |x: u16, y: u16| (x > 0 && unicode_width::UnicodeWidthStr::width(buf[(x - 1, y)].symbol()) == 2) || buf[(x, y)].bg == bg || buf[(x, y)].bg == th.sel_bg || buf[(x, y)].bg == th.code_bg;
        let bad: Vec<_> = (0..24u16).flat_map(|y| (0..100u16).map(move |x| (x, y))).filter(|&(x, y)| !on_bg(x, y)).map(|(x, y)| (x, y, buf[(x, y)].symbol().to_owned(), buf[(x, y)].bg)).take(8).collect();
        assert!(bad.is_empty(), "cells left to the terminal background: {bad:?}");
        app.overlay = Overlay::Help;
        term.draw(|f| draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        assert_eq!(buf[(50, 12)].bg, popup, "the help popup is not transparent");
    }

    #[test]
    fn the_input_grows_with_its_text_and_the_geometry_is_reported() {
        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let mut app = app();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let one_line = app.areas.input.height;
        app.editor.insert_str("a\nb\nc\nd");
        term.draw(|f| draw(f, &mut app)).unwrap();
        assert!(app.areas.input.height > one_line);
        assert_eq!(app.areas.channels.x, 0);
        assert!(app.areas.msgs.width > 50 && app.view_h == app.areas.msgs.height as usize);
    }

    #[test]
    fn panels_and_typing_are_drawn() {
        let mut term = Terminal::new(TestBackend::new(110, 40)).unwrap();
        let mut app = app();
        app.overlay = Overlay::Keys;
        term.draw(|f| draw(f, &mut app)).unwrap();
        assert!(screen(&term).join("\n").contains("Keys  (Esc closes)"));
        app.overlay = Overlay::Help;
        term.draw(|f| draw(f, &mut app)).unwrap();
        assert!(screen(&term).join("\n").contains("/r N text"));
        assert!(screen(&term).join("\n").contains("/update"), "the help screen lists /update");
        app.overlay = Overlay::None;
        app.typing.insert(("c1".into(), "bob".into()), std::time::Instant::now());
        term.draw(|f| draw(f, &mut app)).unwrap();
        assert!(screen(&term).join("\n").contains("Bob is typing"));
    }

    #[test]
    fn the_status_line_shows_the_upgrade_advice() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut term = Terminal::new(TestBackend::new(110, 30)).unwrap();
        let mut app = app();
        app.update_source = crate::update::InstallSource::Homebrew;
        app.focus = Focus::Input;
        for c in "/update".chars() {
            app.handle(AppEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        app.handle(AppEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        term.draw(|f| draw(f, &mut app)).unwrap();
        let s = screen(&term).join("\n");
        assert!(s.contains("upgrade with: brew update && brew upgrade buzzr"), "the advice is on the status line\n{s}");
    }

    #[test]
    fn a_known_newer_release_is_shown_in_the_status_line() {
        let mut term = Terminal::new(TestBackend::new(110, 30)).unwrap();
        let mut app = app();
        app.handle(AppEvent::Update("0.2.0".into()));
        term.draw(|f| draw(f, &mut app)).unwrap();
        let s = screen(&term).join("\n");
        assert!(s.contains("buzzr 0.2.0 available · /update"), "the update notice is on the status line\n{s}");
    }

    #[test]
    fn your_own_message_offers_edit_and_delete_and_the_prompts_show() {
        let mut term = Terminal::new(TestBackend::new(110, 30)).unwrap();
        let mut app = app();
        let mut mine = ev("m3", 1500, "just mine", &[&["h", "c1"]]);
        mine.pubkey = "me".into();
        app.handle(AppEvent::Live(Live::Event(mine)));
        app.layout(70);
        app.handle(AppEvent::Key(ratatui::crossterm::event::KeyEvent::new(ratatui::crossterm::event::KeyCode::End, ratatui::crossterm::event::KeyModifiers::NONE)));
        term.draw(|f| draw(f, &mut app)).unwrap();
        let s = screen(&term).join("\n");
        assert!(s.contains("Edit") && s.contains("Delete") && s.contains("Search"), "footer offers e and d on your own message\n{s}");
        app.handle(AppEvent::Key(ratatui::crossterm::event::KeyEvent::new(ratatui::crossterm::event::KeyCode::Char('d'), ratatui::crossterm::event::KeyModifiers::NONE)));
        term.draw(|f| draw(f, &mut app)).unwrap();
        assert!(screen(&term).join("\n").contains("Delete this message? press y"));
        app.handle(AppEvent::Key(ratatui::crossterm::event::KeyEvent::new(ratatui::crossterm::event::KeyCode::Char('n'), ratatui::crossterm::event::KeyModifiers::NONE)));
        app.handle(AppEvent::Key(ratatui::crossterm::event::KeyEvent::new(ratatui::crossterm::event::KeyCode::Char('e'), ratatui::crossterm::event::KeyModifiers::NONE)));
        term.draw(|f| draw(f, &mut app)).unwrap();
        let s = screen(&term).join("\n");
        assert!(s.contains("edit message") && s.contains("just mine") && s.contains("Cancel edit"), "{s}");
    }

    #[test]
    fn the_channel_bar_follows_the_cursor_and_returns_when_the_list_loses_focus() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let key = |code| AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE));
        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let mut app = app();
        // the channel list has no focus: the bar marks the channel on screen
        term.draw(|f| draw(f, &mut app)).unwrap();
        let s = screen(&term);
        assert!(s[2].starts_with("▌ #general"), "the bar is on the open channel\n{}", s.join("\n"));
        assert!(s[3].starts_with("  #random"), "the other channel has none\n{}", s.join("\n"));
        // (Messages -> Channels) then Down: the bar moves with the cursor
        app.handle(key(KeyCode::BackTab));
        app.handle(key(KeyCode::Down));
        term.draw(|f| draw(f, &mut app)).unwrap();
        let s = screen(&term);
        assert!(s[2].starts_with("  #general") && s[3].starts_with("▌ #random"), "the bar moved to the cursor\n{}", s.join("\n"));
        // Tab away without opening one: the bar is back on the channel on screen
        app.handle(key(KeyCode::Tab));
        term.draw(|f| draw(f, &mut app)).unwrap();
        let s = screen(&term);
        assert!(s[2].starts_with("▌ #general") && s[3].starts_with("  #random"), "the bar came back\n{}", s.join("\n"));
    }

    #[test]
    fn the_channel_list_shows_a_title_per_section() {
        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let mut app = App::new("me".into(), false, chrono_tz::UTC);
        let chan = |id: &str, name: &str, section| Channel { id: id.into(), name: name.into(), about: String::new(), created_at: 1, section, label: name.into(), is_private: false };
        use crate::model::Section::*;
        app.handle(AppEvent::Channels(Ok(vec![chan("d", "DM", Dms), chan("c", "general", Channels), chan("s", "chat", Solo)])));
        term.draw(|f| draw(f, &mut app)).unwrap();
        let s = screen(&term);
        let col: Vec<String> = s[1..7].iter().map(|l| l.chars().take(26).collect::<String>().trim().to_owned()).collect();
        assert_eq!(col[0], "Solo", "{}", s.join("\n"));
        assert!(col[1].ends_with("chat") && col[2] == "Channels" && col[3].ends_with("general"), "{col:?}");
        assert!(col[4] == "DMs" && col[5].ends_with("DM"), "{col:?}");
    }

    #[test]
    fn the_channel_list_marks_private_channels_and_leaves_dms_alone() {
        use crate::model::Section::*;
        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let mut app = App::new("me".into(), false, chrono_tz::UTC);
        let chan = |id: &str, name: &str, section, is_private| Channel { id: id.into(), name: name.into(), about: String::new(), created_at: 1, section, label: name.into(), is_private };
        app.handle(AppEvent::Channels(Ok(vec![chan("p", "general", Channels, false), chan("q", "dev-buzzr", Channels, true), chan("d", "DM", Dms, true)])));
        term.draw(|f| draw(f, &mut app)).unwrap();
        let s = screen(&term);
        let col: Vec<String> = s[1..8].iter().map(|l| l.chars().take(26).collect::<String>().trim().to_owned()).collect();
        assert!(col.iter().any(|r| r.ends_with("#general")), "an open channel is marked #\n{col:?}");
        assert!(col.iter().any(|r| r.ends_with("*dev-buzzr")), "a private channel is marked *\n{col:?}");
        assert!(col.iter().any(|r| r.ends_with("DM") && !r.contains('#') && !r.contains('*')), "a DM gets no marker\n{col:?}");
    }

    #[test]
    fn the_search_overlay_lists_channels_and_messages() {
        let mut term = Terminal::new(TestBackend::new(110, 30)).unwrap();
        let mut app = app();
        app.open_search("ran".into());
        let mut hit = ev("h1", 1200, "a random thought about boats", &[&["h", "c1"]]);
        hit.pubkey = "bob".into();
        app.search.seq = 1;
        app.handle(AppEvent::Search { seq: 1, result: Ok(vec![hit]) });
        term.draw(|f| draw(f, &mut app)).unwrap();
        let s = screen(&term).join("\n");
        assert!(s.contains("Search · Enter opens"), "{s}");
        assert!(s.contains("Search: ran"), "{s}");
        assert!(s.contains("# random") && s.contains("channel"), "channel match\n{s}");
        assert!(s.contains("general · Bob") && s.contains("a random thought about boats"), "message match\n{s}");
        assert!(s.contains("1 messages found") || s.contains("found"), "{s}");
    }

    #[test]
    fn it_survives_tiny_terminals() {
        for (w, h) in [(20, 6), (1, 1), (40, 3), (200, 5)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            let mut app = app();
            term.draw(|f| draw(f, &mut app)).unwrap();
        }
    }

    #[test]
    fn plugin_contributions_are_drawn_on_the_surfaces_they_target() {
        use crate::contrib::{Contributions, Mark, PluginEvent, Segment, Tone};
        let mut term = Terminal::new(TestBackend::new(110, 30)).unwrap();
        let mut app = app();
        app.handle(AppEvent::Plugin(PluginEvent::Ready {
            id: "demo".into(),
            name: "Demo".into(),
            contributions: Contributions {
                status: vec![Segment::new("2 left", Tone::Ok)],
                header: vec![Segment::new("DEMO", Tone::Accent)],
                channel_marks: vec![Mark { target: "c1".into(), text: "★".into(), tone: Tone::Warn }],
                message_marks: vec![Mark { target: "a".into(), text: "TODO".into(), tone: Tone::Accent }],
                ..Default::default()
            },
        }));
        term.draw(|f| draw(f, &mut app)).unwrap();
        let s = screen(&term).join("\n");
        assert!(s.contains("DEMO"), "header badge\n{s}");
        assert!(s.contains("2 left"), "status item\n{s}");
        assert!(s.contains('★'), "channel mark\n{s}");
        assert!(s.contains("TODO"), "message mark\n{s}");
    }

    #[test]
    fn a_plugin_panel_is_drawn_with_its_title_and_rows() {
        use crate::contrib::{Panel, PanelRow, PluginEvent, Tone};
        let mut term = Terminal::new(TestBackend::new(110, 30)).unwrap();
        let mut app = app();
        app.handle(AppEvent::Plugin(PluginEvent::Panel {
            plugin: "demo".into(),
            panel: Panel {
                id: "prs".into(),
                title: "Pull requests".into(),
                rows: vec![PanelRow { text: "#42 fix the anchor".into(), tone: Tone::Default, action: Some("open:42".into()) }],
            },
        }));
        term.draw(|f| draw(f, &mut app)).unwrap();
        let s = screen(&term).join("\n");
        assert!(s.contains("Pull requests") && s.contains("#42 fix the anchor"), "{s}");
        assert!(s.contains("Esc closes"), "the panel title bar says how to leave\n{s}");
    }
}
