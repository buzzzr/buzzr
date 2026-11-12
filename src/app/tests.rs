use super::*;
use crate::model::tests::ev;
use crate::relay::{Profile, Window};
use ratatui::crossterm::event::{KeyEvent, KeyModifiers};

const ME: &str = "me";
const BOB: &str = "bob";

fn chan(id: &str, name: &str) -> Channel {
    Channel { id: id.into(), name: name.into(), about: String::new(), created_at: 1, section: if name == "DM" { Section::Dms } else { Section::Channels }, label: name.into(), is_private: false }
}

fn msg(id: &str, at: u64, who: &str, content: &str, tags: &[&[&str]]) -> Ev {
    let mut e = ev(id, at, content, tags);
    e.pubkey = who.into();
    e
}

fn reply(id: &str, at: u64, who: &str, parent: &str) -> Ev {
    msg(id, at, who, "a reply", &[&["e", parent, "", "reply"]])
}

/// A `kind:20002` typing indicator: `tags` carry the channel (`h`) and, in a thread, the root (`e`, marked `reply`).
fn typing(id: &str, at: u64, who: &str, tags: &[&[&str]]) -> Ev {
    let mut e = msg(id, at, who, "", tags);
    e.kind = 20002;
    e
}

fn key(code: KeyCode) -> AppEvent {
    AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn press(app: &mut App, code: KeyCode) -> Vec<Cmd> {
    let cmds = app.handle(key(code));
    app.layout(78);
    cmds
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle(key(KeyCode::Char(c)));
    }
}

/// An app with channels "general" (open) and a DM, `n` top-level messages in general, 10 lines of screen.
fn app_with(n: usize) -> App {
    let mut app = App::new(ME.into(), false, chrono_tz::UTC);
    app.view_h = 10;
    app.areas = Areas { channels: Rect::new(0, 1, 20, 20), msgs: Rect::new(20, 1, 80, 10), input: Rect::new(20, 12, 80, 3) };
    app.handle(AppEvent::Channels(Ok(vec![chan("dm1", "DM"), chan("c1", "general"), chan("c2", "random")])));
    let events: Vec<Ev> = (0..n).map(|i| msg(&format!("m{i:03}"), 1000 + i as u64, BOB, &format!("message {i}"), &[])).collect();
    app.handle(AppEvent::Window { channel: "c1".into(), cursor: None, result: Ok(Window { events, has_more: false, next_cursor: None }) });
    // A fresh app starts with the keyboard on the channel list; nearly every case here is about the message list or the
    // input, so the fixture moves the cursor into the message list the way a user does (Tab) before handing the app over.
    // The startup focus itself is checked in `opening_the_first_real_channel_loads_it_and_the_dm_comes_last`.
    app.handle(key(KeyCode::Tab));
    app.layout(78);
    app
}

fn max_scroll(app: &mut App) -> usize {
    let vh = app.view_h;
    app.layout(78).max_scroll(vh)
}

fn top_id(app: &mut App) -> Option<String> {
    let scroll = app.state.scroll;
    let l = app.layout(78);
    l.at_line(scroll).map(|(i, _)| l.items[i].id.clone())
}

fn selected(app: &App) -> String {
    app.state.selected.clone().unwrap_or_default()
}

#[test]
fn the_arrow_keys_move_the_focus_between_the_message_list_and_the_input() {
    let mut app = app_with(30);
    // scroll away from the bottom first, so landing on the newest message has to move the view too
    for _ in 0..12 {
        press(&mut app, KeyCode::Up);
    }
    press(&mut app, KeyCode::Tab); // messages -> input
    assert_eq!(app.focus, Focus::Input);
    press(&mut app, KeyCode::Up);
    assert_eq!(app.focus, Focus::Messages, "Up in an empty input leaves it");
    assert_eq!(selected(&app), "m029", "landing on the newest message");
    let max = max_scroll(&mut app);
    assert_eq!(app.state.scroll, max, "with the bottom in view");
    press(&mut app, KeyCode::Down);
    assert_eq!(app.focus, Focus::Input, "Down on the newest message goes back to the input");
}

#[test]
fn down_above_the_newest_message_still_selects_and_up_keeps_a_draft() {
    let mut app = app_with(3);
    press(&mut app, KeyCode::Up);
    assert_eq!(selected(&app), "m001");
    press(&mut app, KeyCode::Down);
    assert_eq!((app.focus, selected(&app)), (Focus::Messages, "m002".to_string()), "Down selects the next message, not the input");
    press(&mut app, KeyCode::Tab); // messages -> input
    type_text(&mut app, "a draft");
    press(&mut app, KeyCode::Up);
    assert_eq!(app.focus, Focus::Input, "Up still belongs to the input while something is typed");
    assert_eq!(app.editor.text(), "a draft");
}

#[test]
fn opening_the_first_real_channel_loads_it_and_the_dm_comes_last() {
    let mut app = App::new(ME.into(), false, chrono_tz::UTC);
    let cmds = app.handle(AppEvent::Channels(Ok(vec![chan("dm1", "DM"), chan("c2", "random"), chan("c1", "general")])));
    let order: Vec<&str> = app.channels.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(order, ["general", "random", "DM"]);
    assert_eq!(app.channel().map(|c| c.id.as_str()), Some("c1"), "the first channel is opened");
    assert!(cmds.contains(&Cmd::LoadWindow { channel: "c1".into(), cursor: None }));
    assert!(cmds.contains(&Cmd::Subscribe(vec!["c1".into(), "c2".into(), "dm1".into()])));
    assert!(cmds.contains(&Cmd::LoadMembers { channel: "dm1".into() }), "DMs need their members to be labelled");
    assert_eq!(app.focus, Focus::Channels, "a fresh start has the keyboard on the channel list");
    assert_eq!(app.channel_bar(), Some(0), "and the bar is on the channel that was opened");
}

#[test]
fn a_dm_is_labelled_with_the_other_person() {
    let mut app = app_with(1);
    app.handle(AppEvent::Members { channel: "dm1".into(), result: Ok(vec![(ME.into(), "member".into()), ("alice".into(), "member".into())]) });
    app.handle(AppEvent::Names(Ok([("alice".to_string(), "Alice".to_string())].into())));
    assert_eq!(app.channels.iter().find(|c| c.id == "dm1").unwrap().label, "Alice");
}

#[test]
fn a_loaded_window_opens_at_the_bottom_with_the_last_message_selected() {
    let mut app = app_with(30);
    assert_eq!(selected(&app), "m029");
    let max = max_scroll(&mut app);
    assert_eq!(app.state.scroll, max);
    assert_eq!(app.shown_ids().len(), 30);
}

#[test]
fn names_of_unknown_authors_are_requested_once() {
    let mut app = App::new(ME.into(), false, chrono_tz::UTC);
    app.handle(AppEvent::Channels(Ok(vec![chan("c1", "general")])));
    let w = Window { events: vec![msg("a", 1, BOB, "hi", &[]), msg("b", 2, "carol", "yo", &[])], has_more: false, next_cursor: None };
    let cmds = app.handle(AppEvent::Window { channel: "c1".into(), cursor: None, result: Ok(w) });
    let asked = cmds.iter().find_map(|c| if let Cmd::LoadNames(v) = c { Some(v.clone()) } else { None }).expect("names requested");
    assert_eq!(asked.len(), 2);
    let w = Window { events: vec![msg("c", 3, BOB, "again", &[])], has_more: false, next_cursor: None };
    let cmds = app.handle(AppEvent::Window { channel: "c1".into(), cursor: None, result: Ok(w) });
    assert!(!cmds.iter().any(|c| matches!(c, Cmd::LoadNames(_))), "already asked");
}

#[test]
fn replies_are_hidden_in_the_channel_and_counted_on_their_root() {
    let mut app = app_with(3);
    app.handle(AppEvent::Live(Live::Event(reply("r1", 2000, "alice", "m001"))));
    app.layout(78);
    assert_eq!(app.shown_ids(), ["m000", "m001", "m002"], "the reply stays in its thread");
    let text: String = app.layout(78).items[1].lines[0].text();
    assert!(text.contains("💬 1 reply"), "{text}");
}

#[test]
fn enter_opens_the_thread_and_escape_returns_to_the_same_place() {
    let mut app = app_with(30);
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Up);
    let (sel, scroll) = (selected(&app), app.state.scroll);
    assert_eq!(sel, "m027");
    let cmds = press(&mut app, KeyCode::Enter);
    assert_eq!(cmds, vec![Cmd::LoadThread { channel: "c1".into(), root: "m027".into() }]);
    assert_eq!(app.view, View::Thread("m027".into()));
    app.handle(AppEvent::Thread { channel: "c1".into(), root: "m027".into(), result: Ok(vec![reply("t1", 3000, "alice", "m027"), reply("t2", 3001, ME, "m027")]) });
    app.layout(78);
    assert_eq!(app.shown_ids(), ["m027", "t1", "t2"]);
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.view, View::Channel);
    assert_eq!((selected(&app), app.state.scroll), (sel, scroll), "back where we were");
}

#[test]
fn escape_walks_back_from_the_input_to_the_message_list_and_then_the_channels() {
    let mut app = app_with(3);
    press(&mut app, KeyCode::Tab); // (Messages -> Input)
    assert_eq!(app.focus, Focus::Input);
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.focus, Focus::Messages, "the input hands the keyboard back to the message list");
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.focus, Focus::Channels, "with nothing left to close, the message list hands it to the channel list");
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.focus, Focus::Channels, "the channel list is the end of the line");
}

#[test]
fn escape_leaves_a_thread_before_the_message_list_releases_the_keyboard() {
    let mut app = app_with(3);
    press(&mut app, KeyCode::Enter); // into the thread of the selected message
    assert!(app.in_thread());
    press(&mut app, KeyCode::Esc);
    assert!(!app.in_thread(), "the first Escape leaves the thread");
    assert_eq!(app.focus, Focus::Messages, "and stays in the message list, so there is still something to back out of");
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.focus, Focus::Channels, "the next Escape reaches the channel list");
}

#[test]
fn a_new_message_follows_at_the_bottom_but_not_when_scrolled_up() {
    let mut app = app_with(30);
    let at_bottom = max_scroll(&mut app);
    assert_eq!(app.state.scroll, at_bottom);
    app.handle(AppEvent::Live(Live::Event(msg("new1", 5000, BOB, "fresh", &[&["h", "c1"]]))));
    let max = max_scroll(&mut app);
    assert_eq!(app.state.scroll, max, "following the bottom");
    // scroll up by hand, then another message arrives
    for _ in 0..12 {
        press(&mut app, KeyCode::Up);
    }
    let scroll = app.state.scroll;
    let sel = selected(&app);
    app.handle(AppEvent::Live(Live::Event(msg("new2", 5001, BOB, "later", &[&["h", "c1"]]))));
    app.layout(78);
    assert_eq!((app.state.scroll, selected(&app)), (scroll, sel), "the reader is left alone");
}

#[test]
fn older_messages_load_above_without_moving_the_reader() {
    let mut app = app_with(30);
    let store = app.stores.get_mut("c1").unwrap();
    store.has_more = true;
    store.next_cursor = Some((1000, "m000".into()));
    for _ in 0..15 {
        press(&mut app, KeyCode::Up);
    }
    let sel = selected(&app);
    let top_before = top_id(&mut app);
    let older: Vec<Ev> = (0..10).map(|i| msg(&format!("o{i:03}"), 500 + i as u64, BOB, "older", &[])).collect();
    app.handle(AppEvent::Window { channel: "c1".into(), cursor: Some((1000, "m000".into())), result: Ok(Window { events: older, has_more: false, next_cursor: None }) });
    let top_after = top_id(&mut app);
    assert_eq!(top_before, top_after, "the message at the top of the screen did not change");
    assert_eq!(selected(&app), sel);
    assert_eq!(app.shown_ids().len(), 40);
}

#[test]
fn up_at_the_top_asks_for_older_messages_once() {
    let mut app = app_with(5);
    let store = app.stores.get_mut("c1").unwrap();
    store.has_more = true;
    store.next_cursor = Some((1000, "m000".into()));
    let mut asked = Vec::new();
    for _ in 0..12 {
        asked.extend(press(&mut app, KeyCode::Up));
    }
    assert_eq!(asked, vec![Cmd::LoadWindow { channel: "c1".into(), cursor: Some((1000, "m000".into())) }], "one request, however often Up is pressed");
}

#[test]
fn nothing_older_is_reported_not_requested() {
    let mut app = app_with(3);
    for _ in 0..5 {
        press(&mut app, KeyCode::Up);
    }
    assert_eq!(press(&mut app, KeyCode::Up), vec![]);
    assert_eq!(app.flash_text(), Some("no older messages"));
}

#[test]
fn typing_a_message_sends_it_to_the_channel_with_mentions_resolved() {
    let mut app = app_with(3);
    app.handle(AppEvent::Members { channel: "c1".into(), result: Ok(vec![(ME.into(), "owner".into()), (BOB.into(), "member".into()), ("robot".into(), "bot".into())]) });
    app.handle(AppEvent::Names(Ok([(BOB.to_string(), "Bob".to_string()), ("robot".to_string(), "robot".to_string())].into())));
    press(&mut app, KeyCode::Tab); // messages -> input
    assert_eq!(app.focus, Focus::Input);
    type_text(&mut app, "hello @robot and @bobby, not @Bob_x");
    let cmds = press(&mut app, KeyCode::Enter);
    assert_eq!(cmds, vec![Cmd::Send { channel: "c1".into(), text: "hello @robot and @bobby, not @Bob_x".into(), root: None, mentions: vec!["robot".into()] }]);
    assert!(app.editor.is_empty());
}

#[test]
fn shift_alt_enter_and_ctrl_j_add_lines_enter_sends_them() {
    let mut app = app_with(1);
    press(&mut app, KeyCode::Tab);
    type_text(&mut app, "one");
    app.handle(AppEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)));
    type_text(&mut app, "two");
    // Alt+Enter is what the footer offers where the terminal cannot report Shift+Enter.
    app.handle(AppEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)));
    type_text(&mut app, "three");
    app.handle(AppEvent::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL)));
    type_text(&mut app, "four");
    assert_eq!(app.editor.text(), "one\ntwo\nthree\nfour");
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(matches!(&cmds[0], Cmd::Send { text, .. } if text == "one\ntwo\nthree\nfour"));
}

#[test]
fn alt_backspace_deletes_the_word_before_the_cursor() {
    let mut app = app_with(1);
    press(&mut app, KeyCode::Tab);
    type_text(&mut app, "one two three");
    let alt_back = |app: &mut App| app.handle(AppEvent::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT)));
    alt_back(&mut app);
    assert_eq!(app.editor.text(), "one two ");
    app.typing_sent = None; // the signal is rate-limited; a word delete announces it like a plain one
    let cmds = alt_back(&mut app);
    assert_eq!(app.editor.text(), "one ");
    assert_eq!(cmds, vec![Cmd::Typing { channel: "c1".into(), root: None }], "deleting a word is typing too");
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(matches!(&cmds[0], Cmd::Send { text, .. } if text == "one"));
}

#[test]
fn replying_from_the_list_goes_into_the_thread_and_escape_cancels() {
    let mut app = app_with(5);
    app.handle(AppEvent::Live(Live::Event(reply("r1", 2000, "alice", "m004"))));
    app.layout(78);
    press(&mut app, KeyCode::Char('r'));
    assert_eq!((app.focus, app.reply_target.as_deref()), (Focus::Input, Some("m004")));
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.reply_target, None);
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.focus, Focus::Messages);
    press(&mut app, KeyCode::Char('r'));
    type_text(&mut app, "thanks");
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(matches!(&cmds[0], Cmd::Send { root: Some(r), text, .. } if r == "m004" && text == "thanks"));
}

#[test]
fn in_a_thread_plain_messages_are_replies() {
    let mut app = app_with(5);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Tab);
    type_text(&mut app, "in thread");
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(matches!(&cmds[0], Cmd::Send { root: Some(r), .. } if r == "m004"));
}

#[test]
fn read_only_never_sends() {
    let mut app = app_with(2);
    app.read_only = true;
    press(&mut app, KeyCode::Tab);
    type_text(&mut app, "hello");
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert!(app.flash_text().unwrap().contains("read-only"));
}

#[test]
fn slash_commands() {
    let mut app = app_with(5);
    press(&mut app, KeyCode::Tab);
    type_text(&mut app, "/copy 2");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.osc.len(), 1);
    let b64 = app.osc[0].trim_start_matches("\x1b]52;c;").trim_end_matches('\x07');
    let decoded = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
    assert_eq!(String::from_utf8(decoded).unwrap(), "message 1");

    type_text(&mut app, "/t 3");
    let cmds = press(&mut app, KeyCode::Enter);
    assert_eq!(cmds, vec![Cmd::LoadThread { channel: "c1".into(), root: "m002".into() }]);
    app.focus = Focus::Input;
    type_text(&mut app, "/c");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.view, View::Channel);

    type_text(&mut app, "/r 1 quick answer");
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(matches!(&cmds[0], Cmd::Send { root: Some(r), text, .. } if r == "m000" && text == "quick answer"));

    type_text(&mut app, "/bogus");
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert!(app.flash_text().unwrap().contains("unknown command"));
    type_text(&mut app, "/q");
    press(&mut app, KeyCode::Enter);
    assert!(app.quit);
}

#[test]
fn escape_closes_a_panel_before_anything_else() {
    let mut app = app_with(5);
    press(&mut app, KeyCode::Enter); // into a thread
    app.handle(key(KeyCode::F(1)));
    assert_eq!(app.overlay, Overlay::Keys);
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.overlay, Overlay::None);
    assert!(app.in_thread(), "the first Escape only closed the panel");
    press(&mut app, KeyCode::Esc);
    assert!(!app.in_thread());
    app.handle(key(KeyCode::F(2)));
    assert_eq!(app.overlay, Overlay::Help);
    app.handle(key(KeyCode::F(2)));
    assert_eq!(app.overlay, Overlay::None);
}

#[test]
fn unread_counts_other_channels_and_clear_when_opened() {
    let mut app = app_with(2);
    app.handle(AppEvent::Live(Live::Event(msg("x1", 9000, "alice", "hey", &[&["h", "c2"]]))));
    app.handle(AppEvent::Live(Live::Event(msg("x2", 9001, "alice", "hey", &[&["h", "c2"]]))));
    app.handle(AppEvent::Live(Live::Event(msg("x3", 9002, ME, "mine", &[&["h", "c2"]]))));
    assert_eq!(app.unread.get("c2"), Some(&2), "own messages do not count");
    assert_eq!(app.total_unread(), 2);
    let i = app.channels.iter().position(|c| c.id == "c2").unwrap();
    app.chan_cursor = i;
    app.focus = Focus::Channels;
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(app.unread.is_empty());
    assert!(!cmds.contains(&Cmd::LoadWindow { channel: "c2".into(), cursor: None }) || app.stores["c2"].head_loaded || true);
}

#[test]
fn a_thread_reply_does_not_raise_another_channels_badge() {
    let mut app = app_with(2); // c1 is on screen, c2 is not
    // a top-level message in c2 is unread on the channel
    app.handle(AppEvent::Live(Live::Event(msg("x1", 9000, "alice", "hey", &[&["h", "c2"]]))));
    assert_eq!(app.unread.get("c2"), Some(&1));
    // a reply in one of c2's threads is unread on that thread's row, not on the channel badge: the channel timeline never
    // shows it, so opening the channel would not clear the badge, and it came back after every restart.
    let thread_reply = msg("r1", 9001, "alice", "a reply", &[&["h", "c2"], &["e", "root", "", "reply"]]);
    app.handle(AppEvent::Live(Live::Event(thread_reply)));
    assert_eq!(app.unread.get("c2"), Some(&1), "a thread reply is not a channel-timeline row");
    // a nested reply is not on the timeline either
    let nested = msg("r2", 9002, "alice", "nested", &[&["h", "c2"], &["e", "root", "", "root"], &["e", "root", "", "reply"]]);
    app.handle(AppEvent::Live(Live::Event(nested)));
    assert_eq!(app.unread.get("c2"), Some(&1), "a nested reply does not count");
    // a broadcast reply is listed on the timeline, so it does count
    let broadcast = msg("b1", 9003, "alice", "to the channel", &[&["h", "c2"], &["e", "root", "", "reply"], &["broadcast", "1"]]);
    app.handle(AppEvent::Live(Live::Event(broadcast)));
    assert_eq!(app.unread.get("c2"), Some(&2), "a broadcast reply is a timeline row");
}

/// An app with "general" (c1) and "random" (c2), a stored read position for c2 and 10 lines of screen. The stored position
/// makes the channel list ask the relay for c2's count, which the test then delivers by hand.
fn app_with_stored_read() -> App {
    let mut app = App::new(ME.into(), false, chrono_tz::UTC);
    app.view_h = 10;
    app.areas = Areas { channels: Rect::new(0, 1, 20, 20), msgs: Rect::new(20, 1, 80, 10), input: Rect::new(20, 12, 80, 3) };
    app.seed_read(ReadState { channels: [("c2".to_owned(), 500u64)].into_iter().collect(), threads: HashMap::new(), marks: HashMap::new() });
    app
}

fn channels_are(app: &mut App) -> Vec<Cmd> {
    let cmds = app.handle(AppEvent::Channels(Ok(vec![chan("c1", "general"), chan("c2", "random")])));
    app.layout(78);
    cmds
}

fn has_divider(app: &mut App) -> bool {
    let l = app.layout(78);
    l.items.iter().any(|r| r.lines.iter().any(|line| line.spans.iter().any(|s| s.content.contains("new messages"))))
}

/// The head line of a message row, where the reply count and the unread mark are drawn.
fn row_text(app: &mut App, id: &str) -> String {
    let l = app.layout(78);
    let i = l.index_of(id).unwrap_or_else(|| panic!("{id} is not in the list"));
    l.items[i].lines[0].text()
}

/// Open "random" the way a person does and feed it its first page, `n` messages newer than the stored position.
fn open_random_with(app: &mut App, n: u64) -> Vec<Cmd> {
    let i = app.channels.iter().position(|c| c.id == "c2").unwrap();
    app.chan_cursor = i;
    app.focus = Focus::Channels;
    let cmds = press(app, KeyCode::Enter);
    let events: Vec<Ev> = (0..n).map(|i| msg(&format!("n{i}"), 1000 + i, BOB, &format!("new {i}"), &[])).collect();
    app.handle(AppEvent::Window { channel: "c2".into(), cursor: None, result: Ok(Window { events, has_more: false, next_cursor: None }) });
    app.layout(78);
    cmds
}

#[test]
fn a_stored_position_asks_for_the_count_once_per_run() {
    let mut app = app_with_stored_read();
    let cmds = channels_are(&mut app);
    assert!(cmds.contains(&Cmd::LoadUnread { channel: "c2".into(), since: 501 }), "the stored position is asked from: {:?}", cmds);
    assert!(!cmds.iter().any(|c| matches!(c, Cmd::LoadUnread { channel, .. } if channel == "c1")), "a channel never read has no position to ask from");
    let cmds = channels_are(&mut app);
    assert!(!cmds.iter().any(|c| matches!(c, Cmd::LoadUnread { .. })), "a reload does not ask the same channel again");
}

#[test]
fn a_startup_count_never_lowers_a_live_badge_and_skips_the_open_channel() {
    let mut app = app_with(2);
    app.handle(AppEvent::Live(Live::Event(msg("x1", 9000, "alice", "hey", &[&["h", "c2"]]))));
    app.handle(AppEvent::Live(Live::Event(msg("x2", 9001, "alice", "hey", &[&["h", "c2"]]))));
    app.handle(AppEvent::Unread { channel: "c2".into(), count: 1 });
    assert_eq!(app.unread.get("c2"), Some(&2), "the live count stays the higher one");
    app.handle(AppEvent::Unread { channel: "c2".into(), count: 7 });
    assert_eq!(app.unread.get("c2"), Some(&7), "a bigger startup count is taken");
    app.handle(AppEvent::Unread { channel: "c1".into(), count: 4 });
    assert_eq!(app.unread.get("c1"), None, "the channel on screen is read as it is drawn");
}

#[test]
fn reopening_a_channel_draws_the_new_messages_line_where_the_last_visit_stopped() {
    let mut app = app_with_stored_read();
    channels_are(&mut app);
    app.handle(AppEvent::Unread { channel: "c2".into(), count: 2 });
    assert_eq!(app.unread.get("c2"), Some(&2));
    let cmds = open_random_with(&mut app, 3);
    assert!(cmds.contains(&Cmd::LoadWindow { channel: "c2".into(), cursor: None }), "the first visit loads the page");
    assert_eq!(app.unread.get("c2"), None, "opening the channel clears its badge");
    assert!(has_divider(&mut app), "the divider marks where the last visit stopped");
    // leaving and coming back once the channel has been read takes the divider away again
    app.focus = Focus::Channels;
    app.chan_cursor = 0;
    press(&mut app, KeyCode::Enter);
    app.focus = Focus::Channels;
    app.chan_cursor = 1;
    press(&mut app, KeyCode::Enter);
    assert!(!has_divider(&mut app), "a channel already read has no new messages line");
}

#[test]
fn u_marks_the_selected_message_unread_and_writes_the_mark() {
    let mut app = app_with(3);
    // Reading the open channel has moved the position past every message.
    assert_eq!(app.read.get("c1"), Some(&1002));
    assert_eq!(app.unread.get("c1"), None);
    // Select the middle message: that message is what becomes unread, not everything after it.
    app.state.selected = Some("m001".into());
    let cmds = press(&mut app, KeyCode::Char('u'));
    assert_eq!(app.unread.get("c1"), Some(&1), "only the marked message is unread");
    assert_eq!(app.read.get("c1"), Some(&1002), "a mark is not a position: the read frontier does not move");
    let saved = cmds.iter().find_map(|c| match c { Cmd::SaveRead(m) => Some(m.clone()), _ => None }).expect("marking unread writes the mark");
    assert_eq!(saved.marks.get("c1").map(|s| s.iter().cloned().collect::<Vec<_>>()), Some(vec!["m001".to_owned()]));
    assert_eq!(app.flash_text(), Some("marked #2 unread"));
    let l = app.layout(78);
    let i = l.index_of("m001").expect("the selected message is in the list");
    assert!(l.items[i].lines[0].spans.iter().any(|s| s.content.contains("new messages")), "the divider sits above the message it was marked from");
    let after = l.index_of("m002").expect("the newer message is in the list");
    assert!(!l.items[after].lines[0].spans.iter().any(|s| s.content.contains("new messages")), "and not above the one after it");
}

#[test]
fn u_on_your_own_newest_message_marks_it_unread() {
    let mut app = app_with(2);
    // A live message of your own in the open channel is read as it arrives, and no frontier rule can make it unread again.
    app.handle(AppEvent::Live(Live::Event(msg("mine", 2000, ME, "mine", &[&["h", "c1"]]))));
    app.state.selected = Some("mine".into());
    let cmds = press(&mut app, KeyCode::Char('u'));
    assert!(cmds.iter().any(|c| matches!(c, Cmd::SaveRead(_))), "marking your own message writes the mark");
    assert_eq!(app.unread.get("c1"), Some(&1), "a message you wrote is unread when you ask for it");
    assert!(has_divider(&mut app), "the divider marks it");
}

#[test]
fn a_mark_stands_while_reading_on_and_comes_back_after_a_restart() {
    let mut app = app_with(3);
    app.state.selected = Some("m001".into());
    let saved = press(&mut app, KeyCode::Char('u')).iter().find_map(|c| match c { Cmd::SaveRead(m) => Some(m.clone()), _ => None }).expect("the mark is written");
    // Reading on — a message arriving in the open channel reads the channel through — leaves the mark alone.
    app.handle(AppEvent::Live(Live::Event(msg("later", 3000, "alice", "later", &[&["h", "c1"]]))));
    assert_eq!(app.unread.get("c1"), Some(&1), "reading on does not clear what was marked");
    // A fresh app seeded with what was written has the badge and the divider again. (The real restart seeds the state before
    // the first draw; the fixture has already drawn once, so it redraws.)
    let mut restarted = app_with(3);
    restarted.seed_read(saved);
    restarted.content_changed();
    assert_eq!(restarted.unread.get("c1"), Some(&1), "the mark is a badge again on the next start");
    assert!(has_divider(&mut restarted), "and the divider is above it");
}

#[test]
fn read_clears_the_marks_of_a_channel_and_their_badge() {
    let mut app = app_with(3);
    app.state.selected = Some("m001".into());
    press(&mut app, KeyCode::Char('u'));
    assert_eq!(app.unread.get("c1"), Some(&1));
    press(&mut app, KeyCode::Tab);
    type_text(&mut app, "/read");
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(app.marks.get("c1").is_none(), "the mark is cleared");
    assert_eq!(app.unread.get("c1"), None, "the badge goes with it");
    assert!(!has_divider(&mut app), "and so does the divider");
    let saved = cmds.iter().find_map(|c| match c { Cmd::SaveRead(m) => Some(m.clone()), _ => None }).expect("clearing the mark writes the state");
    assert!(saved.marks.is_empty(), "nothing is left marked: {saved:?}");
}

#[test]
fn read_all_clears_marks_in_every_channel() {
    let mut app = app_with(2);
    let mut state = ReadState::default();
    state.marks.insert("c1".to_owned(), ["m000".to_owned()].into_iter().collect());
    state.marks.insert("c2".to_owned(), ["x9".to_owned()].into_iter().collect());
    app.seed_read(state);
    assert_eq!(app.unread.get("c1"), Some(&1));
    assert_eq!(app.unread.get("c2"), Some(&1), "a channel that is not open carries its mark too");
    press(&mut app, KeyCode::Tab);
    type_text(&mut app, "/read all");
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(app.unread.is_empty(), "every badge is gone: {:?}", app.unread);
    let saved = cmds.iter().find_map(|c| match c { Cmd::SaveRead(m) => Some(m.clone()), _ => None }).expect("the state is written");
    assert!(saved.marks.is_empty());
}

#[test]
fn read_marks_the_channel_and_writes_the_position() {
    let mut app = app_with(2);
    // a message in the open channel is read as it arrives
    let cmds = app.handle(AppEvent::Live(Live::Event(msg("y1", 9500, "alice", "hey", &[&["h", "c1"]]))));
    let saved = cmds.iter().find_map(|c| match c { Cmd::SaveRead(m) => Some(m.clone()), _ => None }).expect("reading the open channel writes the position");
    assert_eq!(saved.channels.get("c1"), Some(&9500));
    // a second /read with nothing new says so instead of writing the same file again
    press(&mut app, KeyCode::Tab);
    type_text(&mut app, "/read");
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(!cmds.iter().any(|c| matches!(c, Cmd::SaveRead(_))), "nothing new: nothing to write");
    assert_eq!(app.flash_text(), Some("already read"));
}

#[test]
fn read_all_clears_every_badge_and_writes_every_position() {
    let mut app = app_with(2);
    app.handle(AppEvent::Live(Live::Event(msg("x1", 9000, "alice", "hey", &[&["h", "c2"]]))));
    assert_eq!(app.total_unread(), 1);
    press(&mut app, KeyCode::Tab);
    type_text(&mut app, "/read all");
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(app.unread.is_empty());
    let saved = cmds.iter().find_map(|c| match c { Cmd::SaveRead(m) => Some(m.clone()), _ => None }).expect("the positions are written");
    for id in ["c1", "c2", "dm1"] {
        assert!(saved.channels.get(id).map(|at| *at > 9000).unwrap_or(false), "{id} is marked read: {saved:?}");
    }
}

// ---- thread unread ----------------------------------------------------------------------------------------------------

/// A reply delivered live while we are looking at the channel: it lands in the store without reading any thread through.
fn live_reply(app: &mut App, id: &str, at: u64, who: &str, root: &str) {
    app.handle(AppEvent::Live(Live::Event(reply(id, at, who, root))));
}

/// A channel with a stored read position, so that both the channel and the threads in it have somewhere to count from.
fn seed_threads(app: &mut App, threads: &[(&str, u64)]) {
    let channels = [("c1".to_owned(), app.read.get("c1").copied().unwrap_or(0))].into_iter().collect();
    let threads = threads.iter().map(|(id, at)| ((*id).to_owned(), *at)).collect();
    app.seed_read(ReadState { channels, threads, marks: HashMap::new() });
}

#[test]
fn a_thread_with_replies_newer_than_its_read_position_is_marked_unread() {
    let mut app = app_with(2);
    seed_threads(&mut app, &[("m000", 1500)]);
    live_reply(&mut app, "r1", 2000, "alice", "m000");
    live_reply(&mut app, "r2", 3000, BOB, "m000");
    let text = row_text(&mut app, "m000");
    assert!(text.contains("💬 2 replies"), "{text}");
    assert!(text.contains("● 2 new"), "the replies are counted in the mark: {text}");
    assert!(!row_text(&mut app, "m001").contains("new"), "a message with no replies is not marked");
}

#[test]
fn a_thread_that_was_never_read_is_not_marked() {
    let mut app = app_with(2);
    // no stored thread position: unread starts at zero, so a reply on its own does not mark the thread
    live_reply(&mut app, "r1", 2000, "alice", "m000");
    let text = row_text(&mut app, "m000");
    assert!(text.contains("💬 1 reply"), "{text}");
    assert!(!text.contains("new"), "a thread with no stored position is not marked: {text}");
}

#[test]
fn a_thread_only_the_relay_knows_about_is_marked_new_rather_than_counted() {
    let mut app = app_with(2);
    seed_threads(&mut app, &[("m000", 1000)]);
    let summary = Ev {
        kind: crate::model::KIND_THREAD_SUMMARY,
        content: r#"{"reply_count":3,"descendant_count":3,"last_reply_at":5000,"participants":["alice"]}"#.into(),
        tags: vec![vec!["e".into(), "m000".into()], vec!["d".into(), "m000".into()]],
        ..msg("s1", 5001, "alice", "", &[])
    };
    app.handle(AppEvent::Live(Live::Event(summary)));
    let text = row_text(&mut app, "m000");
    assert!(text.contains("💬 3 replies"), "the relay's count is shown: {text}");
    assert!(text.contains("● new"), "with the replies not loaded the mark cannot count them: {text}");
    assert!(!text.contains("● 3 new"), "{text}");
}

#[test]
fn opening_a_thread_reads_it_and_clears_its_mark() {
    let mut app = app_with(2);
    seed_threads(&mut app, &[("m000", 1500)]);
    live_reply(&mut app, "r1", 2000, "alice", "m000");
    assert!(row_text(&mut app, "m000").contains("● 1 new"));
    app.state.selected = Some("m000".into());
    let cmds = press(&mut app, KeyCode::Enter);
    assert_eq!(app.view, View::Thread("m000".into()));
    let saved = cmds.iter().find_map(|c| match c { Cmd::SaveRead(m) => Some(m.clone()), _ => None }).expect("opening the thread writes the new position");
    assert_eq!(saved.threads.get("m000"), Some(&2000), "read through the newest reply");
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.view, View::Channel);
    assert!(!row_text(&mut app, "m000").contains("new"), "the mark is gone once the thread has been opened");
}

#[test]
fn reading_one_thread_leaves_the_others_marked() {
    let mut app = app_with(3);
    seed_threads(&mut app, &[("m000", 1000), ("m001", 1000)]);
    // a reply in each thread while we are looking at the channel: both are marked
    live_reply(&mut app, "a1", 2000, "alice", "m000");
    live_reply(&mut app, "b1", 2100, BOB, "m001");
    assert!(row_text(&mut app, "m000").contains("● 1 new"));
    assert!(row_text(&mut app, "m001").contains("● 1 new"));
    // reading m000 clears m000 and nothing else
    app.state.selected = Some("m000".into());
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Esc);
    assert!(!row_text(&mut app, "m000").contains("new"), "the thread we opened is read");
    assert!(row_text(&mut app, "m001").contains("● 1 new"), "the thread we did not open stays marked");
    // a reply that arrives in the thread on screen is read as it appears
    app.state.selected = Some("m001".into());
    press(&mut app, KeyCode::Enter);
    live_reply(&mut app, "b2", 4000, "alice", "m001");
    assert_eq!(app.thread_read.get("m001"), Some(&4000), "the open thread reads through the live reply");
    press(&mut app, KeyCode::Esc);
    assert!(!row_text(&mut app, "m001").contains("new"), "and it is not marked when we come back");
}

#[test]
fn read_clears_thread_marks_along_with_the_channel() {
    let mut app = app_with(2);
    seed_threads(&mut app, &[("m000", 1000)]);
    live_reply(&mut app, "r1", 2000, "alice", "m000");
    assert!(row_text(&mut app, "m000").contains("● 1 new"));
    press(&mut app, KeyCode::Tab);
    type_text(&mut app, "/read");
    let cmds = press(&mut app, KeyCode::Enter);
    let saved = cmds.iter().find_map(|c| match c { Cmd::SaveRead(m) => Some(m.clone()), _ => None }).expect("marking read writes the positions");
    assert!(saved.threads.get("m000").map(|at| *at > 2000).unwrap_or(false), "the thread position moved past its replies: {saved:?}");
    assert!(!row_text(&mut app, "m000").contains("new"), "the thread mark goes with the channel badge");
}

#[test]
fn read_all_clears_thread_marks_too() {
    let mut app = app_with(2);
    seed_threads(&mut app, &[("m000", 1000)]);
    live_reply(&mut app, "r1", 2000, "alice", "m000");
    press(&mut app, KeyCode::Tab);
    type_text(&mut app, "/read all");
    let cmds = press(&mut app, KeyCode::Enter);
    let saved = cmds.iter().find_map(|c| match c { Cmd::SaveRead(m) => Some(m.clone()), _ => None }).expect("the positions are written");
    assert!(saved.threads.get("m000").map(|at| *at > 9000).unwrap_or(false), "every thread is marked read: {saved:?}");
    assert!(!row_text(&mut app, "m000").contains("new"));
}

#[test]
fn thread_read_positions_round_trip_through_save_and_seed() {
    let mut app = app_with(2);
    seed_threads(&mut app, &[("m000", 1500)]);
    live_reply(&mut app, "r1", 2000, "alice", "m000");
    app.state.selected = Some("m000".into());
    let cmds = press(&mut app, KeyCode::Enter);
    let saved = cmds.iter().find_map(|c| match c { Cmd::SaveRead(m) => Some(m.clone()), _ => None }).expect("opening the thread writes the position");
    assert_eq!(saved.threads.get("m000"), Some(&2000));
    // what was written is what a later start reads back
    let mut restarted = app_with(2);
    restarted.seed_read(saved.clone());
    assert_eq!(restarted.thread_read.get("m000"), Some(&2000), "the thread position comes back");
    assert_eq!(restarted.read.get("c1"), saved.channels.get("c1"), "the channel position comes back with it");
}

#[test]
fn the_channel_bar_follows_the_cursor_and_returns_to_the_open_channel() {
    let mut app = app_with(1); // channels: general (open), random, DM
    assert_eq!((app.focus, app.channel_bar()), (Focus::Messages, Some(0)), "the bar starts on the channel on screen");
    press(&mut app, KeyCode::BackTab); // into the channel list (Messages -> Channels)
    assert_eq!(app.focus, Focus::Channels);
    press(&mut app, KeyCode::Down);
    assert_eq!((app.chan_cursor, app.channel_bar()), (1, Some(1)), "the bar moves with the cursor");
    assert_eq!(app.channel().map(|c| c.id.as_str()), Some("c1"), "navigating does not switch channels");
    press(&mut app, KeyCode::Home);
    assert_eq!(app.channel_bar(), Some(0));
    press(&mut app, KeyCode::End);
    assert_eq!((app.chan_cursor, app.channel_bar()), (2, Some(2)));
    press(&mut app, KeyCode::Tab); // away without opening one (Channels -> Messages)
    assert_eq!((app.focus, app.channel_bar()), (Focus::Messages, Some(0)), "the bar comes back to the open channel");
    press(&mut app, KeyCode::Tab); // (Messages -> Input) it stays on the channel on screen
    assert_eq!((app.focus, app.channel_bar()), (Focus::Input, Some(0)));
    press(&mut app, KeyCode::Tab); // back into the list (Input -> Channels): the cursor starts there
    assert_eq!((app.focus, app.channel_bar()), (Focus::Channels, Some(0)));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert_eq!((app.channel().map(|c| c.id.as_str()), app.channel_bar()), (Some("c2"), Some(1)), "opening a channel keeps the bar on it");
}

#[test]
fn typing_indicators_show_and_a_message_clears_them() {
    let mut app = app_with(2);
    app.names.insert("robot".into(), "robot".into());
    app.handle(AppEvent::Live(Live::Event(typing("t1", 1, "robot", &[&["h", "c1"]]))));
    assert_eq!(app.typing_now(), ["robot"]);
    app.handle(AppEvent::Live(Live::Event(msg("a1", 9000, "robot", "done", &[&["h", "c1"]]))));
    assert!(app.typing_now().is_empty());
    app.handle(AppEvent::Live(Live::Event(typing("t2", 2, ME, &[&["h", "c1"]]))));
    assert!(app.typing_now().is_empty(), "our own typing is not shown");
}

#[test]
fn typing_is_shown_only_in_the_channel_or_thread_it_was_sent_in() {
    let mut app = app_with(3);
    app.names.insert(BOB.into(), "Bob".into());
    app.names.insert("alice".into(), "Alice".into());
    // Bob types in the channel; Alice types in the thread of the newest message (`m002`).
    app.handle(AppEvent::Live(Live::Event(typing("t1", 10, BOB, &[&["h", "c1"]]))));
    app.handle(AppEvent::Live(Live::Event(typing("t2", 11, "alice", &[&["h", "c1"], &["e", "m002", "", "reply"]]))));
    assert_eq!(app.typing_now(), ["Bob"], "the channel names the channel typist only");

    press(&mut app, KeyCode::Enter); // open the thread on the selected (newest) message
    assert_eq!(app.typing_now(), ["Alice"], "the thread names its own typist only");
}

#[test]
fn typing_in_one_thread_is_not_shown_in_another() {
    let mut app = app_with(4);
    app.names.insert("alice".into(), "Alice".into());
    app.handle(AppEvent::Live(Live::Event(typing("t1", 10, "alice", &[&["h", "c1"], &["e", "m002", "", "reply"]]))));
    press(&mut app, KeyCode::Enter); // the thread of `m003`, the selected (newest) message
    assert!(app.typing_now().is_empty(), "another thread's typist is not shown here");
}

#[test]
fn a_message_clears_the_indicator_of_its_own_place_only() {
    let mut app = app_with(3);
    app.names.insert(BOB.into(), "Bob".into());
    app.names.insert("alice".into(), "Alice".into());
    app.handle(AppEvent::Live(Live::Event(typing("t1", 10, BOB, &[&["h", "c1"]]))));
    app.handle(AppEvent::Live(Live::Event(typing("t2", 11, "alice", &[&["h", "c1"], &["e", "m002", "", "reply"]]))));
    // Alice's reply lands in the thread: that indicator goes, the channel's stays.
    app.handle(AppEvent::Live(Live::Event(msg("r1", 12, "alice", "here it is", &[&["h", "c1"], &["e", "m002", "", "reply"]]))));
    assert_eq!(app.typing_now(), ["Bob"], "the channel typist is untouched");
    press(&mut app, KeyCode::Enter);
    assert!(app.typing_now().is_empty(), "the thread's typist was cleared by the reply");
}

#[test]
fn reconnecting_reloads_the_open_view() {
    let mut app = app_with(2);
    assert!(app.handle(AppEvent::Live(Live::Up)).is_empty(), "the first connection needs no catch-up");
    app.handle(AppEvent::Live(Live::Down("gone".into())));
    assert!(matches!(app.conn, Conn::Down(_)));
    let cmds = app.handle(AppEvent::Live(Live::Up));
    assert_eq!(app.conn, Conn::Live);
    assert!(cmds.contains(&Cmd::LoadWindow { channel: "c1".into(), cursor: None }));
}

#[test]
fn edits_and_deletes_arrive_live() {
    let mut app = app_with(3);
    let mut edit = msg("e1", 5000, BOB, "fixed text", &[&["h", "c1"], &["e", "m001"]]);
    edit.kind = 40003;
    app.handle(AppEvent::Live(Live::Event(edit)));
    let head = app.layout(78).items[1].lines.iter().map(Line::text).collect::<Vec<_>>().join("\n");
    assert!(head.contains("fixed text") && head.contains("(edited)"), "{head}");
    let mut del = msg("d1", 5001, BOB, "", &[&["e", "m002"]]);
    del.kind = 5;
    app.handle(AppEvent::Live(Live::Event(del))); // no h tag: routed through the message it points at
    app.layout(78);
    assert_eq!(app.shown_ids(), ["m000", "m001"]);
}

#[test]
fn a_reaction_and_its_removal_arrive_live() {
    let mut app = app_with(3);
    let mut react = msg("r1", 5000, BOB, "👍", &[&["e", "m001"]]);
    react.kind = 7;
    app.handle(AppEvent::Live(Live::Event(react)));
    let shown = |app: &mut App| app.layout(78).items[1].lines.iter().map(Line::text).collect::<Vec<_>>().join("\n");
    assert!(shown(&mut app).contains("👍"), "the new reaction is drawn under the message");
    // A removal has no h tag and points at the reaction event, not the message: it still reaches the right channel.
    let mut remove = msg("d1", 5001, BOB, "", &[&["e", "r1"]]);
    remove.kind = 5;
    app.handle(AppEvent::Live(Live::Event(remove)));
    assert!(!shown(&mut app).contains("👍"), "the removed reaction is gone");
}

#[test]
fn clicking_selects_opens_threads_and_follows_links() {
    let mut app = App::new(ME.into(), false, chrono_tz::UTC);
    app.view_h = 10;
    app.areas = Areas { channels: Rect::new(0, 1, 20, 20), msgs: Rect::new(20, 1, 80, 10), input: Rect::new(20, 12, 80, 3) };
    app.handle(AppEvent::Channels(Ok(vec![chan("c1", "general")])));
    let events = vec![msg("a", 1000, BOB, "plain message", &[]), msg("b", 1001, BOB, "see [the docs](https://example.com/docs) now", &[])];
    app.handle(AppEvent::Window { channel: "c1".into(), cursor: None, result: Ok(Window { events, has_more: false, next_cursor: None }) });
    app.layout(78);
    let click = |app: &mut App, col: u16, row: u16| {
        let cmds = app.handle(AppEvent::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 20 + col, row: 1 + row, modifiers: KeyModifiers::NONE }));
        app.layout(78);
        cmds
    };
    // message "a" occupies lines 0..2 (header, body) + separator; "b" starts at line 3: header 3, body 4
    let cmds = click(&mut app, 8, 4); // on the words "the docs" of message b
    std::env::set_var("SSH_CONNECTION", "1 2 3 4");
    let _ = cmds;
    app.osc.clear();
    let cmds = click(&mut app, 8, 4);
    assert!(cmds.is_empty() && app.view == View::Channel, "a click on a link does not open the thread: {:?}", app.view);
    assert_eq!(app.osc.len(), 1, "the link was copied");
    assert!(base64::engine::general_purpose::STANDARD.decode(app.osc[0].trim_start_matches("\x1b]52;c;").trim_end_matches('\x07')).map(|b| b == b"https://example.com/docs").unwrap_or(false));
    assert!(app.flash_text().unwrap().contains("paste it into your browser"));
    std::env::remove_var("SSH_CONNECTION");
    std::env::remove_var("SSH_TTY");
    let cmds = click(&mut app, 8, 4);
    assert_eq!(cmds, vec![Cmd::OpenUrl("https://example.com/docs".into())], "running locally the browser is opened too");
    assert!(app.follow_link("javascript:alert(1)").is_empty());
    assert!(app.flash_text().unwrap().contains("not following"));
    let cmds = click(&mut app, 3, 0); // the header of message a
    assert_eq!(cmds, vec![Cmd::LoadThread { channel: "c1".into(), root: "a".into() }]);
}

#[test]
fn clicking_a_channel_opens_it_and_the_wheel_scrolls() {
    let mut app = app_with(40);
    let click = |app: &mut App, col: u16, row: u16| app.handle(AppEvent::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: col, row, modifiers: KeyModifiers::NONE }));
    let cmds = click(&mut app, 3, 1 + 2); // the list is a "Channels" title, "general", "random"
    assert_eq!(app.channel().map(|c| c.id.as_str()), Some("c2"));
    assert!(cmds.contains(&Cmd::LoadWindow { channel: "c2".into(), cursor: None }));
    click(&mut app, 3, 1 + 1); // back to "general"
    app.layout(78);
    let before = app.state.scroll;
    app.handle(AppEvent::Mouse(MouseEvent { kind: MouseEventKind::ScrollUp, column: 30, row: 5, modifiers: KeyModifiers::NONE }));
    assert_eq!(app.state.scroll, before - 3);
}

#[test]
fn sending_jumps_to_the_bottom() {
    let mut app = app_with(30);
    for _ in 0..20 {
        press(&mut app, KeyCode::Up);
    }
    press(&mut app, KeyCode::Tab);
    type_text(&mut app, "hello");
    press(&mut app, KeyCode::Enter);
    app.handle(AppEvent::Live(Live::Event(msg("mine1", 9999, ME, "hello", &[&["h", "c1"]]))));
    let max = max_scroll(&mut app);
    assert_eq!(app.state.scroll, max);
}

// ---- typing signal, edit, delete, search, new channel ------------------------------------------------------------------------

fn own(id: &str, at: u64, content: &str) -> Ev {
    msg(id, at, ME, content, &[&["h", "c1"]])
}

fn with_own_message() -> App {
    let mut app = app_with(3);
    app.handle(AppEvent::Live(Live::Event(own("mine1", 5000, "my own words"))));
    app.layout(78);
    press(&mut app, KeyCode::End); // a new message does not steal the selection; End selects the last one
    assert_eq!(selected(&app), "mine1");
    app
}

#[test]
fn typing_is_announced_once_in_a_while_and_not_for_commands() {
    let mut app = app_with(2);
    press(&mut app, KeyCode::Tab);
    let first = app.handle(key(KeyCode::Char('h')));
    assert_eq!(first, vec![Cmd::Typing { channel: "c1".into(), root: None }]);
    assert!(app.handle(key(KeyCode::Char('i'))).is_empty(), "not again within three seconds");
    app.typing_sent = app.typing_sent.take().map(|(t, k)| (t - Duration::from_secs(4), k));
    assert_eq!(app.handle(key(KeyCode::Backspace)), vec![Cmd::Typing { channel: "c1".into(), root: None }], "deleting is typing too");
    press(&mut app, KeyCode::Enter); // sends "h"
    assert!(app.typing_sent.is_none(), "sending ends it, so the next message announces again");
    assert!(!app.handle(key(KeyCode::Char('x'))).is_empty());
    app.editor.clear();
    app.typing_sent = None;
    assert!(app.handle(key(KeyCode::Char('/'))).is_empty() || app.editor.text() == "/", "a slash command is not typing a message");
    assert!(app.handle(key(KeyCode::Char('m'))).is_empty(), "nor are the rest of its letters");
}

#[test]
fn typing_in_a_thread_names_the_thread_and_read_only_or_opt_out_stay_silent() {
    let mut app = app_with(3);
    press(&mut app, KeyCode::Enter); // thread of the newest message
    press(&mut app, KeyCode::Tab);
    let cmds = app.handle(key(KeyCode::Char('a')));
    assert_eq!(cmds, vec![Cmd::Typing { channel: "c1".into(), root: Some("m002".into()) }]);
    let mut quiet = app_with(1);
    quiet.send_typing = false;
    press(&mut quiet, KeyCode::Tab);
    assert!(quiet.handle(key(KeyCode::Char('a'))).is_empty());
    let mut ro = app_with(1);
    ro.read_only = true;
    press(&mut ro, KeyCode::Tab);
    assert!(ro.handle(key(KeyCode::Char('a'))).is_empty());
}

#[test]
fn editing_your_message_loads_it_and_saves_the_change() {
    let mut app = with_own_message();
    press(&mut app, KeyCode::Char('e'));
    assert_eq!((app.focus, app.editing.as_deref(), app.editor.text().as_str()), (Focus::Input, Some("mine1"), "my own words"));
    assert!(app.handle(key(KeyCode::Char('!'))).is_empty(), "no typing signal while editing");
    let cmds = press(&mut app, KeyCode::Enter);
    assert_eq!(cmds, vec![Cmd::Edit { channel: "c1".into(), id: "mine1".into(), text: "my own words!".into() }]);
    assert!(app.editing.is_none() && app.editor.is_empty());
}

#[test]
fn an_unchanged_or_cancelled_edit_sends_nothing() {
    let mut app = with_own_message();
    press(&mut app, KeyCode::Char('e'));
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert_eq!(app.flash_text(), Some("nothing changed"));
    press(&mut app, KeyCode::Esc); // focus returns to the message list
    press(&mut app, KeyCode::Char('e'));
    app.handle(key(KeyCode::Char('x')));
    press(&mut app, KeyCode::Esc);
    assert!(app.editing.is_none() && app.editor.is_empty());
    assert_eq!(app.flash_text(), Some("edit cancelled"));
}

#[test]
fn an_edit_cannot_be_emptied_and_only_your_own_messages_can_be_edited_or_deleted() {
    let mut app = with_own_message();
    press(&mut app, KeyCode::Char('e'));
    for _ in 0..20 {
        app.handle(key(KeyCode::Backspace));
    }
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert!(app.editing.is_some(), "still editing");
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Esc);
    // somebody else's message
    for _ in 0..1 {
        press(&mut app, KeyCode::Up);
    }
    assert_eq!(selected(&app), "m002");
    press(&mut app, KeyCode::Char('e'));
    assert!(app.flash_text().unwrap().contains("only edit your own"));
    press(&mut app, KeyCode::Char('d'));
    assert!(app.confirm_delete.is_none());
    assert!(app.flash_text().unwrap().contains("only delete your own"));
}

#[test]
fn delete_asks_first_and_only_y_deletes() {
    let mut app = with_own_message();
    press(&mut app, KeyCode::Char('d'));
    assert_eq!(app.confirm_delete.as_deref(), Some("mine1"));
    assert!(press(&mut app, KeyCode::Char('n')).is_empty());
    assert!(app.confirm_delete.is_none());
    assert_eq!(app.flash_text(), Some("not deleted"));
    press(&mut app, KeyCode::Char('d'));
    let cmds = press(&mut app, KeyCode::Char('y'));
    assert_eq!(cmds, vec![Cmd::Delete { channel: "c1".into(), id: "mine1".into() }]);
    // the relay's deletion arrives live and the message goes
    let mut del = msg("d1", 5001, ME, "", &[&["h", "c1"], &["e", "mine1"]]);
    del.kind = 9005;
    app.handle(AppEvent::Live(Live::Event(del)));
    app.layout(78);
    assert!(!app.shown_ids().contains(&"mine1".to_string()));
}

#[test]
fn edit_and_delete_by_number_and_not_in_read_only_mode() {
    let mut app = with_own_message();
    press(&mut app, KeyCode::Tab);
    type_text(&mut app, "/edit 4");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.editing.as_deref(), Some("mine1"));
    press(&mut app, KeyCode::Esc);
    type_text(&mut app, "/delete 4");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.confirm_delete.as_deref(), Some("mine1"));
    press(&mut app, KeyCode::Esc);
    let mut ro = with_own_message();
    ro.read_only = true;
    press(&mut ro, KeyCode::Char('e'));
    press(&mut ro, KeyCode::Char('d'));
    assert!(ro.editing.is_none() && ro.confirm_delete.is_none());
}

// ---- reactions --------------------------------------------------------------------------------------------------------------

#[test]
fn a_opens_the_picker_on_the_selected_message_and_enter_sends_the_emoji() {
    let mut app = app_with(3);
    press(&mut app, KeyCode::End); // the newest message
    let target = selected(&app);
    assert!(press(&mut app, KeyCode::Char('a')).is_empty());
    let picker = app.react.as_ref().expect("the picker is up");
    assert_eq!(picker.target, target);
    assert_eq!((picker.query.as_str(), picker.cursor), ("", 0));
    assert_eq!(app.focus, Focus::Messages, "the message keeps its selection underneath");
    assert!(app.editor.is_empty(), "the composer is untouched");
    // typing narrows the table by name; Enter sends the row on the cursor
    type_text(&mut app, "fire");
    assert_eq!(app.react_rows(), vec![ReactRow::Emoji("🔥".into())]);
    let cmds = press(&mut app, KeyCode::Enter);
    assert_eq!(cmds, vec![Cmd::React { target, emoji: "🔥".into(), remove: None }]);
    assert!(app.react.is_none(), "the picker closed");
    assert_eq!(app.flash_text(), Some("reacted 🔥"));
}

#[test]
fn the_picker_offers_your_reaction_first_and_up_enter_takes_it_back() {
    let mut app = app_with(3);
    press(&mut app, KeyCode::End);
    let target = selected(&app);
    let mut mine = msg("reaction1", 6000, ME, "👍", &[&["e", &target]]);
    mine.kind = 7;
    app.handle(AppEvent::Live(Live::Event(mine)));
    press(&mut app, KeyCode::Char('a'));
    let rows = app.react_rows();
    assert_eq!(rows.first(), Some(&ReactRow::Remove("reaction1".into())), "the first row takes your reaction back");
    assert_eq!(app.react.as_ref().unwrap().cursor, 1, "the cursor opens on the reaction you already have");
    assert!(press(&mut app, KeyCode::Enter).is_empty(), "Enter on your own reaction changes nothing");
    assert_eq!(app.flash_text(), Some("already reacted 👍"));
    // up onto the remove row, then Enter takes the reaction back
    press(&mut app, KeyCode::Char('a'));
    press(&mut app, KeyCode::Up);
    let cmds = press(&mut app, KeyCode::Enter);
    assert_eq!(cmds, vec![Cmd::React { target, emoji: String::new(), remove: Some("reaction1".into()) }]);
    assert_eq!(app.flash_text(), Some("reaction removed"));
}

#[test]
fn changing_the_emoji_undoes_the_one_it_replaces() {
    let mut app = app_with(3);
    press(&mut app, KeyCode::End);
    let target = selected(&app);
    let mut mine = msg("reaction1", 6000, ME, "👍", &[&["e", &target]]);
    mine.kind = 7;
    app.handle(AppEvent::Live(Live::Event(mine)));
    press(&mut app, KeyCode::Char('a'));
    assert!(press(&mut app, KeyCode::Enter).is_empty(), "the same emoji is already there");
    assert_eq!(app.flash_text(), Some("already reacted 👍"));
    // typing resets the cursor onto the first match, so Enter sends it rather than falling back to the remove row
    press(&mut app, KeyCode::Char('a'));
    type_text(&mut app, "party");
    let cmds = press(&mut app, KeyCode::Enter);
    assert_eq!(cmds, vec![Cmd::React { target, emoji: "🎉".into(), remove: Some("reaction1".into()) }]);
}

#[test]
fn the_picker_sends_an_emoji_the_table_does_not_have() {
    let mut app = app_with(3);
    press(&mut app, KeyCode::End);
    let target = selected(&app);
    press(&mut app, KeyCode::Char('a'));
    // no name matches, but an emoji in the query is sent as typed — the table is not a limit
    type_text(&mut app, "fire 🔥");
    assert_eq!(app.react_rows(), vec![ReactRow::Typed("🔥".into())]);
    let cmds = press(&mut app, KeyCode::Enter);
    assert_eq!(cmds, vec![Cmd::React { target, emoji: "🔥".into(), remove: None }]);
}

#[test]
fn the_picker_moves_with_the_arrows_and_esc_closes_it() {
    let mut app = app_with(3);
    press(&mut app, KeyCode::End);
    press(&mut app, KeyCode::Char('a'));
    assert_eq!(app.react.as_ref().unwrap().cursor, 0);
    press(&mut app, KeyCode::Down);
    assert_eq!(app.react.as_ref().unwrap().cursor, 1);
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Up); // already at the top: the cursor stays
    assert_eq!(app.react.as_ref().unwrap().cursor, 0);
    assert!(press(&mut app, KeyCode::Esc).is_empty());
    assert!(app.react.is_none());
    assert_eq!(app.focus, Focus::Messages, "Esc only closes the picker; the message stays selected");
}

#[test]
fn a_read_only_reaction_opens_nothing_and_typing_stays_in_the_picker() {
    let mut app = app_with(3);
    press(&mut app, KeyCode::End);
    press(&mut app, KeyCode::Char('a'));
    assert!(app.handle(key(KeyCode::Char('x'))).is_empty(), "typing in the picker is a search, not a message: no typing indicator");
    assert_eq!(app.react.as_ref().unwrap().query, "x");
    assert!(app.editor.is_empty(), "and the composer stays empty");
    let mut ro = app_with(3);
    ro.read_only = true;
    press(&mut ro, KeyCode::End);
    press(&mut ro, KeyCode::Char('a'));
    assert!(ro.react.is_none());
    assert_eq!(ro.flash_text(), Some("read-only mode: nothing is sent"));
}

#[test]
fn new_channels_are_created_from_a_command_and_opened_when_they_appear() {
    let mut app = app_with(1);
    press(&mut app, KeyCode::Tab);
    type_text(&mut app, "/new private team-room for the sailing crew");
    let cmds = press(&mut app, KeyCode::Enter);
    assert_eq!(cmds, vec![Cmd::CreateChannel { name: "team-room".into(), private: true, about: Some("for the sailing crew".into()) }]);
    type_text(&mut app, "/new ideas");
    let cmds = press(&mut app, KeyCode::Enter);
    assert_eq!(cmds, vec![Cmd::CreateChannel { name: "ideas".into(), private: false, about: None }]);
    type_text(&mut app, "/new");
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert!(app.flash_text().unwrap().starts_with("usage: /new"));
    // the relay accepted it: reload the list and open the new channel when it shows up
    let cmds = app.handle(AppEvent::Created(Ok("c9".into())));
    assert_eq!(cmds, vec![Cmd::LoadChannels]);
    let mut list: Vec<Channel> = app.channels.clone();
    list.push(chan("c9", "ideas"));
    app.handle(AppEvent::Channels(Ok(list)));
    assert_eq!(app.channel().map(|c| c.id.as_str()), Some("c9"));
    assert_eq!(app.focus, Focus::Input);
    assert!(matches!(app.handle(AppEvent::Created(Err("denied".into()))).as_slice(), []));
    assert!(app.flash_text().unwrap().contains("denied"));
    let mut ro = app_with(1);
    ro.read_only = true;
    assert!(ro.create_channel("x").is_empty());
}

#[test]
fn dm_opens_an_existing_conversation_and_starts_a_new_one() {
    const ALICE: &str = "alice";
    let mut app = app_with(1);
    // Alice already has a DM with us, Bob does not; both have names we have learned
    app.handle(AppEvent::Members { channel: "dm1".into(), result: Ok(vec![(ME.into(), "member".into()), (ALICE.into(), "member".into())]) });
    app.handle(AppEvent::Names(Ok([(ALICE.to_string(), "Alice".to_string()), (BOB.to_string(), "Bob".to_string())].into())));
    app.focus = Focus::Input;
    type_text(&mut app, "/dm Alice");
    let cmds = press(&mut app, KeyCode::Enter);
    assert_eq!(app.channel().map(|c| c.id.as_str()), Some("dm1"), "the DM that is already there is opened");
    assert_eq!(app.focus, Focus::Input);
    assert!(!cmds.iter().any(|c| matches!(c, Cmd::OpenDm { .. })), "nothing is sent for a conversation we have: {cmds:?}");

    // Bob has none: the relay is asked, and the conversation opens once the reloaded list has it
    type_text(&mut app, "/dm Bob");
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(matches!(cmds.as_slice(), [Cmd::OpenDm { peer }] if peer == BOB));
    assert_eq!(app.handle(AppEvent::DmOpened(Ok(()))), vec![Cmd::LoadChannels]);
    let mut list = app.channels.clone();
    list.push(chan("dm2", "DM"));
    app.handle(AppEvent::Channels(Ok(list)));
    app.handle(AppEvent::Members { channel: "dm2".into(), result: Ok(vec![(ME.into(), "member".into()), (BOB.into(), "member".into())]) });
    assert_eq!(app.channel().map(|c| c.id.as_str()), Some("dm2"), "the new DM is opened as soon as its members are known");
    assert_eq!(app.focus, Focus::Input);

    // a name nobody has, and an empty argument, are reported rather than guessed
    type_text(&mut app, "/dm nobody");
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert!(app.flash_text().unwrap().contains("nobody here is called nobody"));
    type_text(&mut app, "/dm");
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert!(app.flash_text().unwrap().starts_with("usage: /dm"));

    // read-only never sends, and never opens a conversation that does not exist yet
    let mut ro = app_with(1);
    ro.read_only = true;
    ro.handle(AppEvent::Names(Ok([(BOB.to_string(), "Bob".to_string())].into())));
    assert!(ro.open_dm(BOB).is_empty());
    assert!(ro.flash_text().unwrap().contains("read-only"));
}

#[test]
fn whois_shows_a_card_that_fills_in_when_the_profile_arrives() {
    let alice = "aa".repeat(32);
    let mut app = app_with(1);
    app.handle(AppEvent::Names(Ok([(alice.clone(), "Alice".to_string())].into())));
    app.focus = Focus::Input;
    type_text(&mut app, "/whois Alice");
    let cmds = press(&mut app, KeyCode::Enter);
    assert_eq!(cmds, vec![Cmd::LoadProfile(alice.clone())]);
    assert_eq!(app.overlay, Overlay::Whois);
    let card = app.whois.clone().unwrap();
    assert_eq!(card.name, "Alice");
    assert!(card.npub.starts_with("npub1") && card.loading, "the card opens at once with what we know");
    assert_eq!(crate::relay::as_pubkey(&card.npub).as_deref(), Some(alice.as_str()), "and shows the npub for the person asked about");
    // an answer for somebody else is dropped: the card is about the person it names
    app.handle(AppEvent::Profile { pubkey: BOB.into(), result: Ok(Profile { name: "Bob".into(), about: "hi".into() }) });
    assert_eq!(app.whois.as_ref().unwrap(), &card);
    // the right answer fills the card in
    app.handle(AppEvent::Profile { pubkey: card.pubkey.clone(), result: Ok(Profile { name: "Alice Smith".into(), about: "I sail".into() }) });
    let card = app.whois.as_ref().unwrap();
    assert_eq!((card.name.as_str(), card.about.as_deref(), card.loading), ("Alice Smith", Some("I sail"), false));
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.overlay, Overlay::None);
    // a profile that will not load closes the wait and says so
    type_text(&mut app, "/whois Alice");
    press(&mut app, KeyCode::Enter);
    app.handle(AppEvent::Profile { pubkey: alice.clone(), result: Err("offline".into()) });
    assert!(!app.whois.as_ref().unwrap().loading);
    assert!(app.flash_text().unwrap().contains("offline"));
    // Esc closes the card and ends the wait, so the next command goes to the input
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.overlay, Overlay::None);
    // a name nobody has is reported, not guessed
    app.focus = Focus::Input;
    type_text(&mut app, "/whois nobody");
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert_eq!(app.overlay, Overlay::None);
    assert!(app.flash_text().unwrap().contains("nobody here is called"));
}

#[test]
fn the_search_lists_people_and_enter_starts_a_dm() {
    let mut app = app_with(1);
    app.handle(AppEvent::Names(Ok([(BOB.to_string(), "Bob Roberts".to_string())].into())));
    app.handle(AppEvent::Key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL)));
    app.search.query.insert_str("roberts");
    assert!(matches!(app.search_rows().as_slice(), [Row::Person(pk)] if pk == BOB), "the name in the profile finds the person");
    app.search.asked = "roberts".into(); // it has been searched: Enter opens the row
    let cmds = app.handle(key(KeyCode::Enter));
    assert_eq!(app.overlay, Overlay::None);
    assert!(matches!(cmds.as_slice(), [Cmd::OpenDm { peer }] if peer == BOB));
}

#[test]
fn search_finds_channels_locally_and_messages_from_the_relay() {
    let mut app = app_with(3);
    app.handle(AppEvent::Key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL)));
    assert_eq!(app.overlay, Overlay::Search);
    for c in "rand".chars() {
        app.handle(key(KeyCode::Char(c)));
    }
    let rows = app.search_rows();
    assert!(matches!(rows.as_slice(), [Row::Channel(i)] if app.channels[*i].name == "random"), "the channel name matches at once");
    // not searched yet: Tick before the pause does nothing, after it asks the relay
    assert!(app.handle(AppEvent::Tick).is_empty());
    app.search.edited_at -= Duration::from_secs(1);
    let cmds = app.handle(AppEvent::Tick);
    assert_eq!(cmds, vec![Cmd::Search { query: "rand".into(), seq: 1 }]);
    assert!(app.handle(AppEvent::Tick).is_empty(), "the same query is not asked twice");
    // a stale answer is ignored, the right one is shown
    let hit = msg("h1", 100, BOB, "a random thought", &[&["h", "c1"]]);
    app.handle(AppEvent::Search { seq: 0, result: Ok(vec![hit.clone()]) });
    assert!(app.search.messages.is_empty());
    app.handle(AppEvent::Search { seq: 1, result: Ok(vec![hit, msg("x", 1, BOB, "elsewhere", &[&["h", "unknown-channel"]])]) });
    assert_eq!(app.search.messages.len(), 1, "hits from channels we do not know are dropped");
    assert_eq!(app.search_rows().len(), 2);
}

#[test]
fn opening_a_search_hit_goes_to_its_channel_and_selects_the_message() {
    let mut app = app_with(5);
    app.handle(AppEvent::Key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL)));
    app.search.query.insert_str("old words");
    let old = msg("old1", 10, BOB, "some old words from long ago", &[&["h", "c1"]]);
    app.search.seq = 1;
    app.handle(AppEvent::Search { seq: 1, result: Ok(vec![old]) });
    app.search.asked = "old words".into();
    let cmds = app.handle(key(KeyCode::Enter));
    assert_eq!(app.overlay, Overlay::None);
    app.layout(78);
    assert_eq!(selected(&app), "old1", "the hit is selected in its channel");
    assert!(cmds.is_empty() || cmds.iter().all(|c| !matches!(c, Cmd::LoadThread { .. })));
    // a reply opens its thread
    let reply_hit = msg("rp1", 11, BOB, "needle in a thread", &[&["h", "c1"], &["e", "m002", "", "reply"]]);
    app.handle(AppEvent::Key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL)));
    app.search.query.insert_str("needle");
    app.search.seq = 2;
    app.handle(AppEvent::Search { seq: 2, result: Ok(vec![reply_hit]) });
    app.search.asked = "needle".into();
    let cmds = app.handle(key(KeyCode::Enter));
    assert!(cmds.contains(&Cmd::LoadThread { channel: "c1".into(), root: "m002".into() }));
    app.layout(78);
    assert_eq!((app.view.clone(), selected(&app)), (View::Thread("m002".into()), "rp1".into()));
    // Esc closes the search, and the search command opens it with a query
    app.handle(AppEvent::Key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL)));
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.overlay, Overlay::None);
    app.focus = Focus::Input;
    type_text(&mut app, "/search sailing");
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(matches!(cmds.as_slice(), [Cmd::LoadRoster { .. }, Cmd::Search { query, .. }] if query == "sailing"), "{cmds:?}");
    assert_eq!(app.overlay, Overlay::Search);
}

// ---- inviting people --------------------------------------------------------------------------------------------------------

/// `(me, bob)` as the channel's members and Bob's display name, ready for a mention to be resolved against.
fn c1_members(app: &mut App) {
    app.handle(AppEvent::Members { channel: "c1".into(), result: Ok(vec![(ME.into(), "owner".into()), (BOB.into(), "member".into())]) });
}

#[test]
fn invite_adds_someone_to_the_channel_and_refreshes_the_member_list() {
    let alice = "aa".repeat(32);
    let mut app = app_with(1);
    c1_members(&mut app);
    app.handle(AppEvent::Names(Ok([(BOB.into(), "Bob".into()), (alice.clone(), "Alice".into())].into())));
    app.focus = Focus::Input;
    type_text(&mut app, "/invite Alice");
    let cmds = press(&mut app, KeyCode::Enter);
    assert_eq!(cmds, vec![Cmd::Invite { channel: "c1".into(), pubkey: alice.clone(), dm: false }]);
    assert_eq!(app.flash_text(), Some("inviting Alice to #general…"));
    // the relay answers: the cached member list is stale, so it is dropped and asked for again
    let cmds = app.handle(AppEvent::Invited { channel: "c1".into(), pubkey: alice.clone(), result: Ok(()) });
    assert_eq!(cmds, vec![Cmd::LoadMembers { channel: "c1".into() }]);
    assert!(!app.members.contains_key("c1"), "the old list is gone before the fresh one arrives");
    assert_eq!(app.flash_text(), Some("invited Alice"));
    // a refused invite is reported, not swallowed
    app.focus = Focus::Input;
    type_text(&mut app, "/invite Alice");
    press(&mut app, KeyCode::Enter);
    app.handle(AppEvent::Invited { channel: "c1".into(), pubkey: alice.clone(), result: Err("not a relay member".into()) });
    assert!(app.flash_text().unwrap().contains("could not invite: not a relay member"));
}

#[test]
fn invite_reports_a_member_an_unknown_name_and_read_only_mode() {
    let mut app = app_with(1);
    c1_members(&mut app);
    app.handle(AppEvent::Names(Ok([(BOB.into(), "Bob".into())].into())));
    app.focus = Focus::Input;
    type_text(&mut app, "/invite Bob");
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert_eq!(app.flash_text(), Some("Bob is already in #general"));
    type_text(&mut app, "/invite nobody");
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert!(app.flash_text().unwrap().contains("nobody here is called nobody"));
    type_text(&mut app, "/invite");
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert!(app.flash_text().unwrap().starts_with("usage: /invite"));
    let mut ro = app_with(1);
    ro.read_only = true;
    assert!(ro.invite(BOB).is_empty());
    assert!(ro.flash_text().unwrap().contains("read-only"));
}

#[test]
fn inviting_the_group_dm_uses_the_dm_event() {
    let alice = "aa".repeat(32);
    let mut app = app_with(1);
    let dm = app.channels.iter().position(|c| c.id == "dm1").unwrap();
    app.open_channel(dm);
    app.handle(AppEvent::Members { channel: "dm1".into(), result: Ok(vec![(ME.into(), "member".into()), (BOB.into(), "member".into())]) });
    app.handle(AppEvent::Names(Ok([(alice.clone(), "Alice".into())].into())));
    app.focus = Focus::Input;
    type_text(&mut app, "/invite Alice");
    let cmds = press(&mut app, KeyCode::Enter);
    assert_eq!(cmds, vec![Cmd::Invite { channel: "dm1".into(), pubkey: alice.clone(), dm: true }], "a DM adds to the conversation, not to a NIP-29 group");
}

#[test]
fn a_mention_of_someone_outside_the_channel_asks_first_and_y_invites_then_sends() {
    let alice = "aa".repeat(32);
    let mut app = app_with(3);
    c1_members(&mut app);
    app.handle(AppEvent::Names(Ok([(BOB.into(), "Bob".into()), (alice.clone(), "Alice".into())].into())));
    press(&mut app, KeyCode::Tab); // messages -> input
    type_text(&mut app, "hey @Alice can you look");
    assert!(press(&mut app, KeyCode::Enter).is_empty(), "nothing goes out until the question is answered");
    assert_eq!(app.invite_prompt().as_deref(), Some("Invite Alice to #general? y invites and sends, n just sends, Esc cancels"));
    assert!(app.editor.is_empty(), "the message is held, not left in the input");
    // `y`: the message goes out with Alice named in it, and the invite goes with it
    let cmds = press(&mut app, KeyCode::Char('y'));
    assert_eq!(
        cmds,
        vec![
            Cmd::Send { channel: "c1".into(), text: "hey @Alice can you look".into(), root: None, mentions: vec![alice.clone()] },
            Cmd::Invite { channel: "c1".into(), pubkey: alice.clone(), dm: false },
        ]
    );
    assert!(app.confirm_invite.is_none());
    assert_eq!(app.flash_text(), Some("inviting Alice to #general"));
}

#[test]
fn a_mention_of_someone_outside_the_channel_can_be_sent_without_inviting() {
    let alice = "aa".repeat(32);
    let mut app = app_with(3);
    c1_members(&mut app);
    app.handle(AppEvent::Names(Ok([(alice.clone(), "Alice".into())].into())));
    press(&mut app, KeyCode::Tab);
    type_text(&mut app, "@Alice ping");
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    let cmds = press(&mut app, KeyCode::Char('n'));
    assert_eq!(cmds, vec![Cmd::Send { channel: "c1".into(), text: "@Alice ping".into(), root: None, mentions: vec![] }], "no invite, and nobody to notify");
    assert_eq!(app.flash_text(), Some("sent without inviting Alice"));
}

#[test]
fn escape_keeps_a_held_message_in_the_input() {
    let alice = "aa".repeat(32);
    let mut app = app_with(3);
    c1_members(&mut app);
    app.handle(AppEvent::Names(Ok([(alice.clone(), "Alice".into())].into())));
    press(&mut app, KeyCode::Tab);
    type_text(&mut app, "@Alice, thoughts?");
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert!(press(&mut app, KeyCode::Esc).is_empty());
    assert!(app.confirm_invite.is_none());
    assert_eq!(app.editor.text(), "@Alice, thoughts?", "the text comes back so it can be changed");
    assert_eq!(app.focus, Focus::Input);
    assert_eq!(app.flash_text(), Some("not sent"));
}

#[test]
fn a_held_message_keeps_the_thread_it_was_written_in() {
    let alice = "aa".repeat(32);
    let mut app = app_with(3);
    app.handle(AppEvent::Members { channel: "c1".into(), result: Ok(vec![(ME.into(), "owner".into())]) });
    app.handle(AppEvent::Names(Ok([(alice.clone(), "Alice".into())].into())));
    app.open_thread("m000".into());
    press(&mut app, KeyCode::Tab); // messages -> input
    type_text(&mut app, "in here @Alice");
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert_eq!(app.confirm_invite.as_ref().unwrap().root.as_deref(), Some("m000"));
    let cmds = press(&mut app, KeyCode::Char('y'));
    assert!(matches!(cmds.as_slice(), [Cmd::Send { root: Some(r), .. }, Cmd::Invite { .. }] if r == "m000"));
}

#[test]
fn mentioning_a_member_never_asks_and_nothing_is_asked_before_the_member_list_loads() {
    let alice = "aa".repeat(32);
    // Bob is a member: naming him is an ordinary mention, even though Alice's name is known and she is not a member
    let mut app = app_with(3);
    c1_members(&mut app);
    app.handle(AppEvent::Names(Ok([(BOB.into(), "Bob".into()), (alice.clone(), "Alice".into())].into())));
    press(&mut app, KeyCode::Tab);
    type_text(&mut app, "hi @Bob and @Bobby");
    let cmds = press(&mut app, KeyCode::Enter);
    assert_eq!(cmds, vec![Cmd::Send { channel: "c1".into(), text: "hi @Bob and @Bobby".into(), root: None, mentions: vec![BOB.into()] }]);
    assert!(app.confirm_invite.is_none());
    // no member list yet: there is nothing to compare against, so the message simply goes
    let mut fresh = app_with(1);
    fresh.handle(AppEvent::Names(Ok([(alice.clone(), "Alice".into())].into())));
    fresh.focus = Focus::Input;
    type_text(&mut fresh, "@Alice hello");
    assert!(matches!(press(&mut fresh, KeyCode::Enter).as_slice(), [Cmd::Send { .. }]));
    assert!(fresh.confirm_invite.is_none());
}

#[test]
fn a_dm_never_asks_to_invite_a_mentioned_outsider() {
    let alice = "aa".repeat(32);
    let mut app = app_with(1);
    let dm = app.channels.iter().position(|c| c.id == "dm1").unwrap();
    app.open_channel(dm);
    app.handle(AppEvent::Members { channel: "dm1".into(), result: Ok(vec![(ME.into(), "member".into()), (BOB.into(), "member".into())]) });
    app.handle(AppEvent::Names(Ok([(alice.clone(), "Alice".into())].into())));
    press(&mut app, KeyCode::Tab);
    type_text(&mut app, "@Alice hi");
    assert!(matches!(press(&mut app, KeyCode::Enter).as_slice(), [Cmd::Send { .. }]), "a DM is a conversation, not a room to join");
    assert!(app.confirm_invite.is_none());
}

// ---- archiving a channel -----------------------------------------------------------------------------------------------------

#[test]
fn archiving_takes_the_channel_out_of_the_list_and_saves_the_set() {
    let mut app = app_with(1);
    app.set_focus(Focus::Channels);
    while app.channels[app.chan_cursor].id != "c1" {
        app.handle(key(KeyCode::Down));
    }
    let cmds = app.handle(key(KeyCode::Char('x')));
    assert!(app.is_archived("c1"));
    assert!(cmds.iter().any(|c| matches!(c, Cmd::SaveArchived(ids) if ids.contains("c1"))), "the set goes to disk: {cmds:?}");
    assert_eq!(side(&app), ["# Channels", "random", "# DMs", "DM"], "the row is gone");
    assert!(app.channels.iter().any(|c| c.id == "c1"), "but the channel is still loaded");
}

#[test]
fn a_section_whose_channels_are_all_archived_keeps_no_title() {
    let mut app = sectioned_app();
    app.seed_archived(["s1".to_owned(), "s2".to_owned()].into_iter().collect());
    assert_eq!(side(&app), ["# Channels", "general", "random", "team", "# DMs", "DM"], "the emptied Solo section is gone too");
}

#[test]
fn x_archives_the_channel_under_the_bar_not_the_open_one() {
    let mut app = app_with(1);
    assert_eq!(app.channel().map(|c| c.id.as_str()), Some("c1"), "general is open");
    app.set_focus(Focus::Channels);
    app.handle(key(KeyCode::End)); // the DM
    assert_eq!(app.channels[app.chan_cursor].id, "dm1");
    app.handle(key(KeyCode::Char('x')));
    assert!(app.is_archived("dm1"));
    assert!(!app.is_archived("c1"), "the open channel is untouched");
    assert_eq!(app.channel().map(|c| c.id.as_str()), Some("c1"));
    assert_eq!(app.channels[app.chan_cursor].id, "c2", "the cursor steps off the row it removed");
}

#[test]
fn archiving_the_open_channel_opens_the_next_one_the_list_still_draws() {
    let mut app = app_with(3);
    assert_eq!(app.channel().map(|c| c.id.as_str()), Some("c1"), "general is open");
    let cmds = app.archive(0);
    assert!(app.is_archived("c1"));
    assert_eq!(app.channel().map(|c| c.id.as_str()), Some("c2"), "random takes over");
    assert!(cmds.contains(&Cmd::LoadWindow { channel: "c2".into(), cursor: None }));
    assert_eq!(app.channel_bar(), Some(1));
}

#[test]
fn archiving_the_last_channel_leaves_nothing_open() {
    let mut app = App::new(ME.into(), false, chrono_tz::UTC);
    app.handle(AppEvent::Channels(Ok(vec![chan("c1", "general")])));
    assert_eq!(app.channel().map(|c| c.id.as_str()), Some("c1"));
    app.set_focus(Focus::Channels);
    let cmds = app.handle(key(KeyCode::Char('x')));
    assert!(app.is_archived("c1"));
    assert!(app.channel().is_none(), "nothing is left to show");
    assert_eq!(app.channel_bar(), None, "and no row to keep the bar on");
    assert!(cmds.iter().any(|c| matches!(c, Cmd::SaveArchived(_))));
}

#[test]
fn an_archived_channel_is_left_out_of_the_unread_total_but_keeps_its_count() {
    let mut app = app_with(2);
    app.handle(AppEvent::Live(Live::Event(msg("x1", 9000, "alice", "hey", &[&["h", "c2"]]))));
    assert_eq!(app.total_unread(), 1);
    app.seed_archived(["c2".to_owned()].into_iter().collect());
    assert_eq!(app.total_unread(), 0, "no row to show it on, so it is not in the total");
    assert_eq!(app.unread.get("c2"), Some(&1), "the count is kept for when it comes back");
}

#[test]
fn a_search_result_brings_an_archived_channel_back_and_opens_it() {
    let mut app = app_with(1);
    app.seed_archived(["c2".to_owned()].into_iter().collect());
    assert!(side(&app).iter().all(|r| r != "random"), "its row is gone");
    app.handle(AppEvent::Key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL)));
    for c in "rand".chars() {
        app.handle(key(KeyCode::Char(c)));
    }
    let rows = app.search_rows();
    assert!(matches!(rows.as_slice(), [Row::Channel(i)] if app.channels[*i].id == "c2"), "search still finds it");
    app.search.asked = "rand".into(); // it has been searched: Enter opens the row
    let cmds = app.handle(key(KeyCode::Enter));
    assert!(!app.is_archived("c2"), "Enter brings it back");
    assert_eq!(app.channel().map(|c| c.id.as_str()), Some("c2"), "and opens it");
    assert!(cmds.iter().any(|c| matches!(c, Cmd::SaveArchived(_))), "the change is written: {cmds:?}");
    assert!(side(&app).iter().any(|r| r == "random"), "its row is back");
}

#[test]
fn the_search_row_opens_on_enter_after_a_real_relay_search() {
    // The whole path the user takes: Ctrl+F, type a name, let the debounce ask the relay, then Enter. The
    // first Enter after a fresh query asks the relay instead of opening, so this checks the row opens once
    // the answer is in.
    let mut app = app_with(1);
    app.seed_archived(["c2".to_owned()].into_iter().collect());
    app.handle(AppEvent::Key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL)));
    for c in "rand".chars() {
        app.handle(key(KeyCode::Char(c)));
    }
    app.search.edited_at -= Duration::from_secs(1);
    let cmds = app.handle(AppEvent::Tick);
    assert!(matches!(cmds.as_slice(), [Cmd::Search { query, .. }] if query == "rand"), "the debounce asks: {cmds:?}");
    app.handle(AppEvent::Search { seq: 1, result: Ok(vec![]) });
    let cmds = app.handle(key(KeyCode::Enter));
    assert_eq!(app.overlay, Overlay::None, "Enter opens the row rather than asking again");
    assert!(!app.is_archived("c2"));
    assert_eq!(app.channel().map(|c| c.id.as_str()), Some("c2"));
    assert!(cmds.iter().any(|c| matches!(c, Cmd::SaveArchived(_))));
}

#[test]
fn the_archive_command_archives_the_channel_you_are_in() {
    let mut app = at_input(1);
    type_text(&mut app, "/archive");
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(app.is_archived("c1"), "the channel on screen is archived");
    assert!(cmds.iter().any(|c| matches!(c, Cmd::SaveArchived(_))));
}

#[test]
fn archive_with_no_channel_open_says_what_to_do() {
    let mut app = App::new(ME.into(), false, chrono_tz::UTC);
    app.focus = Focus::Input;
    type_text(&mut app, "/archive");
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(cmds.is_empty());
    assert!(app.flash_text().unwrap().contains("open the channel you want to archive first"));
}

#[test]
fn a_fresh_start_opens_the_first_channel_that_is_not_archived() {
    let mut app = App::new(ME.into(), false, chrono_tz::UTC);
    app.seed_archived(["c1".to_owned()].into_iter().collect());
    app.handle(AppEvent::Channels(Ok(vec![chan("c1", "general"), chan("c2", "random")])));
    assert_eq!(app.channel().map(|c| c.id.as_str()), Some("c2"), "the archived first channel is skipped");
    assert_eq!(app.chan_cursor, 1, "and the cursor never rests on an archived row");
    assert_eq!(app.focus, Focus::Channels);
}

// ---- plugins ------------------------------------------------------------------------------------------------------------

fn ready(id: &str, name: &str, cmd: &str) -> AppEvent {
    AppEvent::Plugin(PluginEvent::Ready {
        id: id.into(),
        name: name.into(),
        contributions: contrib::Contributions {
            commands: vec![contrib::PluginCommand { id: cmd.into(), title: cmd.to_uppercase(), usage: format!("/{cmd} [args]") }],
            status: vec![contrib::Segment::new("2 left", contrib::Tone::Ok)],
            header: vec![contrib::Segment::new("T", contrib::Tone::Dim)],
            ..Default::default()
        },
    })
}

#[test]
fn a_ready_plugin_registers_its_contributions_and_leaving_removes_them() {
    let mut app = app_with(3);
    app.handle(ready("todo", "TODO", "todo"));
    assert_eq!(app.plugins.command("todo").map(|(p, c)| (p, c.id.as_str())), Some(("todo", "todo")));
    assert_eq!(app.plugins.status()[0].text, "2 left");
    assert_eq!(app.plugins.header()[0].text, "T");

    app.handle(AppEvent::Plugin(PluginEvent::Gone { id: "todo".into(), why: "the process exited".into() }));
    assert!(app.plugins.command("todo").is_none(), "a gone plugin keeps nothing");
    assert!(app.plugins.status().is_empty());
    assert!(app.flash_text().unwrap().contains("stopped"));
}

#[test]
fn a_plugin_command_in_the_input_becomes_a_plugin_invoke() {
    let mut app = app_with(3);
    app.handle(ready("github", "GitHub", "prs"));
    press(&mut app, KeyCode::Tab); // into the input
    type_text(&mut app, "/prs open 42");
    let cmds = press(&mut app, KeyCode::Enter);
    assert_eq!(cmds, vec![Cmd::Plugin(PluginCmd::Invoke { plugin: "github".into(), command: "prs".into(), args: "open 42".into() })]);

    app.focus = Focus::Input;
    type_text(&mut app, "/nope");
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert!(app.flash_text().unwrap().contains("unknown command"));
}

#[test]
fn a_draft_only_lands_when_the_input_is_free_and_never_in_read_only() {
    let mut app = app_with(2);
    app.handle(AppEvent::Plugin(PluginEvent::Draft { plugin: "todo".into(), text: "a draft".into() }));
    assert_eq!(app.editor.text(), "a draft");
    assert_eq!(app.focus, Focus::Input);

    // the input is no longer free: the next draft is dropped rather than overwriting what the user typed
    app.handle(AppEvent::Plugin(PluginEvent::Draft { plugin: "todo".into(), text: "second".into() }));
    assert_eq!(app.editor.text(), "a draft");
    assert!(app.flash_text().unwrap().contains("already writing"));

    let mut ro = app_with(2);
    ro.read_only = true;
    ro.handle(AppEvent::Plugin(PluginEvent::Draft { plugin: "todo".into(), text: "x".into() }));
    assert!(ro.editor.is_empty());
    assert!(ro.flash_text().unwrap().contains("read-only"));
}

#[test]
fn a_plugin_may_only_send_in_the_open_channel_and_never_in_read_only() {
    let mut app = app_with(3); // channel c1 is open, c2 is not
    let cmds = app.handle(AppEvent::Plugin(PluginEvent::Send { plugin: "bot".into(), channel: Some("c2".into()), text: "hi".into() }));
    assert!(cmds.is_empty(), "another channel is refused");
    assert!(app.flash_text().unwrap().contains("another channel"));

    let cmds = app.handle(AppEvent::Plugin(PluginEvent::Send { plugin: "bot".into(), channel: Some("c1".into()), text: "hi".into() }));
    assert!(matches!(cmds.as_slice(), [Cmd::Send { channel, text, .. }] if channel == "c1" && text == "hi"));
    let cmds = app.handle(AppEvent::Plugin(PluginEvent::Send { plugin: "bot".into(), channel: None, text: "again".into() }));
    assert!(matches!(cmds.as_slice(), [Cmd::Send { channel, .. }] if channel == "c1"), "no channel means the open one");

    app.read_only = true;
    assert!(app.handle(AppEvent::Plugin(PluginEvent::Send { plugin: "bot".into(), channel: None, text: "no".into() })).is_empty());
    assert!(app.flash_text().unwrap().contains("read-only"));
}

#[test]
fn the_plugins_overlay_lists_plugins_and_refreshes_while_it_is_open() {
    let mut app = app_with(2);
    app.handle(AppEvent::Plugin(PluginEvent::Meta(vec![PluginMeta {
        id: "todo".into(),
        name: "TODO".into(),
        state: PluginState::Running,
        detail: String::new(),
        permissions: vec!["read:visible".into()],
        commands: 1,
    }])));
    app.focus = Focus::Input;
    type_text(&mut app, "/plugins");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.overlay, Overlay::Panel);
    let panel = app.panel.as_ref().unwrap();
    assert!(panel.plugin.is_empty(), "the built-in list has no plugin process behind it");
    assert!(panel.rows.iter().any(|r| r.text.contains("todo") && r.text.contains("running")));

    // a state change while it is open is reflected
    app.handle(AppEvent::Plugin(PluginEvent::Meta(vec![PluginMeta {
        id: "todo".into(),
        name: "TODO".into(),
        state: PluginState::Failed,
        detail: "boom".into(),
        permissions: vec![],
        commands: 0,
    }])));
    assert!(app.panel.as_ref().unwrap().rows.iter().any(|r| r.text.contains("failed") && r.text.contains("boom")));

    press(&mut app, KeyCode::Esc);
    assert_eq!(app.overlay, Overlay::None);
    assert!(app.panel.is_none());
}

#[test]
fn a_plugin_panel_is_navigated_and_enter_sends_the_row_action() {
    let mut app = app_with(2);
    app.handle(ready("github", "GitHub", "prs"));
    app.handle(AppEvent::Plugin(PluginEvent::Panel {
        plugin: "github".into(),
        panel: contrib::Panel {
            id: "prs".into(),
            title: "Pull requests".into(),
            rows: vec![
                PanelRow { text: "row one".into(), tone: contrib::Tone::Default, action: None },
                PanelRow { text: "row two".into(), tone: contrib::Tone::Default, action: Some("open:42".into()) },
            ],
        },
    }));
    assert_eq!(app.overlay, Overlay::Panel);
    assert_eq!(app.panel.as_ref().unwrap().plugin, "github");
    press(&mut app, KeyCode::Down);
    assert_eq!(app.panel.as_ref().unwrap().cursor, 1);
    let cmds = press(&mut app, KeyCode::Enter);
    assert_eq!(
        cmds,
        vec![Cmd::Plugin(PluginCmd::PanelAction { plugin: "github".into(), id: "prs".into(), row: 1, value: Some("open:42".into()) })]
    );
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.overlay, Overlay::None);
    assert!(app.panel.is_none());
}

#[test]
fn the_projection_carries_only_what_is_on_screen() {
    let mut app = app_with(30);
    app.layout(78);
    let p = app.projection();
    assert_eq!(p.me, ME);
    assert_eq!(p.view, "channel");
    assert_eq!(p.channel.as_ref().map(|c| c.id.as_str()), Some("c1"));
    assert_eq!(p.connection, "connecting");
    assert_eq!(p.selected.as_deref(), Some("m029"));
    assert!(p.messages.len() <= app.view_h, "only the rows on screen: {} vs {}", p.messages.len(), app.view_h);
    assert!(p.messages.iter().all(|m| m.text.starts_with("message ")), "only this channel's messages");
    assert_eq!(p.messages.iter().filter(|m| m.selected).count(), 1);
}

#[test]
fn update_names_the_command_for_how_this_copy_was_installed() {
    let mut app = app_with(1);
    app.focus = Focus::Input;

    // Nothing is known about this copy, so it points at the release page.
    type_text(&mut app, "/update");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.flash_text(), Some("install the newest release: https://github.com/buzzzr/buzzr/releases/latest"));

    // A Homebrew install gets Homebrew's own command, never a self-replacement.
    app.update_source = InstallSource::Homebrew;
    type_text(&mut app, "/update");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.flash_text(), Some("upgrade with: brew update && brew upgrade buzzr"));

    // And a checkout build is told there is nothing to upgrade.
    app.update_source = InstallSource::Dev;
    type_text(&mut app, "/update");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.flash_text(), Some("this is a working copy; rebuild it from your checkout"));
}

#[test]
fn a_newer_release_is_kept_for_the_status_line() {
    let mut app = app_with(1);
    assert_eq!(app.latest, None);
    assert_eq!(app.update_hint(), None);

    app.handle(AppEvent::Update("0.2.0".into()));
    assert_eq!(app.latest.as_deref(), Some("0.2.0"));
    assert_eq!(app.update_hint().as_deref(), Some("buzzr 0.2.0 available · /update"));
}

fn chan_in(id: &str, name: &str, section: Section) -> Channel {
    Channel { section, ..chan(id, name) }
}

fn sectioned_app() -> App {
    let mut app = App::new(ME.into(), false, chrono_tz::UTC);
    app.handle(AppEvent::Channels(Ok(vec![
        chan("dm1", "DM"),
        chan_in("s1", "Plan the week", Section::Solo),
        chan("c2", "random"),
        chan_in("g1", "team", Section::Channels),
        chan("c1", "general"),
        chan_in("s2", "Another chat", Section::Solo),
    ])));
    app
}

/// The list as drawn, titles and channels: "# Title" for a title, the label for a channel.
fn side(app: &App) -> Vec<String> {
    app.sidebar_rows()
        .into_iter()
        .map(|r| match r {
            SideRow::Title(s) => format!("# {}", s.title()),
            SideRow::Channel(i) => app.channels[i].label.clone(),
        })
        .collect()
}

#[test]
fn the_list_is_split_into_groups_solo_channels_and_dms() {
    let app = sectioned_app();
    assert_eq!(side(&app), ["# Solo", "Another chat", "Plan the week", "# Channels", "general", "random", "team", "# DMs", "DM"]);
}

#[test]
fn empty_sections_have_no_title() {
    let mut app = App::new(ME.into(), false, chrono_tz::UTC);
    app.handle(AppEvent::Channels(Ok(vec![chan("c1", "general"), chan_in("s1", "chat", Section::Solo)])));
    assert_eq!(side(&app), ["# Solo", "chat", "# Channels", "general"]);
    app.handle(AppEvent::Channels(Ok(vec![])));
    assert!(app.sidebar_rows().is_empty());
}

#[test]
fn the_first_channel_opens_on_start_and_the_cursor_skips_titles() {
    let mut app = sectioned_app();
    assert_eq!(app.channel().map(|c| c.id.as_str()), Some("s2"), "the first channel of the first section, not its title");
    app.set_focus(Focus::Channels);
    let mut seen = vec![app.channels[app.chan_cursor].label.clone()];
    for _ in 0..9 {
        app.handle(key(KeyCode::Down));
        seen.push(app.channels[app.chan_cursor].label.clone());
    }
    assert_eq!(seen, ["Another chat", "Plan the week", "general", "random", "team", "DM", "DM", "DM", "DM", "DM"]);
    app.handle(key(KeyCode::Home));
    assert_eq!(app.channels[app.chan_cursor].label, "Another chat");
}

#[test]
fn clicking_a_title_does_nothing_and_a_channel_row_opens_that_channel() {
    let mut app = sectioned_app();
    app.areas.channels = Rect::new(0, 1, 20, 20);
    let click = |app: &mut App, row: u16| app.handle(AppEvent::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 3, row, modifiers: KeyModifiers::NONE }));
    assert!(click(&mut app, 1 + 3).is_empty(), "row 3 is the Channels title");
    assert_eq!(app.channel().map(|c| c.id.as_str()), Some("s2"));
    click(&mut app, 1 + 2); // "Plan the week"
    assert_eq!(app.channel().map(|c| c.id.as_str()), Some("s1"));
    click(&mut app, 1 + 6); // "team", a private channel in the Channels section
    assert_eq!(app.channel().map(|c| c.id.as_str()), Some("g1"));
    click(&mut app, 1 + 8); // the DM
    assert_eq!(app.channel().map(|c| c.id.as_str()), Some("dm1"));
}

#[test]
fn a_long_list_scrolls_to_keep_the_bar_in_view_and_clicks_follow() {
    let mut app = sectioned_app();
    app.areas.channels = Rect::new(0, 1, 20, 4);
    let rows = app.sidebar_rows();
    assert_eq!(app.sidebar_scroll(&rows, 4), 0, "the bar on the first channel");
    app.set_focus(Focus::Channels);
    app.handle(key(KeyCode::End));
    // the DM is row 8 of 9, so rows 5..=8 are shown
    assert_eq!(app.sidebar_scroll(&rows, 4), 5);
    let click = |app: &mut App, row: u16| app.handle(AppEvent::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 3, row, modifiers: KeyModifiers::NONE }));
    click(&mut app, 1); // first shown row is row 5: "random"
    assert_eq!(app.channel().map(|c| c.label.as_str()), Some("random"));
}

#[test]
fn dm_labels_resolving_later_keep_dms_in_their_own_section_and_sorted() {
    let mut app = App::new(ME.into(), false, chrono_tz::UTC);
    app.handle(AppEvent::Channels(Ok(vec![chan("dm1", "DM"), chan("dm2", "DM"), chan("c1", "general"), chan_in("s1", "chat", Section::Solo)])));
    app.handle(AppEvent::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
    app.set_focus(Focus::Channels);
    app.handle(key(KeyCode::End));
    assert_eq!(app.channels[app.chan_cursor].id, "dm2");
    app.handle(AppEvent::Members { channel: "dm1".into(), result: Ok(vec![(ME.into(), "member".into()), ("zed".into(), "member".into())]) });
    app.handle(AppEvent::Members { channel: "dm2".into(), result: Ok(vec![(ME.into(), "member".into()), ("alice".into(), "member".into())]) });
    app.handle(AppEvent::Names(Ok([("zed".to_string(), "Zed".to_string()), ("alice".to_string(), "Alice".to_string())].into())));
    assert_eq!(side(&app), ["# Solo", "chat", "# Channels", "general", "# DMs", "Alice", "Zed"]);
    assert_eq!(app.channels[app.chan_cursor].id, "dm2", "the cursor stays on the channel it was on");
    assert_eq!(app.channel().map(|c| c.id.as_str()), Some("s1"), "so does the open channel");
}

#[test]
fn a_refresh_in_another_relay_order_keeps_the_open_channel_and_the_cursor() {
    let mut app = sectioned_app();
    app.set_focus(Focus::Channels);
    app.handle(key(KeyCode::End));
    let before = app.channels[app.chan_cursor].id.clone();
    let open = app.channel().map(|c| c.id.clone());
    let cmds = app.handle(AppEvent::Channels(Ok(vec![
        chan_in("s2", "Another chat", Section::Solo),
        chan("c1", "general"),
        chan("dm1", "DM"),
        chan_in("g1", "team", Section::Channels),
        chan("c2", "random"),
        chan_in("s1", "Plan the week", Section::Solo),
    ])));
    assert_eq!(app.channels[app.chan_cursor].id, before);
    assert_eq!(app.channel().map(|c| c.id.clone()), open);
    assert!(!cmds.iter().any(|c| matches!(c, Cmd::Subscribe(_))), "same channels, no resubscribe");
}

// ---- slash-command palette ---------------------------------------------------------------------------------------------------

/// The app with the keyboard in the input (channels -> messages -> input), where a `/command` is typed.
fn at_input(n: usize) -> App {
    let mut app = app_with(n);
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.focus, Focus::Input);
    app
}

#[test]
fn the_palette_is_up_while_a_command_word_is_typed_and_narrows_as_it_grows() {
    let mut app = at_input(3);
    assert!(!app.palette_up(), "no palette before the slash");
    assert!(app.command_rows().is_empty());
    type_text(&mut app, "/");
    assert!(app.palette_up(), "a lone slash lists the commands");
    assert_eq!(app.command_rows().len(), COMMANDS.len(), "every built-in command is listed");
    assert_eq!(app.command_rows().first().unwrap().name, COMMANDS[0].0, "in the table's order");
    type_text(&mut app, "he");
    assert_eq!(
        app.command_rows(),
        vec![CommandRow { name: "help".into(), desc: "the commands and how things work".into() }],
        "typing narrows to the names that still match"
    );
    // a space ends the command word: what follows is an argument, so the palette closes
    type_text(&mut app, " ");
    assert!(!app.palette_up());
    assert!(app.command_rows().is_empty());
}

#[test]
fn enter_runs_the_command_on_the_cursor_line() {
    let mut app = at_input(3);
    type_text(&mut app, "/sea");
    assert_eq!(app.command_rows().len(), 1, "only /search starts with \"sea\"");
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(matches!(cmds.as_slice(), [Cmd::LoadRoster { .. }]), "opening the search asks who is in the channels, and sends nothing else: {cmds:?}");
    assert_eq!(app.overlay, Overlay::Search);
    assert!(app.editor.is_empty(), "the input is cleared, as a submitted command is");
}

#[test]
fn the_arrows_move_the_palette_and_tab_fills_the_command_in() {
    let mut app = at_input(3);
    type_text(&mut app, "/");
    assert_eq!(app.cmd_cursor, 0);
    press(&mut app, KeyCode::Down);
    assert_eq!(app.cmd_cursor, 1, "Down moves the highlight, not the editor");
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Up);
    assert_eq!(app.cmd_cursor, 0, "already at the top: the highlight stays");
    // Tab fills the command on the cursor line into the input and lets an argument follow
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.editor.text(), format!("/{} ", COMMANDS[0].0));
    assert!(!app.palette_up(), "the trailing space ends the command word");
}

#[test]
fn a_changed_command_word_drops_the_highlight_back_to_the_first_row() {
    let mut app = at_input(3);
    type_text(&mut app, "/");
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    assert_eq!(app.cmd_cursor, 2);
    type_text(&mut app, "c");
    assert_eq!(app.cmd_cursor, 0, "a changed word drops the highlight back to the first row");
    assert!(app.command_rows().iter().all(|r| r.name.starts_with('c')));
}

#[test]
fn esc_closes_the_palette_and_keeps_the_command_in_the_input() {
    let mut app = at_input(3);
    type_text(&mut app, "/inv");
    assert!(app.palette_up(), "the palette is up over a command word");

    assert!(press(&mut app, KeyCode::Esc).is_empty(), "Esc closes the list; nothing is sent or opened");
    assert!(!app.palette_up(), "the palette is closed");
    assert_eq!(app.focus, Focus::Input, "the keyboard stays in the composer");
    assert_eq!(app.editor.text(), "/inv", "the half-written command stays in the input");

    type_text(&mut app, "ite");
    assert_eq!(app.editor.text(), "/invite");
    assert!(app.palette_up(), "typing on makes it a new command word, so the list comes back");

    press(&mut app, KeyCode::Esc);
    assert_eq!(app.focus, Focus::Input, "the list closed again; the composer still has the keyboard");
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.focus, Focus::Messages, "with the list already closed, Esc steps back to the messages");
    assert_eq!(app.editor.text(), "/invite", "and the command is still in the input");
}

#[test]
fn a_lone_slash_closed_with_esc_lists_again_after_the_next_one() {
    let mut app = at_input(3);
    type_text(&mut app, "/");
    assert!(app.palette_up());
    press(&mut app, KeyCode::Esc);
    assert!(!app.palette_up(), "Esc closes the palette on a lone slash too");
    // Enter settles what is typed (the empty command is reported as unknown) and clears the input, so the next `/`
    // is a new command word rather than the one the palette was closed on.
    press(&mut app, KeyCode::Enter);
    assert!(app.editor.is_empty(), "the input is cleared, as a submitted command is");
    type_text(&mut app, "/");
    assert!(app.palette_up(), "a fresh slash lists the commands again");
}

#[test]
fn the_palette_belongs_to_the_input_and_not_to_an_edit() {
    let mut app = at_input(3);
    type_text(&mut app, "/he");
    assert!(app.palette_up());
    press(&mut app, KeyCode::Esc); // the list closes, but the composer keeps the keyboard (see the Esc case above)
    assert_eq!(app.focus, Focus::Input);
    assert!(!app.palette_up());
    press(&mut app, KeyCode::Tab); // with the palette down, Tab switches panes again
    assert_eq!(app.focus, Focus::Channels);
    assert!(!app.palette_up(), "the palette is shown only while the input has the keyboard");
    press(&mut app, KeyCode::Tab); // channels -> messages
    press(&mut app, KeyCode::Tab); // messages -> input
    assert_eq!(app.focus, Focus::Input);
    assert!(app.palette_up(), "the draft is still a command word, and the list is back with the keyboard");

    // while editing a message a leading slash is message text, not a command
    let mut app = at_input(3);
    app.editing = Some("m001".into());
    type_text(&mut app, "/he");
    assert!(!app.palette_up());
    assert!(app.command_rows().is_empty());
}

#[test]
fn a_plugin_command_is_offered_in_the_palette_and_run_from_it() {
    let mut app = at_input(3);
    app.handle(ready("github", "GitHub", "prs"));
    type_text(&mut app, "/pr");
    let rows = app.command_rows();
    assert!(rows.iter().any(|r| r.name == "prs"), "a plugin command is listed: {rows:?}");
    let cmds = press(&mut app, KeyCode::Enter);
    assert_eq!(cmds, vec![Cmd::Plugin(PluginCmd::Invoke { plugin: "github".into(), command: "prs".into(), args: String::new() })]);
}

#[test]
fn a_typo_still_reports_an_unknown_command() {
    let mut app = at_input(3);
    type_text(&mut app, "/zzz");
    assert!(app.palette_up(), "the palette is up until the word is finished, even with no match");
    assert!(app.command_rows().is_empty());
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    assert_eq!(app.flash_text(), Some("unknown command /zzz (try /help)"));
    assert!(app.editor.is_empty());
}

#[test]
fn the_palette_names_every_command_that_is_handled() {
    // If a command is added to `command` and the palette table is forgotten, its line here fails. Each name runs on its own
    // app: `q` quits and `/help` opens an overlay.
    for (name, _) in COMMANDS {
        let mut app = at_input(1);
        app.command(name, "");
        let flash = app.flash_text().unwrap_or_default();
        assert!(!flash.starts_with("unknown command"), "/{name} is in the palette but not handled: {flash:?}");
    }
}

// ---- joining and leaving channels -------------------------------------------------------------------------------------------

/// A channel with its own visibility, for the join/leave cases.
fn chan_priv(id: &str, name: &str, private: bool) -> Channel {
    Channel { id: id.into(), name: name.into(), about: String::new(), created_at: 1, section: Section::Channels, label: name.into(), is_private: private }
}

/// A roster answer saying who is in each channel.
fn roster(entries: &[(&str, &[&str])]) -> AppEvent {
    AppEvent::Roster(Ok(entries.iter().map(|(id, members)| ((*id).to_string(), members.iter().map(|m| (*m).to_string()).collect())).collect()))
}

#[test]
fn a_channel_the_relay_has_not_answered_for_is_not_read_as_one_you_are_not_in() {
    let app = app_with(1);
    // Nothing is known about c1 or the DM: a channel the relay said nothing about keeps its composer rather than offering a join.
    assert!(!app.not_joined("c1"), "no roster answer is not a channel we are not in");
    assert!(!app.not_joined("dm1"));
}

#[test]
fn a_channel_the_relay_says_you_are_not_in_is_one_to_join_and_a_dm_never_is() {
    let mut app = app_with(1);
    app.handle(roster(&[("c2", &[BOB]), ("dm1", &[BOB])]));
    assert!(app.not_joined("c2"), "the relay answered and we are not among its members");
    assert!(!app.not_joined("dm1"), "a DM is never a channel to join");
    assert!(!app.not_joined("c1"), "the relay still said nothing about c1");
}

#[test]
fn a_channel_you_are_not_in_has_no_composer_only_the_join_button() {
    let mut app = app_with(1);
    app.handle(roster(&[("c2", &[BOB])]));
    app.open_channel(1); // random (c2)
    assert!(app.open_channel_not_joined(), "the channel on screen is one we are not in");
    press(&mut app, KeyCode::Tab); // messages -> input
    assert_eq!(app.focus, Focus::Input, "the Join button is reached like the input");
    type_text(&mut app, "hello");
    assert!(app.editor.is_empty(), "there is no text field to type into");
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(matches!(cmds.as_slice(), [Cmd::JoinChannel { channel }] if channel == "c2"), "Enter joins: {cmds:?}");
    assert_eq!(app.open_after.as_deref(), Some("c2"), "and the channel is opened once the list comes back with us in it");
}

#[test]
fn the_join_button_turns_back_into_the_composer_once_the_relay_has_us_in_the_channel() {
    let mut app = app_with(1);
    app.handle(roster(&[("c2", &[BOB])]));
    app.open_channel(1);
    assert!(app.open_channel_not_joined());
    app.handle(AppEvent::Joined { channel: "c2".into(), result: Ok("ev".into()) });
    app.handle(roster(&[("c2", &[BOB, ME])]));
    assert!(!app.open_channel_not_joined(), "we are a member now: the composer is back");
    assert!(app.is_member("c2"));
}

#[test]
fn a_private_channel_offers_no_join() {
    let mut app = App::new(ME.into(), false, chrono_tz::UTC);
    app.handle(AppEvent::Channels(Ok(vec![chan_priv("p1", "secret", true)])));
    app.handle(roster(&[("p1", &[BOB])]));
    app.handle(key(KeyCode::Tab)); // channels -> messages
    app.handle(key(KeyCode::Tab)); // messages -> input
    assert!(app.open_channel_not_joined(), "we are not in it");
    let cmds = app.handle(key(KeyCode::Enter));
    assert!(cmds.is_empty(), "no join is sent: {cmds:?}");
    assert_eq!(app.open_after, None);
    assert!(app.flash_text().unwrap().contains("needs an invitation"), "{:?}", app.flash_text());
}

#[test]
fn opening_the_search_asks_the_relay_who_is_in_the_channels() {
    let mut app = app_with(1);
    let cmds = app.open_search(String::new());
    assert_eq!(app.overlay, Overlay::Search);
    assert!(matches!(cmds.as_slice(), [Cmd::LoadRoster { channels }] if channels.len() == 3), "one request for every channel: {cmds:?}");
}

#[test]
fn enter_on_a_search_row_for_a_channel_you_are_not_in_opens_it_without_joining() {
    let mut app = app_with(1);
    app.handle(roster(&[("c2", &[BOB])]));
    app.open_search("random".into());
    app.search.asked = "random".into(); // the search has already run, so Enter opens the row on the cursor
    let cmds = app.handle(key(KeyCode::Enter));
    assert_eq!(app.overlay, Overlay::None, "the search closes");
    assert_eq!(app.channel().map(|c| c.id.as_str()), Some("c2"), "the channel is opened");
    assert!(!cmds.iter().any(|c| matches!(c, Cmd::JoinChannel { .. })), "nothing is sent: it opens read-only, with the Join button: {cmds:?}");
    assert!(app.open_channel_not_joined());
}

#[test]
fn join_by_name_finds_the_channel_by_name_hash_or_id() {
    for typed in ["/join general", "/join #GENERAL", "/join gen", "/join c1"] {
        let mut app = at_input(1);
        type_text(&mut app, typed);
        let cmds = press(&mut app, KeyCode::Enter);
        assert!(matches!(cmds.as_slice(), [Cmd::JoinChannel { channel }] if channel == "c1"), "{typed}: {cmds:?}");
        assert_eq!(app.open_after.as_deref(), Some("c1"), "{typed}");
    }
}

#[test]
fn join_by_name_opens_a_channel_you_are_already_in_instead_of_joining() {
    let mut app = at_input(1);
    app.joined.insert("c1".into());
    type_text(&mut app, "/join general");
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(!cmds.iter().any(|c| matches!(c, Cmd::JoinChannel { .. })), "no join for a channel you are in: {cmds:?}");
    assert_eq!(app.channel().map(|c| c.id.as_str()), Some("c1"));
}

#[test]
fn join_by_name_says_when_it_finds_nothing_or_too_much() {
    let mut app = at_input(1);
    type_text(&mut app, "/join nope");
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(cmds.is_empty());
    assert!(app.flash_text().unwrap().contains("no channel here is called nope"), "{:?}", app.flash_text());

    let mut app = at_input(1);
    type_text(&mut app, "/join");
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(cmds.is_empty());
    assert!(app.flash_text().unwrap().contains("usage: /join"), "{:?}", app.flash_text());

    let mut app = at_input(1);
    type_text(&mut app, "/join a"); // both general and random contain an "a": nothing to choose between them
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(cmds.is_empty());
    assert!(app.flash_text().unwrap().contains("could be any of 2"), "{:?}", app.flash_text());
}

#[test]
fn leave_asks_first_and_only_y_leaves() {
    let mut app = at_input(1);
    type_text(&mut app, "/leave");
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(cmds.is_empty(), "nothing is sent until it is confirmed: {cmds:?}");
    assert_eq!(app.confirm_leave.as_deref(), Some("c1"));
    assert!(app.flash_text().unwrap().contains("leave general?"), "{:?}", app.flash_text());

    let cmds = press(&mut app, KeyCode::Char('n'));
    assert!(cmds.is_empty(), "anything but y cancels");
    assert_eq!(app.confirm_leave, None);
    assert!(app.flash_text().unwrap().contains("not leaving"), "{:?}", app.flash_text());

    type_text(&mut app, "/leave");
    press(&mut app, KeyCode::Enter);
    let cmds = press(&mut app, KeyCode::Char('y'));
    assert!(matches!(cmds.as_slice(), [Cmd::LeaveChannel { channel }] if channel == "c1"), "{cmds:?}");
    assert_eq!(app.confirm_leave, None);
}

#[test]
fn a_dm_is_not_a_channel_to_leave() {
    let mut app = at_input(1);
    app.open_channel(2); // the DM row
    type_text(&mut app, "/leave");
    let cmds = press(&mut app, KeyCode::Enter);
    assert!(cmds.is_empty());
    assert_eq!(app.confirm_leave, None);
    assert!(app.flash_text().unwrap().contains("not a channel to leave"), "{:?}", app.flash_text());
}

#[test]
fn leaving_drops_the_channel_from_the_local_state_when_the_relay_takes_it() {
    let mut app = app_with(1);
    app.joined.insert("c1".into());
    app.member_counts.insert("c1".into(), 3);
    app.members.insert("c1".into(), vec![("bob".into(), "member".into())]);
    app.unread.insert("c1".into(), 2);
    let cmds = app.handle(AppEvent::Left { channel: "c1".into(), result: Ok("ev".into()) });
    assert!(!app.joined.contains("c1"), "the membership mark goes");
    assert!(!app.member_counts.contains_key("c1"), "and the count");
    assert!(!app.members.contains_key("c1"), "and who may be mentioned in it");
    assert!(!app.unread.contains_key("c1"), "and the unread badge");
    assert!(cmds.iter().any(|c| matches!(c, Cmd::LoadChannels)), "the list is reloaded: {cmds:?}");
    assert!(cmds.iter().any(|c| matches!(c, Cmd::LoadRoster { .. })), "and who is in what: {cmds:?}");
}

#[test]
fn a_refused_join_is_reported_and_only_cancels_the_open_it_names() {
    let mut app = app_with(1);
    app.open_after = Some("c2".into());
    let cmds = app.handle(AppEvent::Joined { channel: "c2".into(), result: Err("not an open channel".into()) });
    assert!(cmds.is_empty());
    assert_eq!(app.open_after, None, "the channel it was waiting to open is given up");
    assert!(app.flash_text().unwrap().contains("could not join the channel"), "{:?}", app.flash_text());

    app.open_after = Some("c9".into());
    app.handle(AppEvent::Joined { channel: "c2".into(), result: Err("no".into()) });
    assert_eq!(app.open_after.as_deref(), Some("c9"), "an answer for another channel does not cancel a newer join");
}

#[test]
fn a_refused_leave_is_reported_and_leaves_the_channel_alone() {
    let mut app = app_with(1);
    app.joined.insert("c1".into());
    let cmds = app.handle(AppEvent::Left { channel: "c1".into(), result: Err("the last owner cannot leave".into()) });
    assert!(cmds.is_empty());
    assert!(app.joined.contains("c1"), "still a member");
    assert!(app.flash_text().unwrap().contains("could not leave the channel"), "{:?}", app.flash_text());
}

#[test]
fn the_roster_marks_membership_and_counts_and_is_rebuilt_from_each_answer() {
    let mut app = app_with(1);
    let members = |who: &[&str]| who.iter().map(|p| (*p).to_string()).collect::<Vec<String>>();
    let cmds = app.handle(AppEvent::Roster(Ok([("c1".to_string(), members(&["me", "bob"])), ("c2".to_string(), members(&["bob"]))].into())));
    assert!(cmds.is_empty());
    assert!(app.is_member("c1"), "you are in c1");
    assert!(!app.is_member("c2"));
    assert_eq!(app.member_counts.get("c1"), Some(&2));
    assert_eq!(app.member_counts.get("c2"), Some(&1));

    // Each answer replaces the last: a channel it says nothing about keeps no count, and the marks come from it alone.
    app.handle(AppEvent::Roster(Ok([("c2".to_string(), members(&["me"]))].into())));
    assert!(app.is_member("c2"));
    assert!(!app.is_member("c1"), "rebuilt from the answer, not remembered");
    assert_eq!(app.member_counts.get("c1"), None, "an unanswered channel keeps no count");
}

#[test]
fn the_roster_drops_a_channel_that_left_the_list_between_the_two_requests() {
    let mut app = app_with(1);
    app.handle(AppEvent::Roster(Ok([("gone".to_string(), vec!["me".to_string()])].into())));
    assert!(!app.is_member("gone"), "a channel that is not in the list is not tracked");
    assert_eq!(app.member_counts.get("gone"), None);
}

#[test]
fn a_channel_list_that_appears_or_changes_asks_who_is_in_what() {
    let mut app = App::new(ME.into(), false, chrono_tz::UTC);
    let cmds = app.handle(AppEvent::Channels(Ok(vec![chan("c1", "general")])));
    assert!(
        cmds.iter().any(|c| matches!(c, Cmd::LoadRoster { channels } if channels.len() == 1 && channels[0] == "c1")),
        "the marks and counts come from the relay: {cmds:?}"
    );
}
