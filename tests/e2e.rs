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
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The pane the app thinks it is drawing into, and the size the screenshots use.
const ROWS: u16 = 36;
const COLS: u16 = 110;
/// A pane short enough that a thread is taller than the message pane. The fake relay gives every channel a
/// single message, so only in a short pane can the newest reply fall out of view. Twelve rows leave a
/// five-line pane: the pinned root header on the first row, and four rows under it for #general's two
/// two-line replies, whose six lines (each reply's two lines and its blank separator) do not all fit.
const SHORT_ROWS: u16 = 12;
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

/// `POST /query` to the relay, returning the raw JSON body — a plain HTTP request, so the test reads what the relay
/// actually holds rather than what buzzr's own client thinks it holds.
fn relay_query(relay: &Relay, filters: &str) -> String {
    let addr = relay.url.trim_start_matches("http://");
    let mut stream = std::net::TcpStream::connect(addr).expect("could not reach the relay");
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let request = format!(
        "POST /query HTTP/1.1\r\nhost: {addr}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{filters}",
        filters.len()
    );
    stream.write_all(request.as_bytes()).expect("could not write to the relay");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("could not read the relay's answer");
    response.split_once("\r\n\r\n").map(|(_, body)| body.to_owned()).unwrap_or(response)
}

/// The agent's pubkey out of its record file, as a `kind:0` filter needs it.
fn agent_pubkey(record: &str) -> String {
    const KEY: &str = "\"pubkey\": \"";
    let start = record.find(KEY).expect("the record names the agent's key") + KEY.len();
    record[start..start + 64].to_owned()
}

/// A channel id for the traffic a test has to publish itself: buzzr builds a message from a channel **uuid**, and the
/// fake relay's ids (`c-design`) are not uuids — the wall the invite and join cases meet — so a channel of the test's
/// own is the only channel a message can be built for here.
const TEST_CHANNEL: &str = "0b6c4b1e-7a0a-4f4f-9d4f-2a6f0f1c5a11";

/// Publish one message to the relay as the person, through buzzr's own client: a real signed event over `POST /events`,
/// with the mentions as `p` tags — exactly what a composer sends. `root` makes it a reply in that thread. Returns the
/// event's id, so a test can thread a reply onto what it published. For a test that needs relay traffic the UI cannot
/// send (see the agent cases).
fn publish_message(relay: &Relay, channel: &str, text: &str, mentions: &[String], root: Option<&str>) -> String {
    let keys = nostr::Keys::parse(KEY).expect("the test key");
    let client = buzzr::relay::Relay::new(&relay.url, keys, None).expect("a client for the relay");
    let message = buzzr::relay::message_builder(channel, text, root, mentions).expect("a message for that channel");
    tokio::runtime::Runtime::new().expect("a runtime").block_on(client.submit(message)).expect("the relay took the message")
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
        Self::start_in(relay, fresh_config_dir(), false)
    }

    /// A session whose pty answers the keyboard-enhancement probe, so the app believes the terminal reports
    /// Shift+Enter separately from Enter (the kitty keyboard protocol: kitty, foot, WezTerm, Ghostty, iTerm2).
    fn start_enhanced(relay: &Relay) -> Session {
        Session::start_in(relay, fresh_config_dir(), true)
    }

    /// A session on a pty short enough that the newest message can leave the view (see `SHORT_ROWS`).
    fn start_short(relay: &Relay) -> Session {
        Session::start_sized(relay, fresh_config_dir(), false, SHORT_ROWS)
    }

    /// Start the app against `relay` with `config` as its config directory, so a test can restart it on
    /// the state (a read position) a previous run left behind.
    fn start_in(relay: &Relay, config: PathBuf, keyboard_enhanced: bool) -> Session {
        Session::start_sized(relay, config, keyboard_enhanced, ROWS)
    }

    fn start_sized(relay: &Relay, config: PathBuf, keyboard_enhanced: bool, rows: u16) -> Session {
        Session::start_full(relay, config, keyboard_enhanced, rows, None)
    }

    /// Like `start_sized`, but with plugins on and `dir` as their folder, so a test can see what a plugin draws
    /// (every other session starts with `--no-plugins`).
    fn start_with_plugins(relay: &Relay, dir: PathBuf, rows: u16) -> Session {
        Session::start_full(relay, fresh_config_dir(), false, rows, Some(dir))
    }

    fn start_full(relay: &Relay, config: PathBuf, keyboard_enhanced: bool, rows: u16, plugins: Option<PathBuf>) -> Session {
        let pty = nix::pty::openpty(
            Some(&nix::pty::Winsize { ws_row: rows, ws_col: COLS, ws_xpixel: 0, ws_ypixel: 0 }),
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
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_buzzr"));
        match &plugins {
            Some(dir) => cmd.env("BUZZR_PLUGINS_DIR", dir),
            None => cmd.arg("--no-plugins"),
        };
        cmd.env("BUZZR_CONFIG_DIR", &config)
            .env("BUZZ_RELAY_URL", &relay.url)
            .env("BUZZ_PRIVATE_KEY", KEY)
            .env("BUZZR_NO_UPDATE_CHECK", "1")
            .env("TERM", "xterm-256color")
            .env("TZ", "UTC")
            .stdin(Stdio::from(inp.try_clone().unwrap()))
            .stdout(Stdio::from(inp.try_clone().unwrap()))
            .stderr(Stdio::from(inp));
        // The harness the agents flow starts is not installed in a test environment: put the fake first on `PATH`.
        let mut harness_path = fake_acp_dir().as_os_str().to_owned();
        harness_path.push(":");
        harness_path.push(std::env::var_os("PATH").unwrap_or_default());
        cmd.env("PATH", harness_path);
        if keyboard_enhanced {
            // Make the pty the child's controlling terminal, the way a terminal emulator does: the app asks for the
            // keyboard protocol on /dev/tty, and a test runner started from a terminal has a /dev/tty of its own.
            use std::os::unix::process::CommandExt;
            unsafe {
                cmd.pre_exec(|| {
                    if nix::libc::setsid() == -1 || nix::libc::ioctl(0, nix::libc::TIOCSCTTY, 0) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let child = cmd.spawn().expect("could not start buzzr");

        let rows = std::num::NonZeroU16::new(rows).unwrap();
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
                // ESC [ ? u asks which keyboard-enhancement flags are supported and ESC [ c asks for the primary
                // device attributes; the flags answer must come first or the app reads the DA as "no flags".
                if keyboard_enhanced && data.windows(4).any(|w| w == b"\x1b[?u") {
                    let _ = reader_out.write_all(b"\x1b[?1u\x1b[?62;c");
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
        "alt+enter" => b"\x1b\r".to_vec(),          // what every terminal sends for Alt+Enter
        "shift+enter" => b"\x1b[13;2u".to_vec(),    // what a kitty-protocol terminal sends for Shift+Enter
        "tab" => b"\t".to_vec(),
        "backtab" => b"\x1b[Z".to_vec(),
        "up" => b"\x1b[A".to_vec(),
        "down" => b"\x1b[B".to_vec(),
        "right" => b"\x1b[C".to_vec(),
        "left" => b"\x1b[D".to_vec(),
        "home" => b"\x1b[H".to_vec(),
        "end" => b"\x1b[F".to_vec(),
        "f1" => b"\x1bOP".to_vec(),
        "f2" => b"\x1bOQ".to_vec(),
        "ctrl+f" => b"\x06".to_vec(),
        "ctrl+k" => b"\x0b".to_vec(),
        "ctrl+q" => b"\x11".to_vec(),
        other => other.as_bytes().to_vec(),
    };
    (b, if key == "esc" { slow } else { quick })
}

/// The sidebar, top to bottom, as the app draws it: the rows in the channel pane (the pane is 28
/// columns wide, and a `▌` marks the row the channel cursor is on). Every row is returned, including
/// the section titles: a channel row starts with `#` (or `*` when private) and a DM row with `@`.
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

/// The lines of the messages pane, top to bottom: what sits right of the divider that separates it from the
/// channel list. Lines that are not part of the pane (the sidebar's own rows, the footer) come back empty.
fn messages_pane(screen: &str) -> Vec<String> {
    screen
        .lines()
        .skip(1) // the header row
        .map(|l| match l.split_once('│') {
            Some((_, right)) => right.trim_end().to_string(),
            None => String::new(),
        })
        .collect()
}

/// How far a pane line is pushed in: its count of leading spaces. The cursor bar (`▌`) covers the line's first
/// column, so it is read as the blank it hides (`unbarred` first).
fn indent_of(line: &str) -> usize {
    let line = match line.strip_prefix('▌') {
        Some(rest) => format!(" {rest}"),
        None => line.to_string(),
    };
    line.chars().take_while(|c| *c == ' ').count()
}

/// The line of the messages pane the cursor bar (`▌`) is on, if any — the selected message's first line.
fn cursor_row(screen: &str) -> Option<String> {
    messages_pane(screen).into_iter().find(|l| l.starts_with('▌'))
}

/// How many lines the input box is showing: the rows between its top border (which carries the ` message ` title)
/// and its bottom one. A composer that has sent its text is back to the one-line empty state.
fn composer_height(screen: &str) -> usize {
    let top = screen.lines().position(|l| l.contains('┌') && l.contains("message"));
    let bottom = screen.lines().position(|l| l.contains('└'));
    match (top, bottom) {
        (Some(t), Some(b)) if b > t => b - t - 1,
        _ => 0,
    }
}

fn session(relay: &Relay) -> Session {
    let s = Session::start(relay);
    settle(&s);
    s
}

/// A session started on an existing config directory (see `Session::start_in`).
fn session_in(relay: &Relay, config: PathBuf) -> Session {
    let s = Session::start_in(relay, config, false);
    settle(&s);
    s
}

/// Run a command the way a user does now: open the `Ctrl+K` palette, type the command's name, Enter, then the
/// argument and Enter again. `name` and `arg` are the two halves of what the old `/command` line held. It is for a
/// command that takes an argument — the palette asks for one, so the second Enter runs it (an empty `arg` takes the
/// command whose argument is optional).
fn run_in_palette(s: &Session, name: &str, arg: &str) {
    s.press("ctrl+k");
    s.expect("the command palette", "Commands · Enter runs · Esc closes");
    s.type_text(name);
    s.press("enter");
    if !arg.is_empty() {
        s.type_text(arg);
    }
    s.press("enter");
}

/// A plugins folder with nothing in it: plugins are on, so the compiled-in ones start, but no runtime-loaded plugin
/// is there to draw. (`session` starts with `--no-plugins` for isolation; a test of anything a plugin owns needs this.)
fn empty_plugins_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("buzzr-e2e-no-plugins-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// The fake `claude-agent-acp` on a directory of its own, made once and put first on every session's `PATH`.
///
/// `agents create` starts the harness and reads what it offers over ACP before it makes anything, and no real harness
/// is installed in a test environment. The fake is the script committed at `tests/fixtures/claude-agent-acp` (the same
/// one the unit tests in `src/harness/acp.rs` drive); it is copied out, and made executable, so every session can find
/// it by name on `PATH`. Only the agents flow ever starts a harness, so nothing else notices it.
fn fake_acp_dir() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("buzzr-e2e-harness-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("could not make the fake harness directory");
        let path = dir.join("claude-agent-acp");
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-agent-acp");
        std::fs::copy(fixture, &path).expect("could not copy the fake harness");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("could not make the fake harness executable");
        dir
    })
}

/// Like `session`, but with plugins on and an empty plugins folder, so a compiled-in plugin is there and no
/// runtime-loaded one is.
fn session_with_plugins(relay: &Relay) -> Session {
    let s = Session::start_full(relay, fresh_config_dir(), false, ROWS, Some(empty_plugins_dir()));
    settle(&s);
    s
}

/// Like `session_in`, but with plugins on (see `session_with_plugins`).
fn session_in_with_plugins(relay: &Relay, config: PathBuf) -> Session {
    let s = Session::start_full(relay, config, false, ROWS, Some(empty_plugins_dir()));
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

/// The same startup, on a pty that answers the keyboard-enhancement probe (a terminal that reports Shift+Enter).
fn session_enhanced(relay: &Relay) -> Session {
    let s = Session::start_enhanced(relay);
    settle(&s);
    s
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
        sidebar(sc).iter().filter(|n| n.starts_with("@")).count() == 3
    });
    let names = sidebar(&screen);
    for want in ["#design", "#dev", "#general", "#ops", "#random"] {
        assert!(names.iter().any(|n| n == want), "the sidebar is missing {want:?}. It lists {names:?}");
    }
    assert_eq!(names.iter().filter(|n| n.starts_with('#')).count(), 5, "five channels. The sidebar lists {names:?}");
    assert_eq!(names.iter().filter(|n| n.starts_with("@")).count(), 3, "three DMs. The sidebar lists {names:?}");
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
fn the_arrow_keys_move_the_focus_between_the_messages_and_the_input() {
    let Some(relay) = relay_or_skip("the_arrow_keys_move_the_focus_...") else { return };
    let s = session(&relay);
    // Startup leaves the keyboard on the channel list; two Tabs reach the input.
    s.press("tab");
    s.expect("the messages pane", "Open thread");
    s.press("tab");
    s.expect("the input box", "Send");
    s.press("up"); // nothing typed: back to the messages, on the newest one
    s.expect("the messages pane to take focus again", "Open thread");
    s.expect_gone("the input hint to go", "Send");
    s.press("down"); // the newest message: back to the input
    s.expect("the input box to take focus again", "Send");
    s.expect_gone("the messages hint to go", "Open thread");
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
fn a_thread_reads_as_a_root_header_line_with_the_replies_indented_under_it() {
    let Some(relay) = relay_or_skip("a_thread_reads_as_a_root_header_line_...") else { return };
    let s = session(&relay);
    // #general is the third channel, and its newest message carries a two-reply thread.
    s.press_all(&["down", "down", "enter"]);
    s.expect("the general channel", "hello from c-general");
    s.press("enter"); // open the thread of the selected message
    s.expect("the thread view", "› thread");
    // The root is the thread's header: its number, time and author share the line with the first line of
    // what it says, instead of taking a row of its own in the reply feed.
    let screen = s.until("the root on the first line of the pane", |sc| {
        messages_pane(sc).first().is_some_and(|l| l.contains("hello from c-general"))
    });
    let pane = messages_pane(&screen);
    let root = pane[0].clone();
    assert!(
        root.contains("#1") && root.contains("hello from c-general"),
        "the root's number and text are on one header line: {root:?}"
    );
    let root_indent = indent_of(&root);

    // Every reply sits two columns further in than the root — head line and body alike — so the pane reads
    // as a root with replies under it.
    for (head, body) in [
        ("#2", "the sidebar keeps a fixed order"),
        ("#3", "and Enter opens the thread"),
    ] {
        let head_line = pane
            .iter()
            .find(|l| l.trim_start_matches('▌').trim_start().starts_with(head))
            .unwrap_or_else(|| panic!("reply {head} is on screen. The pane is:\n{}", pane.join("\n")));
        let body_line = pane
            .iter()
            .find(|l| l.contains(body))
            .unwrap_or_else(|| panic!("reply {body:?} is on screen. The pane is:\n{}", pane.join("\n")));
        assert_eq!(
            indent_of(head_line),
            root_indent + 2,
            "reply {head} is indented under the root: {head_line:?} (root {root:?})"
        );
        assert_eq!(
            indent_of(body_line),
            root_indent + 2,
            "reply {head}'s text lines up with its head: {body_line:?} (root {root:?})"
        );
    }
}

/// With a message row selected, Right walks into its thread and Left leaves it, the up/down of the tree rather
/// than of the list. A message with no replies has nothing under it, so there both keys do nothing.
#[test]
fn the_arrow_keys_walk_into_a_thread_and_back_out_of_it() {
    let Some(relay) = relay_or_skip("the_arrow_keys_walk_...") else { return };
    let s = session(&relay);
    // #general is the third channel; its newest message carries a two-reply thread. Enter opens the channel and
    // leaves the keyboard on the messages, on that message.
    s.press_all(&["down", "down", "enter"]);
    s.expect("the general channel", "hello from c-general");
    s.expect("its thread summary", "2 replies");

    // Right, on the selected message, opens its thread the way Enter does.
    s.press("right");
    s.expect("the thread view", "› thread");
    s.expect("the first reply, which is only in the thread", "the sidebar keeps a fixed order");
    // The thread opens at its bottom, on the newest reply. Its root is the header pinned to the top of the pane, so
    // it is the pane's first row and not a row the cursor can move onto.
    let pane = messages_pane(&s.screen());
    let root_row = pane.first().cloned().unwrap_or_default();
    assert!(
        root_row.contains("#1") && root_row.contains("hello from c-general"),
        "the pane pins its root as the first row, not a row in the feed: {root_row:?}"
    );
    let screen = s.until("the cursor on the newest reply", |sc| cursor_row(sc).is_some_and(|l| l.contains("#3")));
    assert!(
        messages_pane(&screen).iter().any(|l| l.contains("and Enter opens the thread")),
        "the newest reply is the one the cursor is on. The pane is:\n{}",
        messages_pane(&screen).join("\n")
    );

    // Up walks up into the replies, and the pinned root stays the first row while the cursor moves under it.
    s.press("up");
    let screen = s.until("the cursor on the first reply", |sc| cursor_row(sc).is_some_and(|l| l.contains("#2")));
    assert!(
        messages_pane(&screen).iter().any(|l| l.contains("the sidebar keeps a fixed order")),
        "Up walks up to the first reply. The pane is:\n{}",
        messages_pane(&screen).join("\n")
    );
    assert!(
        messages_pane(&screen).first().is_some_and(|l| l.contains("#1") && l.contains("hello from c-general")),
        "the root stays pinned to the top. The pane is:\n{}",
        messages_pane(&screen).join("\n")
    );

    // Right inside a thread opens nothing: the cursor stays on the reply it is on.
    s.press("right");
    let screen = s.screen();
    assert!(header(&screen).contains("› thread"), "Right inside a thread opens no other thread: {:?}", header(&screen));
    assert!(
        cursor_row(&screen).is_some_and(|l| l.contains("#2")),
        "and leaves the cursor where it was. The pane is:\n{}",
        messages_pane(&screen).join("\n")
    );

    // Left leaves the thread in one go — it does not walk up to the root first, because the root is the pinned
    // header rather than a row — and the cursor lands on the message the thread hangs off, back in the channel.
    s.press("left");
    let screen = s.until("the channel again, on the message the thread hangs off", |sc| {
        header(sc).contains("#general") && !header(sc).contains("› thread") && cursor_row(sc).is_some_and(|l| l.contains("#1"))
    });
    assert!(header(&screen).contains("#general"), "Left leaves the thread for the channel: {:?}", header(&screen));
    assert!(
        messages_pane(&screen).iter().any(|l| l.contains("hello from c-general")),
        "and the cursor is back on the message the thread hangs off. The pane is:\n{}",
        messages_pane(&screen).join("\n")
    );
    s.expect("the thread summary again", "2 replies");
}

#[test]
fn the_arrows_leave_a_message_with_no_thread_alone() {
    let Some(relay) = relay_or_skip("the_arrows_leave_a_message_...") else { return };
    let s = session(&relay);
    // #design opens by itself, and its one message has no replies: neither arrow opens anything.
    s.expect("the design channel", "hello from c-design");
    s.press("tab"); // the channel list -> the messages
    s.expect("the messages pane", "Open thread");
    for key in ["right", "left"] {
        s.press(key);
        let screen = s.screen();
        assert!(!header(&screen).contains("› thread"), "{key} on a message with no replies opens no thread: {:?}", header(&screen));
        assert!(
            cursor_row(&screen).is_some_and(|l| l.contains("#1")) && messages_pane(&screen).iter().any(|l| l.contains("hello from c-design")),
            "{key} leaves the cursor on the message. The pane is:\n{}",
            messages_pane(&screen).join("\n")
        );
    }
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

/// `x` archives the channel under the bar: its row leaves the list, and a search result is the way back. The
/// cursor starts on the channel the app opened by itself (#design), so `x` archives what is on screen.
#[test]
fn x_archives_the_channel_under_the_bar_and_a_search_result_brings_it_back() {
    let Some(relay) = relay_or_skip("x_archives_...") else { return };
    let s = session(&relay);
    assert_eq!(bar_on(&s.screen()).as_deref(), Some("#design"), "the bar starts on the open channel");
    s.press("x");
    let screen = s.until("#design to leave the sidebar", |sc| !sidebar(sc).iter().any(|n| n == "#design"));
    assert!(sidebar(&screen).iter().any(|n| n == "#dev"), "the other channels are still listed:\n{screen}");
    assert!(!header(&screen).contains("#design"), "the app stepped off the archived channel: {:?}", header(&screen));

    // Ctrl+F still matches it: its row says so, and Enter brings it back and opens it.
    s.press("ctrl+f");
    s.expect("the search overlay", "Search · Enter opens");
    s.type_text("design");
    s.expect("the archived row in the search", "archived · Enter brings it back");
    s.expect("the relay to answer the search", "hello from c-design");
    s.press("enter");
    let screen = s.until("#design to be back in the sidebar", |sc| sidebar(sc).iter().any(|n| n == "#design"));
    assert!(sidebar(&screen).iter().any(|n| n == "#design"), "the row is back:\n{screen}");
    assert!(header(&screen).contains("#design"), "and the channel was opened: {:?}", header(&screen));
}

/// A channel we are not in opens read-only: its composer is a Join button, not a text field. Ctrl+F says the
/// same about the channel first (the relay's roster), and Enter on the button sends the join.
#[test]
fn a_channel_we_are_not_in_opens_with_a_join_button_in_place_of_the_composer() {
    let Some(relay) = relay_or_skip("a_channel_we_are_not_in_opens_...") else { return };
    let s = session(&relay);
    // Ctrl+F finds #random; its row carries the relay's roster (who is in it) and says what Enter does.
    s.press("ctrl+f");
    s.expect("the search overlay", "Search · Enter opens");
    s.type_text("random");
    s.expect("the channel row to count the members the relay named", "2 members");
    s.expect("and to say we are not one of them", "not joined · Enter opens");
    // Enter opens the row on the cursor, which needs the search to have run: wait for the relay's answer (its message
    // hit) first, or the Enter just starts the search.
    s.expect("the relay's answer to the search", "hello from c-random");
    s.press("enter");
    let screen = s.until("the channel to open", |sc| header(sc).contains("#random"));
    assert!(screen.contains("[ Join ]"), "the composer is a Join button:\n{screen}");
    assert!(screen.contains("join this channel"), "and says what it is:\n{screen}");
    assert!(!screen.contains("Type a message"), "there is no text field to type into:\n{screen}");
    // The button takes the focus the way the input does, and the footer names it.
    s.press("tab"); // messages -> input
    s.expect("the footer to name the join key", "Join  esc  Messages");
    // Enter on the button sends the join. The relay's rooms carry placeholder ids, not the UUIDs the protocol
    // uses, so the send is refused and reported; the point here is that the button is wired to the join at all.
    s.press("enter");
    s.expect("the join to be attempted and its refusal reported", "could not join the channel");
    // The channel is still not ours, so the button is still there, and Esc leaves it for the messages.
    s.expect("the join button to stay", "join this channel");
    s.press("esc");
    s.expect("the messages footer", "Open thread");
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
fn f2_opens_nothing() {
    let Some(relay) = relay_or_skip("f2_opens_nothing") else { return };
    let s = session(&relay);
    // There is no help screen any more: F2 leaves the screen as it is and the keyboard on the channel list.
    s.expect("the channel list to have focus", "Open channel");
    s.press("f2");
    s.expect("the channel list still has the keyboard, F2 opened nothing", "Open channel");
    s.press("down");
    s.until("the cursor to move on, so F2 left nothing modal behind", |sc| bar_on(sc).as_deref() == Some("#dev"));
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
fn a_draft_comes_back_with_its_channel_and_is_still_there_after_a_restart() {
    let Some(relay) = relay_or_skip("a_draft_comes_back_...") else { return };
    let mut s = session(&relay);
    let config = s.config.clone();
    s.press_all(&["tab", "tab"]); // channels -> messages -> input
    s.expect("the input box to take focus", "Send");
    s.type_text("half a thought");
    s.expect("the draft in the box", "half a thought");
    // Leave for #dev without sending: the box clears, and the draft stays behind in #design.
    s.press("esc"); // input -> messages
    s.press("esc"); // -> the channel list
    s.expect("the channel list", "Open channel");
    s.press_all(&["down", "enter"]);
    s.expect("the dev channel", "hello from c-dev");
    s.expect_gone("the other channel's draft", "half a thought");
    // Back in #design it is in the box again.
    s.press("esc");
    s.expect("the channel list again", "Open channel");
    s.press_all(&["up", "enter"]);
    s.expect("the design channel again", "hello from c-design");
    s.expect("the draft back in the box", "half a thought");
    // Quit, and start again on what it left behind: the draft comes back with the channel.
    s.press("ctrl+q");
    s.wait_exit(Duration::from_secs(10)).expect("buzzr should exit on Ctrl+Q");
    let written = std::fs::read_to_string(config.join("drafts.json")).expect("the draft is on disk");
    assert!(written.contains("half a thought"), "the draft is stored for the channel it was typed in: {written}");
    drop(s); // the session takes its config directory with it
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("drafts.json"), &written).unwrap();
    let again = session_in(&relay, config);
    again.expect("the channel it opens by itself", "#design");
    again.expect("the draft it was left holding", "half a thought");
}

/// A message that `@mentions` someone the channel does not have is held back, and the status line asks whether to
/// invite them; Esc answers by keeping the message in the input. The fake relay serves no member list for an open
/// channel, so a person whose profile it seeds (carol, the author of #design's message) is not a member of the channel
/// the app opens by itself, and a mention of her is a mention of someone outside it.
///
/// The send itself cannot be driven here: a message is built from a channel *uuid*, and the relay's ids (`c-design`)
/// are not uuids, so a send ends in `failed: not a channel id` — the same wall the join/leave paths meet. What this case
/// is about is the app's own screen: the question, and the keys that answer it.
#[test]
fn a_mention_of_someone_outside_the_channel_asks_whether_to_invite_them() {
    let Some(relay) = relay_or_skip("a_mention_of_someone_outside_...") else { return };
    let s = session(&relay);
    // The names arrive with the profiles; the DM rows are relabelled once a peer's has, so wait until carol is named.
    s.until("the DMs to be named after the other member", |sc| sidebar(sc).iter().any(|n| n == "@carol"));
    s.press_all(&["tab", "tab"]); // channels -> messages -> input
    s.expect("the input box to take focus", "Send");
    s.type_text("@carol: can you look?");
    s.press("enter");
    // Nothing goes out: the message is held out of the input while the status line asks.
    s.expect("the question about inviting carol", "Invite carol to #design?");
    s.expect_gone("the held message to leave the input while the question is up", "@carol: can you look?");
    // Esc answers it by keeping the message: the text comes back, so it can be changed.
    s.press("esc");
    s.expect("the status line to say the message was not sent", "not sent");
    s.expect("the message to come back to the input", "@carol: can you look?");
    s.expect_gone("the question to clear when it is answered", "Invite carol to #design?");
    // Enter asks again, and `n` answers: the question is gone once it has an answer.
    s.press("enter");
    s.expect("the question again", "Invite carol to #design?");
    s.press("n");
    s.expect_gone("the question to clear once it is answered", "Invite carol to #design?");
}

/// A name none of the loaded channels has ever shown is looked up in the relay's directory. `dave` is seeded there with a
/// profile, in no room and in no message, so the lookup is the only way to reach him: the case a client's own table of
/// names cannot answer. The card is the whole path — the app asked the directory, it named one person, and the card is his
/// (name *and* about line, both only the relay had).
///
/// The invite itself cannot be driven here (see the case above). The second half asks for a name nobody has at all: the
/// fake relay matches no names and answers a `search` filter with everything it holds, so acting on the wrong person
/// there would show up as a card for somebody else rather than as "nobody here is called".
#[test]
fn a_name_no_channel_has_shown_is_looked_up_in_the_relay_directory() {
    let Some(relay) = relay_or_skip("a_name_no_channel_has_shown_...") else { return };
    let s = session(&relay);
    s.press_all(&["tab", "tab"]); // channels -> messages -> input
    s.expect("the input box to take focus", "Send");
    run_in_palette(&s, "whois", "dave");
    s.expect("the card titled with the name the directory gave", "dave · Esc closes");
    s.expect("and his about line, which only the relay had", "I write things down");
    s.press("esc");
    run_in_palette(&s, "whois", "nobodyish");
    s.expect("a name nobody has to be said plainly, not filled in with whoever the relay sent", "nobody here is called nobodyish");
    s.expect_gone("and no card opens for it", "· Esc closes");
}

#[test]
fn ctrl_k_opens_the_command_palette_and_enter_runs_the_command() {
    let Some(relay) = relay_or_skip("ctrl_k_opens_the_command_palette_...") else { return };
    let s = session(&relay);
    s.press_all(&["tab", "tab"]); // channels -> messages -> input
    s.expect("the input box to take focus", "Send");
    // Ctrl+K opens the palette with the whole list
    s.press("ctrl+k");
    s.expect("the command palette", "Commands · Enter runs · Esc closes");
    s.expect("a command with a line saying what it does", "archive this channel");
    s.expect("the footer to follow the palette", "Run");
    // typing narrows the list to the names that still match
    s.type_text("plugins");
    s.expect("the list narrowed to plugins", "what is loaded, and the commands plugins added");
    s.expect_gone("the other commands dropped out", "load older messages");
    // Enter runs the command on the cursor line: plugins opens its panel
    s.press("enter");
    s.expect("Enter to run the command", "Plugins · Up/Down · Enter runs · Esc closes");
    s.press("esc");
    s.expect_gone("the plugin panel to close", "Plugins · Up/Down · Enter runs · Esc closes");

    // a command that takes an argument asks for it in a step of its own, then runs with what was typed
    s.press("ctrl+k");
    s.type_text("search");
    s.expect("the palette for search", "Commands · Enter runs · Esc closes");
    s.press("enter");
    s.expect("the argument step", "search · Enter runs · Esc goes back");
    s.expect("the argument it takes", "search channels, people and messages");
    s.type_text("sailing");
    s.press("enter");
    s.expect("the command to run with its argument", "Search · Enter opens");
}

/// The palette takes a whole command line, not just a command name: `whois @dave` and Enter run the command in one step,
/// with no separate argument step. The `@` is how a person is addressed in the message list and is not part of the name,
/// so the relay's directory is asked for `dave` — a person only the directory has (see the case above).
#[test]
fn a_whole_command_line_typed_into_the_palette_runs_in_one_step() {
    let Some(relay) = relay_or_skip("a_whole_command_line_typed_into_the_palette_runs_in_one_step") else { return };
    let s = session(&relay);
    s.press_all(&["tab", "tab"]); // channels -> messages -> input
    s.expect("the input box to take focus", "Send");
    s.press("ctrl+k");
    s.expect("the command palette", "Commands · Enter runs · Esc closes");
    s.type_text("whois @dave");
    s.expect("the list narrowed to the command at the head of the line", "somebody's profile");
    s.expect_gone("the other commands dropped out", "load older messages");
    s.press("enter"); // one Enter: the whole line runs as it stands
    s.expect("the command to run with the name the line carried", "dave · Esc closes");
    s.expect("the directory to have been asked for the bare name", "I write things down");
}

/// The palette has a query of its own, so it never touches the composer: a leading slash is ordinary text, and a draft
/// left in the box is exactly as it was when the palette closes.
#[test]
fn the_command_palette_leaves_the_composer_alone() {
    let Some(relay) = relay_or_skip("the_command_palette_leaves_the_composer_alone") else { return };
    let s = session(&relay);
    s.press_all(&["tab", "tab"]); // channels -> messages -> input
    s.expect("the input box to take focus", "Send");
    // a slash is plain text now: no palette, just what was typed
    s.type_text("/notacommand");
    s.expect("the text in the composer", "/notacommand");
    s.expect_gone("no palette for a leading slash", "Commands · Enter runs · Esc closes");
    // Ctrl+K opens the palette over the draft, and Esc closes it again without disturbing the draft
    s.press("ctrl+k");
    s.expect("the command palette", "Commands · Enter runs · Esc closes");
    s.expect("the footer to follow the palette", "Close");
    s.press("esc");
    s.expect_gone("the palette to close", "Commands · Enter runs · Esc closes");
    s.expect("the composer to keep the keyboard", "Send");
    s.expect("the draft untouched", "/notacommand");
}

/// `agents` and the "new agent" flow, all through the real UI: the list opens from the composer, Enter on its
/// "new agent…" row turns the composer into the flow's box (a name, then what the agent should do), the harness is
/// asked what it offers (its models on a panel, a model's thinking levels on another), and the pick makes a local
/// agent — a key of its own, a folder of its own, and a record on disk — which the list then shows. The flow reads and
/// writes only the config directory, so a session with its own temp config is safe to run it in. The harness is the
/// fake on `PATH` (see `fake_acp_dir`), since no real one is installed here.
#[test]
fn agents_are_listed_and_a_new_one_is_created_through_the_composer() {
    let Some(relay) = relay_or_skip("agents_are_listed_and_a_new_one_...") else { return };
    // `agents` is a compiled-in plugin, and `session` starts with `--no-plugins`, so this one starts with plugins on.
    let mut s = session_with_plugins(&relay);
    let config = s.config.clone();
    s.press_all(&["tab", "tab"]); // channels -> messages -> input
    s.expect("the input box to take focus", "Send");
    // nothing made yet: the panel says so, and its last row makes one
    run_in_palette(&s, "agents", "");
    s.expect("the local agents panel", "local agents");
    s.expect("the empty list", "No agents yet.");
    s.expect("the row that starts one", "new agent…");
    // Enter on that row turns the composer into the flow's box, asking first for a name
    s.press("end");
    s.press("enter");
    s.expect("the composer to ask for a name", "new agent · name");
    s.expect("the hint for the first step", "New agent: type a name, then Enter");
    s.expect_gone("the panel to give the composer back", "No agents yet.");
    // the name takes the Enter, and the second step asks what the agent should do
    s.type_text("Scout");
    s.press("enter");
    s.expect("the composer to ask what it should do", "new agent · instructions");
    s.expect("the question, naming the agent", "What should Scout do?");
    // the instructions ask the harness what it offers, and its models come up on a panel, the one it is on marked
    s.type_text("watch the repo");
    s.press("enter");
    s.expect("the models panel", "new agent · model");
    s.expect("a model the harness listed", "Opus");
    // pick another model: the harness is asked again, for that model's own thinking levels
    s.press("down");
    s.press("enter");
    s.expect("the levels panel", "new agent · level");
    // the level makes it: the list comes up on the new agent, with its harness and its own folder
    s.press("enter");
    s.expect("the confirmation", "Scout created");
    s.expect("the agent in the list", "Scout");
    s.expect("its harness", "claude-agent-acp");
    s.expect("the folder of its own beside the record", "agents/scout");
    s.press("esc");
    s.expect_gone("the panel to close", "local agents");
    // it is on disk, with the harness and the picks the flow took, so a fresh run on the same config lists it again
    s.press("ctrl+q");
    s.wait_exit(Duration::from_secs(10)).expect("buzzr should exit on Ctrl+Q");
    let written = std::fs::read_to_string(config.join("agents.json")).expect("the agent is on disk");
    assert!(written.contains("Scout") && written.contains("claude-agent-acp"), "the record names the agent and its harness: {written}");
    assert!(written.contains("\"model\": \"opus\""), "the record keeps the model the flow picked: {written}");
    assert!(written.contains("\"effort\": \"low\""), "and the thinking level it picked: {written}");
    assert!(written.contains("nsec1"), "the agent keeps its own key in the file: {written}");
    // The relay kept what buzzr published: the agent's own kind-0 profile (carrying the owner's attestation) and the
    // owner's kind:30177 listing of it both come back out of a query. This needs a relay that keeps writes.
    let agent = agent_pubkey(&written);
    let profile = relay_query(&relay, &format!(r##"[{{"kinds":[0],"authors":["{agent}"]}}]"##));
    assert!(profile.contains("Scout"), "the agent's own profile came back out of the relay: {profile}");
    assert!(profile.contains("\"auth\""), "and it carries the owner's attestation tag: {profile}");
    let listing = relay_query(&relay, &format!(r##"[{{"kinds":[30177],"#d":["{agent}"]}}]"##));
    assert!(listing.contains("Scout"), "the owner's listing came back out of the relay: {listing}");
    assert!(listing.contains("buzz.claude-code/claude-code"), "naming the agent type it runs: {listing}");
    drop(s); // the session takes its config directory with it
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("agents.json"), &written).unwrap();
    let again = session_in_with_plugins(&relay, config);
    again.press_all(&["tab", "tab"]);
    run_in_palette(&again, "agents", "");
    again.expect("the agent made before the restart", "Scout");
    again.expect("its harness still named", "claude-agent-acp");
}

/// Deleting a local agent, through the real UI: the list offers a row that deletes each agent, which asks to confirm
/// before anything happens; confirming takes the agent off the relay (its own profile and the owner's listing of it,
/// both by NIP-09) and then deletes its record here. The flow reads and writes only the config directory and needs no
/// harness, so this case runs on the fake on `PATH` only to make the agent in the first place.
#[test]
fn an_agent_is_deleted_from_the_panel_after_a_confirmation() {
    let Some(relay) = relay_or_skip("an_agent_is_deleted_from_the_panel_...") else { return };
    let mut s = session_with_plugins(&relay);
    let config = s.config.clone();
    s.press_all(&["tab", "tab"]);
    // make one through the flow, the same steps the create case takes
    run_in_palette(&s, "agents", "");
    s.expect("the empty list", "No agents yet.");
    s.press("end");
    s.press("enter");
    s.type_text("Scout");
    s.press("enter");
    s.type_text("watch the repo");
    s.press("enter");
    s.expect("the models panel", "new agent · model");
    s.press("enter"); // take the model in force
    s.press("enter"); // take the level in force, which makes it
    s.expect("the confirmation", "Scout created");
    s.expect("the list to come back", "new agent…");
    // the list offers to delete it; Enter on that row asks to confirm first, and only a second Enter deletes
    s.press_all(&["down", "down"]);
    s.press("enter");
    s.expect("the confirmation panel", "delete Scout");
    s.expect("what the confirmation does", "take it off the relay and delete its key here");
    s.press("enter");
    s.expect("the deletion reported", "Scout deleted");
    s.expect("the list without it", "No agents yet.");
    s.press("esc");
    // it is gone from disk: nothing lists it, and its key went with it
    s.press("ctrl+q");
    s.wait_exit(Duration::from_secs(10)).expect("buzzr should exit on Ctrl+Q");
    let written = std::fs::read_to_string(config.join("agents.json")).unwrap_or_default();
    assert!(!written.contains("Scout"), "the deleted agent is out of the record: {written}");
    assert!(!written.contains("nsec1"), "and its key is gone with it: {written}");
    // the relay took both NIP-09 deletions: the agent's own profile, and the owner's listing of it
    let deletions = relay_query(&relay, r#"[{"kinds":[5]}]"#);
    assert!(deletions.contains("\"kind\":5"), "a deletion reached the relay: {deletions}");
    assert!(deletions.contains("30177:"), "the owner's listing was deleted by coordinate: {deletions}");
    assert!(deletions.contains("0:"), "and so was the agent's own profile: {deletions}");
}

/// A message that names a local agent is answered by it. The agent an `agents create` makes is only an identity and a
/// folder until something runs it: the **runner** (`src/runner.rs`, started with the app) watches `agents.json` and, for
/// every message that carries the agent in a `p` tag, runs its harness over ACP and publishes what the harness answered
/// back to the channel the mention named, as the agent's own message. This case drives that whole loop on the real
/// binary: the agent is made through the composer, the mention reaches the relay, the answer comes back on it, and the
/// status line says the agent replied.
///
/// The mention is published by the test rather than typed into the composer, for the reason the invite case gives: a
/// message is built from a channel *uuid* and the relay's ids are not uuids, so a message for a channel of the relay
/// cannot be built at all here (and a composer cannot mention someone who is not in the channel anyway). From the relay
/// in, every step is the real one — the runner reads the mention over HTTP with the agent's own key, starts the harness
/// in the agent's folder, and publishes the answer signed as the agent.
#[test]
fn a_message_that_names_a_local_agent_is_answered_by_it() {
    let Some(relay) = relay_or_skip("a_message_that_names_a_local_agent_...") else { return };
    // The runner is part of the app and `agents` is a compiled-in plugin, so this session starts with plugins on.
    let s = session_with_plugins(&relay);
    let config = s.config.clone();
    s.press_all(&["tab", "tab"]); // channels -> messages -> input
    s.expect("the input box to take focus", "Send");
    // make an agent, the way the case above does: a name, then what it should do, then a model and a level
    run_in_palette(&s, "agents", "create");
    s.expect("the composer to ask for a name", "new agent · name");
    s.type_text("Scout");
    s.press("enter");
    s.expect("the composer to ask what it should do", "new agent · instructions");
    s.type_text("watch the repo");
    s.press("enter");
    s.expect("the models panel", "new agent · model");
    s.press("enter");
    s.expect("the levels panel", "new agent · level");
    s.press("enter");
    s.expect("the agent to be made", "Scout created");
    s.press("esc");
    let written = std::fs::read_to_string(config.join("agents.json")).expect("the agent is on disk");
    let agent = agent_pubkey(&written);
    // The runner re-reads agents.json every couple of seconds and a watch answers only what arrives after it starts, so
    // give it the beat it needs before anything is sent: the mention below must land after the watch began.
    std::thread::sleep(Duration::from_secs(3));
    let asker = nostr::Keys::parse(KEY).expect("the test key").public_key().to_hex();
    publish_message(&relay, TEST_CHANNEL, "@Scout what is new?", std::slice::from_ref(&agent), None);
    // The turn blocks on the harness off the event loop, so the status line is what says it happened.
    s.until("the agent to have replied", |sc| sc.contains("Scout replied"));
    // And the answer is on the relay: it is the harness's own text, signed as the agent, in the channel the mention
    // named, naming the person who asked so they are told about it. A relay that keeps what it is sent is needed here.
    let replies = relay_query(&relay, &format!(r#"[{{"kinds":[9],"authors":["{agent}"]}}]"#));
    assert!(replies.contains("heard:"), "the answer is what the harness streamed back: {replies}");
    assert!(replies.contains("@Scout what is new?"), "and it is the answer to that mention: {replies}");
    assert!(replies.contains("(permission: yes)"), "the turn ran to its end — the fake asks permission and the client allows it: {replies}");
    assert!(replies.contains(TEST_CHANNEL), "the answer is in the channel the mention came from: {replies}");
    assert!(replies.contains(&asker), "and it names the person who asked: {replies}");
}

/// A thread mention reaches the agent with the thread in hand. The runner hands the harness the earlier messages of the
/// mention's thread — the root and the replies that came before it, the same window the Buzz app gives its agents — so a
/// turn that joins a thread reads the conversation instead of one message out of it. The loop is the case above; what
/// this one adds is that the mention is a reply, and that the thread's earlier message is in the prompt the harness saw.
#[test]
fn a_local_agent_answers_a_thread_mention_with_the_thread_in_hand() {
    let Some(relay) = relay_or_skip("a_local_agent_answers_a_thread_mention_...") else { return };
    let s = session_with_plugins(&relay);
    let config = s.config.clone();
    s.press_all(&["tab", "tab"]);
    s.expect("the input box to take focus", "Send");
    run_in_palette(&s, "agents", "create");
    s.expect("the composer to ask for a name", "new agent · name");
    s.type_text("Scout");
    s.press("enter");
    s.expect("the composer to ask what it should do", "new agent · instructions");
    s.type_text("watch the repo");
    s.press("enter");
    s.expect("the models panel", "new agent · model");
    s.press("enter");
    s.expect("the levels panel", "new agent · level");
    s.press("enter");
    s.expect("the agent to be made", "Scout created");
    s.press("esc");
    let written = std::fs::read_to_string(config.join("agents.json")).expect("the agent is on disk");
    let agent = agent_pubkey(&written);
    // The watch answers only what arrives after it starts, so give the runner the beat it needs first.
    std::thread::sleep(Duration::from_secs(3));
    // A thread of the test's own: a root, and a reply that names the agent.
    let root = publish_message(&relay, TEST_CHANNEL, "the release is next week", &[], None);
    publish_message(&relay, TEST_CHANNEL, "@Scout when exactly?", std::slice::from_ref(&agent), Some(&root));
    s.until("the agent to have replied", |sc| sc.contains("Scout replied"));
    // The answer is the harness's own text, which quotes the prompt it was given: the thread's earlier message being in
    // it is what says the agent was handed the thread it was answering in.
    let replies = relay_query(&relay, &format!(r#"[{{"kinds":[9],"authors":["{agent}"]}}]"#));
    assert!(replies.contains("@Scout when exactly?"), "the answer is to the mention: {replies}");
    assert!(replies.contains("the release is next week"), "and the thread it joined came with it: {replies}");
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
fn a_reacts_to_the_selected_message_from_the_keyboard() {
    let Some(relay) = relay_or_skip("a_reacts_to_the_selected_...") else { return };
    let s = session(&relay);
    // #design is open with the keyboard on the channel list; Tab moves it to the message list.
    s.press("tab");
    s.expect("the messages pane", "Open thread");
    s.expect("the footer to name the reaction key", "React");
    s.press("a");
    s.expect("the reaction picker", "React · Enter sends · Esc closes");
    s.expect("its search line", "Search:");
    s.expect("the footer to follow the picker", "Send");
    s.expect("the list to offer emoji by name", "thumbs up yes like approve +1");
    // typing narrows the list to the names that match
    s.type_text("fire");
    s.expect("the list to narrow to the match", "fire hot lit");
    s.expect_gone("the other emoji to drop out of the list", "thumbs up yes like approve +1");
    s.press("enter");
    // The relay accepts a kind:7 like any other event; what the screen shows is that it went out and that the
    // picker closed.
    let scr = s.until("the status line to confirm the reaction", |sc| sc.contains("reacted 🔥"));
    assert!(!scr.contains("failed"), "the relay accepted the reaction:\n{scr}");
    s.expect_gone("the reaction picker to close", "React · Enter sends");
    std::thread::sleep(Duration::from_millis(500)); // a rejection would replace the status line
    let scr = s.screen();
    assert!(!scr.contains("failed"), "the reaction was not rejected:\n{scr}");
    // Esc on the message list steps back to the channels, as it does outside the picker.
    s.press("esc");
    s.expect("the channel list again", "Open channel");
}

#[test]
fn the_pane_flags_the_newest_reply_only_while_it_is_off_screen() {
    let Some(relay) = relay_or_skip("the_pane_flags_the_newest_...") else { return };
    // In a pane shorter than the thread, the marker follows what is on screen rather than where the cursor is, and
    // the root stays pinned above it. #general carries the relay's only multi-message list: a message and two replies.
    let s = Session::start_short(&relay);
    s.press_all(&["down", "down", "enter"]);
    s.expect("the general channel", "hello from c-general");
    s.expect("its thread summary", "2 replies");
    s.press("enter"); // open the thread: it starts at the bottom, on the newest reply
    s.expect("the thread view", "› thread");
    s.expect_gone("no marker while the newest reply is on screen", "more below");
    // At the bottom of the thread the newest reply is on screen and the root is still the pane's first row: it is
    // pinned, not a row that scrolls away with the replies.
    let bottom = messages_pane(&s.screen());
    assert!(
        bottom.iter().any(|l| l.contains("and Enter opens the thread")),
        "the newest reply is on screen at the bottom. The pane is:\n{}",
        bottom.join("\n")
    );
    assert!(
        bottom.first().is_some_and(|l| l.contains("#1") && l.contains("hello from c-general")),
        "the root stays pinned to the top even at the thread's bottom. The pane is:\n{}",
        bottom.join("\n")
    );
    // Jump to the top of the thread: the newest reply is now below the view, so the pane says so.
    s.press("home");
    s.expect("a marker once the newest reply is off screen", "more below");
    // The pinned root header is the first row of the pane the whole time, even as the replies scroll under it.
    let pinned = messages_pane(&s.screen()).first().cloned().unwrap_or_default();
    assert!(pinned.contains("#1") && pinned.contains("hello from c-general"), "the root stays pinned at the top: {pinned:?}");
    s.press("end");
    s.expect_gone("the marker again once the newest reply is back on screen", "more below");
}

#[test]
fn a_mark_survives_leaving_the_channel_and_coming_back() {
    let Some(relay) = relay_or_skip("a_mark_survives_leaving_...") else { return };
    let s = session(&relay);
    // #design is open with the keyboard on the channel list; Tab moves it into the messages, and `u` marks the
    // message the cursor is on — a mark on that message, not a move of the read position.
    s.press("tab");
    s.expect("the messages pane", "Open thread");
    s.press("u");
    s.until("the open channel to show an unread count", |sc| sidebar(sc).iter().any(|n| n.starts_with("#design (1)")));
    // Leave for #dev and come back: opening a channel reads it, but reading does not clear what was marked.
    // Enter opens a channel and moves the keyboard into its messages, so Esc returns to the list before `Up`.
    s.press("esc");
    s.expect("the channel list", "Open channel");
    s.press_all(&["down", "enter"]);
    s.expect("the dev channel", "hello from c-dev");
    s.press("esc");
    s.expect("the channel list again", "Open channel");
    s.press_all(&["up", "enter"]);
    s.expect("the design channel again", "hello from c-design");
    let screen = s.until("the mark to still be there", |sc| sidebar(sc).iter().any(|n| n.starts_with("#design (1)")));
    assert!(screen.contains("new messages"), "the divider still sits above the marked message:\n{screen}");
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

#[test]
fn a_thread_reply_marks_its_channel_with_a_circle_not_a_count() {
    let Some(relay) = relay_or_skip("a_thread_reply_marks_its_channel_...") else { return };
    // #general carries a thread with two replies, both newer than its own message. Read the channel and its
    // thread once (so both positions are stored), then restart with the thread position far in the past: only
    // the replies are unread, and nothing on the channel's own timeline is.
    let first = session(&relay);
    first.press_all(&["down", "down", "enter"]);
    first.expect("the general channel", "hello from c-general");
    first.press("enter"); // open the thread, which records its own read position
    first.expect("the thread view", "the sidebar keeps a fixed order");
    let stored = wait_for_stored_channel(&first.config.join("read-state.json"), "c-general");
    let root = wait_for_stored_thread(&first.config.join("read-state.json"));
    let at = stored_position(&stored, "c-general");
    let config = first.config.clone();
    drop(first); // kills the app and clears the config directory
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("read-state.json"), format!("{{\"channels\":{{\"c-general\":{at}}},\"threads\":{{\"{root}\":1}}}}")).unwrap();

    let s = session_in(&relay, config);
    // #design opens on start, so #general is off screen and its count is asked from the relay. Nothing arrived on
    // its timeline, so it is not a *counted* unread — a thread reply is not a channel row.
    s.expect("the channel list", "#general");
    std::thread::sleep(Duration::from_millis(800)); // long enough for a startup count to arrive if one were coming
    let screen = s.screen();
    assert!(
        !sidebar(&screen).iter().any(|row| row.starts_with("#general (")),
        "a thread reply must not count as a channel unread:\n{}\n{screen}",
        ruler()
    );
    // Open the channel: the message list marks the thread, and the channel row now says the same thing — one circle,
    // still no count.
    s.press_all(&["down", "down", "enter"]);
    s.expect("the general channel", "hello from c-general");
    s.expect("the thread's own mark", "● 2 new");
    let screen = s.until("the channel row's circle", |sc| sidebar(sc).iter().any(|row| row == "#general ●"));
    assert!(
        !sidebar(&screen).iter().any(|row| row.starts_with("#general (")),
        "the row carries the circle without a count:\n{screen}"
    );
    // Reading the thread clears both markers together.
    s.press("enter");
    s.expect("the thread view", "the sidebar keeps a fixed order");
    s.press("esc");
    s.expect_gone("the thread's message mark", "● 2 new");
    s.until("the channel row's circle to clear", |sc| !sidebar(sc).iter().any(|row| row == "#general ●"));
}

/// The `created_at` stored for `channel` in a `read-state.json` body (the file is pretty-printed, so the value is after
/// the key's colon and some whitespace).
fn stored_position(text: &str, channel: &str) -> u64 {
    let needle = format!("\"{channel}\"");
    let i = text.find(&needle).expect("the channel is in the read state") + needle.len();
    text[i..].chars().skip_while(|c| *c == ':' || c.is_ascii_whitespace()).take_while(char::is_ascii_digit).collect::<String>().parse().expect("a position")
}

/// The whole `read-state.json` once it records a position for `channel`, waiting for the file to appear.
fn wait_for_stored_channel(path: &std::path::Path, channel: &str) -> String {
    let end = Instant::now() + WAIT;
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if text.contains(&format!("\"{channel}\"")) {
                return text;
            }
        }
        if Instant::now() > end {
            panic!("read-state.json never recorded a position for {channel}");
        }
        std::thread::sleep(Duration::from_millis(40));
    }
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
fn alt_enter_adds_a_line_where_the_terminal_cannot_report_shift_enter() {
    let Some(relay) = relay_or_skip("alt_enter_adds_a_line_...") else { return };
    // This pty never answers the keyboard-enhancement probe, so the app cannot see Shift+Enter at all and must
    // name the key this terminal can actually send.
    let s = session(&relay);
    s.press_all(&["tab", "tab"]); // channels -> messages -> input
    s.expect("the input box to take focus", "Send");
    s.expect("the footer to offer the new-line key this terminal can report", "Alt+⏎");
    assert!(!s.screen().contains("Shift+⏎"), "a terminal that never reports Shift+Enter is not offered it:\n{}", s.screen());
    s.type_text("one");
    s.press("alt+enter");
    s.type_text("two");
    // Two lines in the composer: Alt+Enter added a line rather than sending. Had it sent, the box would hold
    // only "two" and be back to one line.
    let scr = s.until("the input box to grow to two lines", |sc| composer_height(sc) == 2);
    assert!(scr.contains("one") && scr.contains("two"), "both lines are in the input box:\n{scr}");
    s.press("enter"); // Enter still sends
    s.expect("the input box to empty, so the message went out", "Type a message");
}

#[test]
fn shift_enter_adds_a_line_where_the_terminal_reports_it() {
    let Some(relay) = relay_or_skip("shift_enter_adds_a_line_...") else { return };
    let s = session_enhanced(&relay);
    s.press_all(&["tab", "tab"]); // channels -> messages -> input
    s.expect("the input box to take focus", "Send");
    s.expect("the footer to offer Shift+Enter", "Shift+⏎");
    assert!(!s.screen().contains("Alt+⏎"), "the terminal reports Shift+Enter, so that is the key to offer:\n{}", s.screen());
    s.type_text("one");
    s.press("shift+enter"); // what a kitty-protocol terminal sends for Shift+Enter
    s.type_text("two");
    let scr = s.until("the input box to grow to two lines", |sc| composer_height(sc) == 2);
    assert!(scr.contains("one") && scr.contains("two"), "both lines are in the input box:\n{scr}");
    s.press("enter");
    s.expect("the input box to empty, so the message went out", "Type a message");
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

// ---------------------------------------------------------------------------------------------------
// Plugins: what a plugin puts on screen. Every other session starts with `--no-plugins`; this one turns
// them on and points at a folder of its own, so the check runs through the real host and child process.

/// A plugin that declares a pane and asks for a heartbeat, both in its `ready`, then replaces the
/// pane on every tick — the shape `examples/plugins/clock` uses, with fixed text so the test can
/// assert on it.
const PANE_PLUGIN_MAIN: &str = r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"type":"hello"'*) printf '%s\n' '{"type":"ready","tickMs":250,"contributions":{"pane":{"title":"Demo pane","rows":[{"text":"pane one","tone":"accent"}]}}}' ;;
    *'"type":"tick"'*) printf '%s\n' '{"type":"pane","title":"Demo pane","rows":[{"text":"pane two","tone":"accent"}]}' ;;
  esac
done
"#;

#[test]
fn a_plugin_pane_is_drawn_under_the_channel_list_and_a_heartbeat_refreshes_it() {
    let Some(relay) = relay_or_skip("a_plugin_pane_is_drawn_...") else { return };
    let dir = std::env::temp_dir().join(format!("buzzr-e2e-plugins-{}", std::process::id()));
    let plugin = dir.join("demo");
    std::fs::create_dir_all(&plugin).expect("could not make the plugin folder");
    let entry = plugin.join("main.sh");
    std::fs::write(&entry, PANE_PLUGIN_MAIN).unwrap();
    // buzzr runs the entry file itself, so it has to be executable.
    std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o755)).expect("chmod +x main.sh");

    let s = Session::start_with_plugins(&relay, dir.clone(), ROWS);
    settle(&s);
    // The pane is always on screen: under the channel list, with its title on the rule above its rows, and
    // the channel list keeps its own rows.
    let screen = s.until("the plugin's pane", |sc| sc.contains("pane one"));
    let lines: Vec<&str> = screen.lines().collect();
    let row = lines.iter().position(|l| l.contains("pane one")).expect("the pane row");
    let last_channel = lines.iter().rposition(|l| l.contains("#random")).expect("a channel row");
    assert!(row > last_channel, "the pane sits under the channel list:\n{screen}");
    assert!(screen.contains("Demo pane"), "the pane's title is on its rule:\n{screen}");
    assert!(screen.contains("#design"), "the channel list keeps its rows:\n{screen}");
    // The heartbeat the plugin asked for in its `ready` reaches the child, which replaces the pane from it.
    let screen = s.until("the heartbeat to refresh the pane", |sc| sc.contains("pane two"));
    assert!(!screen.contains("pane one"), "the pane is replaced, not stacked:\n{screen}");
    let _ = std::fs::remove_dir_all(&dir);
}
