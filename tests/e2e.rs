//! End-to-end UI tests: the real `buzzr` binary, in a pseudo-terminal, talking to the fake relay
//! (`https://github.com/buzzzr/buzz-fake-relay`). They check what is actually on the screen — which
//! channel opens, what the sidebar lists, what each key does, what the overlays say — none of which
//! the unit tests, that never start a process, can see.
//!
//! **Every change to navigation, to what is drawn, or to a key binding adds a case here.** See
//! `tests/e2e/README.md` for how to run them and what to check.
//!
//! The tests skip themselves (they do not fail) unless `BUZZ_FAKE_RELAY_BIN` names a built
//! `buzz-fake-relay` binary. `tests/e2e/run.sh` fetches, builds and runs one.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The pane the app thinks it is drawing into, and the size the screenshots use.
const ROWS: u16 = 36;
const COLS: u16 = 110;
/// Any fixed key: the fake relay accepts AUTH without checking it.
const KEY: &str = "0505050505050505050505050505050505050505050505050505050505050505";
/// How long to wait for the screen to show something. Generous: a loaded machine must not flake.
const WAIT: Duration = Duration::from_secs(20);

/// The built `buzz-fake-relay`, or `None` when the suite cannot run here.
fn relay_bin() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("BUZZ_FAKE_RELAY_BIN")?);
    path.is_file().then_some(path)
}

fn relay_or_skip(test: &str) -> Option<Relay> {
    match Relay::start() {
        Some(relay) => Some(relay),
        None => {
            eprintln!("skipping {test}: set BUZZ_FAKE_RELAY_BIN to a built buzz-fake-relay (or run tests/e2e/run.sh)");
            None
        }
    }
}

/// A `buzz-fake-relay` on its own port, killed when the test that started it ends.
struct Relay {
    child: Child,
    url: String,
}

impl Relay {
    fn start() -> Option<Relay> {
        let bin = relay_bin()?;
        let port = free_port();
        let mut child = Command::new(bin)
            .arg(format!("127.0.0.1:{port}"))
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("could not start buzz-fake-relay");
        // It prints its address as soon as it is listening.
        let mut line = String::new();
        if let Some(err) = child.stderr.take() {
            let _ = BufReader::new(err).read_line(&mut line);
        }
        assert!(line.contains(":") || line.contains("relay"), "the relay said nothing useful: {line:?}");
        wait_for_port(port);
        Some(Relay { child, url: format!("http://127.0.0.1:{port}") })
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").expect("no free port").local_addr().unwrap().port()
}

/// A config directory no other run is using.
fn fresh_config_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir().join(format!("buzzr-e2e-{}-{nanos}", std::process::id()))
}

fn wait_for_port(port: u16) {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("the fake relay never started listening on port {port}");
}

/// One run of the real binary on a pseudo-terminal, with the screen parsed as the app draws it.
struct Session {
    child: Child,
    out: std::fs::File,
    parser: Arc<Mutex<vt100::Parser>>,
    reader: Option<JoinHandle<()>>,
    config: PathBuf,
}

impl Session {
    fn start(relay: &Relay) -> Session {
        Self::start_in(relay, fresh_config_dir())
    }

    /// Start the app against `relay` with `config` as its config directory, so a test can restart it on
    /// the state (a read position) a previous run left behind.
    fn start_in(relay: &Relay, config: PathBuf) -> Session {
        let pty = nix::pty::openpty(
            Some(&nix::pty::Winsize { ws_row: ROWS, ws_col: COLS, ws_xpixel: 0, ws_ypixel: 0 }),
            None,
        )
        .expect("could not open a pty");
        // No echo: our keystrokes must not come back and land on the screen we are reading.
        if let Ok(mut t) = nix::sys::termios::tcgetattr(&pty.slave) {
            t.local_flags.remove(nix::sys::termios::LocalFlags::ECHO);
            let _ = nix::sys::termios::tcsetattr(&pty.slave, nix::sys::termios::SetArg::TCSANOW, &t);
        }
        std::fs::create_dir_all(&config).expect("could not make a config directory");

        let out = std::fs::File::from(pty.master);
        let inp = std::fs::File::from(pty.slave);
        let child = Command::new(env!("CARGO_BIN_EXE_buzzr"))
            .arg("--no-plugins")
            .env("BUZZR_CONFIG_DIR", &config)
            .env("BUZZ_RELAY_URL", &relay.url)
            .env("BUZZ_PRIVATE_KEY", KEY)
            .env("BUZZR_NO_UPDATE_CHECK", "1")
            .env("TERM", "xterm-256color")
            .env("TZ", "UTC")
            .stdin(Stdio::from(inp.try_clone().unwrap()))
            .stdout(Stdio::from(inp.try_clone().unwrap()))
            .stderr(Stdio::from(inp))
            .spawn()
            .expect("could not start buzzr");

        let rows = std::num::NonZeroU16::new(ROWS).unwrap();
        let cols = std::num::NonZeroU16::new(COLS).unwrap();
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let sink = parser.clone();
        let mut reader_out = out.try_clone().unwrap();
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                let n = match reader_out.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let data = &buf[..n];
                // Answer the cursor-position query; the rest of the capability probes can time out.
                if data.windows(4).any(|w| w == b"\x1b[6n") {
                    let _ = reader_out.write_all(b"\x1b[1;1R");
                }
                sink.lock().unwrap().process(data);
            }
        });
        Session { child, out, parser, reader: Some(reader), config }
    }

    fn screen(&self) -> String {
        self.parser.lock().unwrap().screen().contents()
    }

    /// The screen once `pred` holds, or a panic with the last screen when it never does.
    fn until(&self, what: &str, pred: impl Fn(&str) -> bool) -> String {
        let end = Instant::now() + WAIT;
        loop {
            let s = self.screen();
            if pred(&s) {
                return s;
            }
            if Instant::now() > end {
                panic!("timed out waiting for {what}. The screen was:\n{}\n{}", ruler(), s);
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    }

    /// Wait until the screen shows `needle`.
    fn expect(&self, what: &str, needle: &str) {
        self.until(&format!("{what}: {needle:?}"), |s| s.contains(needle));
    }

    /// Wait until the screen no longer shows `needle`.
    fn expect_gone(&self, what: &str, needle: &str) {
        self.until(&format!("{what}: {needle:?} to be gone"), |s| !s.contains(needle));
    }

    fn write(&self, bytes: &[u8]) {
        let mut out = self.out.try_clone().unwrap();
        out.write_all(bytes).expect("could not write to the pty");
        out.flush().ok();
    }

    /// Press one key, by name ("esc", "backtab", "ctrl+f", ...) or as literal text.
    fn press(&self, key: &str) {
        let (bytes, settle) = key_bytes(key);
        self.write(&bytes);
        std::thread::sleep(settle);
    }

    fn press_all(&self, keys: &[&str]) {
        for k in keys {
            self.press(k);
        }
    }

    /// Type literal text in one write (faster than one `press` per character).
    fn type_text(&self, text: &str) {
        self.write(text.as_bytes());
        std::thread::sleep(Duration::from_millis(250));
    }

    /// Wait for the process to end (Ctrl+Q and friends), returning its exit status.
    fn wait_exit(&mut self, within: Duration) -> Option<std::process::ExitStatus> {
        let end = Instant::now() + within;
        while Instant::now() < end {
            if let Some(status) = self.child.try_wait().expect("could not wait for buzzr") {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        None
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
        let _ = std::fs::remove_dir_all(&self.config);
    }
}

/// A screen with a ruler above it, so a failure says which line is wrong.
fn ruler() -> String {
    let mut s = String::from("    ");
    for i in 0..COLS {
        s.push_str(if i % 10 == 0 { "|" } else { "-" });
    }
    s.push('\n');
    s
}

fn key_bytes(key: &str) -> (Vec<u8>, Duration) {
    let quick = Duration::from_millis(250);
    let slow = Duration::from_millis(450); // a lone Esc needs a moment before the next key
    let b: Vec<u8> = match key {
        "esc" => b"\x1b".to_vec(),
        "enter" => b"\r".to_vec(),
        "tab" => b"\t".to_vec(),
        "backtab" => b"\x1b[Z".to_vec(),
        "up" => b"\x1b[A".to_vec(),
        "down" => b"\x1b[B".to_vec(),
        "home" => b"\x1b[H".to_vec(),
        "end" => b"\x1b[F".to_vec(),
        "f1" => b"\x1bOP".to_vec(),
        "f2" => b"\x1bOQ".to_vec(),
        "ctrl+f" => b"\x06".to_vec(),
        "ctrl+q" => b"\x11".to_vec(),
        other => other.as_bytes().to_vec(),
    };
    (b, if key == "esc" { slow } else { quick })
}

/// The sidebar, top to bottom, as the app draws it: the rows in the channel pane (the pane is 28
/// columns wide, and a `▌` marks the row the channel cursor is on). Every row is returned, including
/// the section titles: a channel row starts with `#` (or `*` when private) and a DM row with `✉ `.
fn sidebar_marked(screen: &str) -> Vec<(bool, String)> {
    screen
        .lines()
        .skip(1) // the header row
        .take(12)
        .map(|l| l.chars().take(24).collect::<String>())
        .map(|l| l.trim_end().to_string())
        .map(|l| match l.strip_prefix('▌') {
            Some(rest) => (true, rest.trim_start().to_string()),
            None => (false, l.trim_start().to_string()),
        })
        .filter(|(_, l)| !l.is_empty())
        .collect()
}

fn sidebar(screen: &str) -> Vec<String> {
    sidebar_marked(screen).into_iter().map(|(_, label)| label).collect()
}

/// The label of the channel the cursor bar (`▌`) is on, if any.
fn bar_on(screen: &str) -> Option<String> {
    sidebar_marked(screen).into_iter().find_map(|(bar, label)| bar.then_some(label))
}

/// The header line: `buzzr`, the open channel or thread, and the connection state.
fn header(screen: &str) -> String {
    screen.lines().next().unwrap_or_default().trim_end().to_string()
}

fn session(relay: &Relay) -> Session {
    let s = Session::start(relay);
    settle(&s);
    s
}

/// A session started on an existing config directory (see `Session::start_in`).
fn session_in(relay: &Relay, config: PathBuf) -> Session {
    let s = Session::start_in(relay, config);
    settle(&s);
    s
}

/// Wait for the app's first screen: the channel it opens by itself, the live connection, and the
/// keyboard on the channel list.
fn settle(s: &Session) {
    s.expect("the app to draw the first channel", "#design");
    s.expect("the live WebSocket to connect", "● live");
    // Startup settles with the channel list focused: the first channel is opened so its messages are
    // on screen, but the keyboard stays on the channels, and Escape comes back here from the messages.
    s.expect("the channel list to have focus", "Open channel");
}

// ---------------------------------------------------------------------------------------------------
// Navigation: what the sidebar lists, what the cursor and the pane switcher do, what opens what.

#[test]
fn the_sidebar_lists_the_channels_the_relay_serves_and_opens_the_first_one() {
    let Some(relay) = relay_or_skip("the_sidebar_lists_...") else { return };
    let s = session(&relay);
    // The relay serves the DMs as hidden rooms named "DM"; buzzr renames each after the other
    // member, but only once the room's member list and those members' profiles have arrived — which
    // is after the first screen. Wait for that, or the assertion below races the relabelling.
    let screen = s.until("the three DMs to be named after the other member", |sc| {
        sidebar(sc).iter().filter(|n| n.starts_with("✉ ")).count() == 3
    });
    let names = sidebar(&screen);
    for want in ["#design", "#dev", "#general", "#ops", "#random"] {
        assert!(names.iter().any(|n| n == want), "the sidebar is missing {want:?}. It lists {names:?}");
    }
    assert_eq!(names.iter().filter(|n| n.starts_with('#')).count(), 5, "five channels. The sidebar lists {names:?}");
    assert_eq!(names.iter().filter(|n| n.starts_with("✉ ")).count(), 3, "three DMs. The sidebar lists {names:?}");
    assert!(!names.iter().any(|n| n == "DM"), "every DM row is named after a member: {names:?}");
    let h = header(&s.screen());
    assert!(h.contains("#design"), "the first channel opens by itself; the header is {h:?}");
    assert_eq!(bar_on(&s.screen()).as_deref(), Some("#design"), "the cursor bar is on the open channel");
}

#[test]
fn the_arrow_keys_move_the_channel_cursor_and_enter_opens_that_channel() {
    let Some(relay) = relay_or_skip("the_arrow_keys_move_...") else { return };
    let s = session(&relay);
    // The app starts on the channel list, so the cursor is already live.
    s.expect("the channel list to have focus", "Open channel");
    s.press_all(&["down", "down"]);
    let screen = s.until("the bar to reach the third channel", |sc| bar_on(sc).as_deref() == Some("#general"));
    assert_eq!(bar_on(&screen).as_deref(), Some("#general"), "the bar follows the cursor:\n{screen}");
    s.press("enter");
    let screen = s.until("the header to name the channel that was opened", |sc| header(sc).contains("#general"));
    let h = header(&screen);
    assert!(!h.contains("#design"), "the header left the old channel: {h:?}");
    s.expect("the opened channel's messages", "hello from c-general");
}

#[test]
fn tab_cycles_the_focus_between_the_channels_messages_and_input() {
    let Some(relay) = relay_or_skip("tab_cycles_the_focus_...") else { return };
    let s = session(&relay);
    s.expect("the channel list", "Open channel");
    s.press("tab");
    s.expect("the messages pane", "Open thread");
    s.press("tab");
    s.expect("the input box", "Send");
    s.press("tab");
    s.expect("the channel list again", "Open channel");
}

#[test]
fn enter_on_a_message_opens_its_thread_and_esc_goes_back_to_the_channel() {
    let Some(relay) = relay_or_skip("enter_on_a_message_...") else { return };
    let s = session(&relay);
    // Open #general: design, dev, general is the third row. The cursor is already on the channel list.
    s.press_all(&["down", "down", "enter"]);
    s.expect("the general channel", "hello from c-general");
    s.expect("its thread summary", "2 replies");
    s.press("enter"); // open the thread of the selected message
    let screen = s.until("the thread view", |sc| header(sc).contains("› thread"));
    let h = header(&screen);
    assert!(h.contains("#general"), "the thread still names its channel: {h:?}");
    s.expect("the first reply, which is only in the thread", "the sidebar keeps a fixed order");
    s.press("esc");
    let screen = s.until("the header to go back to the channel", |sc| header(sc).contains("#general") && !header(sc).contains("› thread"));
    let h = header(&screen);
    assert!(h.contains("#general"), "back in the channel: {h:?}");
    s.expect("the thread summary again", "2 replies");
}

#[test]
fn ctrl_f_searches_channels_and_messages_and_esc_closes_the_overlay() {
    let Some(relay) = relay_or_skip("ctrl_f_searches_...") else { return };
    let s = session(&relay);
    s.press("ctrl+f");
    s.expect("the search overlay", "Search · Enter opens");
    s.expect("the prompt", "Search:");
    s.type_text("sidebar");
    s.expect("the reply that contains the query", "the sidebar keeps a fixed order");
    s.press("esc");
    s.expect_gone("the overlay to close", "Search · Enter opens");
}

#[test]
fn f1_lists_the_keys_and_esc_closes_them() {
    let Some(relay) = relay_or_skip("f1_lists_the_keys_...") else { return };
    let s = session(&relay);
    s.press("f1");
    s.expect("the keys overlay", "Keys  (Esc closes)");
    s.expect("a key listing", "switch between channels");
    s.press("esc");
    s.expect_gone("the keys overlay to close", "Keys  (Esc closes)");
}

#[test]
fn f2_shows_the_help_and_esc_closes_it() {
    let Some(relay) = relay_or_skip("f2_shows_the_help_...") else { return };
    let s = session(&relay);
    s.press("f2");
    s.expect("the help overlay", "/r N text");
    s.expect("how to search", "/search text");
    s.press("esc");
    s.expect_gone("the help overlay to close", "/r N text");
}

#[test]
fn typing_reaches_the_input_box() {
    let Some(relay) = relay_or_skip("typing_reaches_the_input_box") else { return };
    let s = session(&relay);
    s.press_all(&["tab", "tab"]); // channels -> messages -> input
    s.expect("the input box to take focus", "Send");
    s.type_text("a message for the fake relay");
    s.expect("the typed text in the input box", "a message for the fake relay");
    s.press("esc"); // empty and not replying: back to the messages
    s.expect("the messages pane again", "Open thread");
}

#[test]
fn u_marks_the_selected_message_unread_and_the_sidebar_counts_it() {
    let Some(relay) = relay_or_skip("u_marks_the_selected_message_...") else { return };
    let s = session(&relay);
    // #design is open but the keyboard starts on the channels; Tab moves it to the message list.
    s.press("tab");
    s.expect("the messages pane", "Open thread");
    s.expect("the design message", "hello from c-design");
    s.press("u");
    let screen = s.until("the open channel to show an unread count", |sc| {
        sidebar(sc).iter().any(|n| n.starts_with("#design (1)"))
    });
    assert!(screen.contains("new messages"), "the divider marks the message the channel was marked unread from:\n{screen}");
}

#[test]
fn a_thread_with_replies_since_the_last_visit_is_marked_unread_on_its_message() {
    let Some(relay) = relay_or_skip("a_thread_with_replies_...") else { return };
    // #general carries a thread with two replies. Open it once so the app writes the thread's read
    // position down, then start again with that position far in the past: both replies are unseen.
    let first = session(&relay);
    first.press_all(&["down", "down", "enter"]);
    first.expect("the general channel", "hello from c-general");
    first.expect("the thread summary", "2 replies");
    first.press("enter"); // open the thread, which reads it
    first.expect("the thread view", "the sidebar keeps a fixed order");
    let root = wait_for_stored_thread(&first.config.join("read-state.json"));
    let config = first.config.clone();
    drop(first); // kills the app and clears the config directory
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("read-state.json"), format!("{{\"channels\":{{}},\"threads\":{{\"{root}\":1}}}}")).unwrap();

    let s = session_in(&relay, config);
    s.press_all(&["down", "down", "enter"]);
    s.expect("the general channel", "hello from c-general");
    s.expect("the thread to be marked unread", "● 2 new");
    s.press("enter"); // open the thread: reading it clears the mark
    s.expect("the thread view", "the sidebar keeps a fixed order");
    s.press("esc");
    s.expect("the thread summary again", "2 replies");
    s.expect_gone("the unread mark once the thread has been read", "● 2 new");
}

/// The first thread root id the app has written into `read-state.json`, waiting for the file to appear.
fn wait_for_stored_thread(path: &std::path::Path) -> String {
    let end = Instant::now() + WAIT;
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Some(root) = first_thread_id(&text) {
                return root;
            }
        }
        if Instant::now() > end {
            panic!("read-state.json never recorded a thread position");
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

/// The first key of the `threads` map in `read-state.json`, when it has one.
fn first_thread_id(text: &str) -> Option<String> {
    let i = text.find("\"threads\"")?;
    let open = i + text[i..].find('{')? + 1;
    let start = open + text[open..].find('"')? + 1;
    let end = start + text[start..].find('"')?;
    (end > start).then(|| text[start..end].to_owned())
}

#[test]
fn ctrl_q_quits() {
    let Some(relay) = relay_or_skip("ctrl_q_quits") else { return };
    let mut s = session(&relay);
    s.press("ctrl+q");
    let status = s.wait_exit(Duration::from_secs(10)).expect("buzzr should exit on Ctrl+Q");
    assert!(status.success(), "buzzr exited with {status:?}");
}

#[test]
fn the_selftest_passes_against_the_fake_relay() {
    let Some(relay) = relay_or_skip("the_selftest_passes_...") else { return };
    let config = std::env::temp_dir().join(format!("buzzr-selftest-{}", std::process::id()));
    let out = Command::new(env!("CARGO_BIN_EXE_buzzr"))
        .args(["--selftest", "--relay", &relay.url])
        .env("BUZZR_CONFIG_DIR", &config)
        .env("BUZZ_PRIVATE_KEY", KEY)
        .env("BUZZR_NO_UPDATE_CHECK", "1")
        .output()
        .expect("could not run buzzr --selftest");
    let _ = std::fs::remove_dir_all(&config);
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "buzzr --selftest failed against the fake relay:\n{text}");
    assert!(text.contains("selftest ok"), "buzzr --selftest did not say it was ok:\n{text}");
}
