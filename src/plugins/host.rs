//! Running plugin processes and carrying messages between them and the app.
//!
//! One tokio task supervises every child: it starts them, enforces the handshake deadline, notices edits in the plugins folder
//! and reloads by starting the new revision next to the old one, swaps only when the new process says it is ready, and never
//! blocks the UI. Nothing here touches `App`; the only things that cross are `PluginEvent`s and `PluginCmd`s.

use crate::app::AppEvent;
use crate::contrib::{PluginCmd, PluginEvent, PluginMeta, PluginState, Projection};
use crate::plugins::discover::{self, DISABLED};
use crate::plugins::proto::{self, FromPlugin, ToPlugin};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

/// A plugin that has not said `ready` by then is not going to work with this buzzr.
pub const HANDSHAKE: Duration = Duration::from_secs(2);
/// How often the plugins folder is checked for edits. Cheap: one directory listing and a few metadata calls.
pub const POLL: Duration = Duration::from_secs(2);
/// An editor writing a file twice must not start two revisions.
pub const DEBOUNCE: Duration = Duration::from_millis(300);
/// How often the runtime looks at which plugins are due a heartbeat. A plugin's `tickMs` is honoured to this
/// granularity, so it is the same number as `discover::MIN_TICK_MS`.
pub const TICK_CHECK: Duration = Duration::from_millis(discover::MIN_TICK_MS);
/// A plugin may post this often, no more. The user always sees it happen. A compiled-in plugin is held to the very
/// same numbers (see `plugin`), so the transport never changes how fast a plugin may talk.
const SEND_INTERVAL: Duration = crate::plugin::SEND_INTERVAL;
const SENDS_PER_MINUTE: usize = crate::plugin::SENDS_PER_MINUTE;
const MAX_LOG: u64 = 1024 * 1024;

enum Msg {
    Snapshot(Projection),
    Cmd(PluginCmd),
    /// After the debounce: the folder changed.
    Reload(String),
    Stop,
}

enum ProcMsg {
    Line { plugin: String, rev: u64, line: String },
    Eof { plugin: String, rev: u64 },
    Failed { plugin: String, rev: u64, why: String },
}

/// The handle the app and the event loop hold. Every method is non-blocking and never fails in the caller's face.
#[derive(Clone)]
pub struct Host {
    tx: UnboundedSender<Msg>,
}

impl Host {
    /// Start supervising the plugins in `root`. Child output goes to `logs`.
    pub fn start(root: PathBuf, logs: PathBuf, events: UnboundedSender<AppEvent>, read_only: bool) -> Host {
        let (tx, rx) = mpsc::unbounded_channel();
        let self_tx = tx.clone();
        tokio::spawn(async move {
            Runtime::new(root, logs, events, read_only, self_tx).run(rx).await;
        });
        Host { tx }
    }

    /// Plugin support is off: every message is dropped.
    pub fn none() -> Host {
        let (tx, _rx) = mpsc::unbounded_channel();
        Host { tx }
    }

    pub fn snapshot(&self, projection: Projection) {
        let _ = self.tx.send(Msg::Snapshot(projection));
    }

    pub fn command(&self, command: PluginCmd) {
        let _ = self.tx.send(Msg::Cmd(command));
    }

    /// Ask every plugin to exit. The children are killed if they do not.
    pub fn shutdown(&self) {
        let _ = self.tx.send(Msg::Stop);
    }
}

struct Proc {
    rev: u64,
    child: Child,
    stdin: UnboundedSender<String>,
    started: Instant,
}

#[derive(Default)]
struct Info {
    /// The file buzzr runs for this plugin; `None` when the folder did not load, and `detail` says why.
    entry: Option<PathBuf>,
    disabled: bool,
    state: PluginState,
    detail: String,
    /// What the plugin called itself when it said `ready`; empty means "call it by its id".
    name: String,
    /// What the plugin asked to be allowed to do, filtered to what this build enforces.
    permissions: Vec<String>,
    /// The heartbeat it asked for in `ready`; `0` means never.
    tick_ms: u64,
    commands: usize,
}

struct Runtime {
    root: PathBuf,
    logs: PathBuf,
    events: UnboundedSender<AppEvent>,
    self_tx: UnboundedSender<Msg>,
    read_only: bool,
    plugins: BTreeMap<String, Info>,
    running: HashMap<String, Proc>,
    pending: HashMap<String, Proc>,
    last_stamp: BTreeMap<PathBuf, SystemTime>,
    last_snapshot: Option<Projection>,
    seq: u64,
    invoke: u64,
    /// When each running plugin last got a heartbeat, and the number to put on the next one.
    ticks: HashMap<String, Instant>,
    tick_seq: u64,
    sends: HashMap<String, Vec<Instant>>,
    published: Vec<PluginMeta>,
}

impl Runtime {
    fn new(root: PathBuf, logs: PathBuf, events: UnboundedSender<AppEvent>, read_only: bool, self_tx: UnboundedSender<Msg>) -> Runtime {
        Runtime {
            root,
            logs,
            events,
            self_tx,
            read_only,
            plugins: BTreeMap::new(),
            running: HashMap::new(),
            pending: HashMap::new(),
            last_stamp: BTreeMap::new(),
            last_snapshot: None,
            seq: 0,
            invoke: 0,
            ticks: HashMap::new(),
            tick_seq: 0,
            sends: HashMap::new(),
            published: Vec::new(),
        }
    }

    async fn run(mut self, mut rx: UnboundedReceiver<Msg>) {
        let (proc_tx, mut proc_rx) = mpsc::unbounded_channel::<ProcMsg>();
        self.rescan(&proc_tx);
        self.publish();
        let mut poll = tokio::time::interval(POLL);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // A separate, faster timer: only a plugin whose own code asked for a heartbeat is woken by it.
        let mut heartbeat = tokio::time::interval(TICK_CHECK);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                Some(msg) = rx.recv() => match msg {
                    Msg::Snapshot(p) => self.snapshot(p),
                    Msg::Cmd(c) => self.command(c),
                    Msg::Reload(id) => self.reload(&id, &proc_tx),
                    Msg::Stop => break,
                },
                Some(p) = proc_rx.recv() => self.on_proc(p),
                _ = poll.tick() => {
                    self.check_deadlines();
                    self.scan_for_changes();
                }
                _ = heartbeat.tick() => self.send_ticks(),
            }
        }
        self.stop_all();
        self.publish();
    }

    // ---- discovery --------------------------------------------------------------------------------------------------------

    fn rescan(&mut self, proc_tx: &UnboundedSender<ProcMsg>) {
        let found = discover::scan(&self.root);
        let seen: Vec<String> = found.iter().map(|d| d.id()).collect();
        let gone: Vec<String> = self.plugins.keys().filter(|id| !seen.contains(id)).cloned().collect();
        for id in gone {
            self.stop(&id);
            self.plugins.remove(&id);
            self.emit(PluginEvent::Gone { id, why: "the plugin folder is gone".into() });
        }
        self.plugins = found
            .into_iter()
            .map(|d| {
                let id = d.id();
                let dir = d.dir;
                let mut info = Info { entry: d.entry.clone(), disabled: d.disabled, ..Info::default() };
                if let Some(entry) = &info.entry {
                    match self.spawn(&id, entry, &dir, 1, proc_tx) {
                        Ok(proc) => {
                            info.state = PluginState::Starting;
                            self.pending.insert(id.clone(), proc);
                        }
                        Err(e) => {
                            info.state = if info.disabled { PluginState::Disabled } else { PluginState::Failed };
                            info.detail = e;
                        }
                    }
                } else {
                    info.state = if info.disabled { PluginState::Disabled } else { PluginState::Failed };
                    info.detail = d.problem.clone().unwrap_or_else(|| "no entry file".into());
                }
                (id, info)
            })
            .collect();
        self.last_stamp = discover::stamps(&self.root);
        // a plugin may have been removed while it was running
        for id in self.running.keys().cloned().collect::<Vec<_>>() {
            if !self.plugins.contains_key(&id) {
                self.stop(&id);
            }
        }
        self.publish();
    }

    fn scan_for_changes(&mut self) {
        // A folder that went away or appeared, and edits inside one, both show up as a changed stamp.
        let current = discover::stamps(&self.root);
        let mut changed: Vec<String> = Vec::new();
        for (dir, stamp) in &current {
            if self.last_stamp.get(dir) != Some(stamp) {
                if let Some(name) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) {
                    changed.push(name);
                }
            }
        }
        if current != self.last_stamp {
            self.last_stamp = current;
        }
        if changed.is_empty() {
            return;
        }
        // Debounce: run the reload after the editor is done, in this task, with the newest files.
        let tx = self.self_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(DEBOUNCE).await;
            for dir in changed {
                let _ = tx.send(Msg::Reload(dir));
            }
        });
    }

    fn reload(&mut self, folder: &str, proc_tx: &UnboundedSender<ProcMsg>) {
        let dir = self.root.join(folder);
        if !dir.is_dir() {
            self.stop(folder);
            if self.plugins.remove(folder).is_some() {
                self.emit(PluginEvent::Gone { id: folder.into(), why: "removed".into() });
            }
            self.publish();
            return;
        }
        let disabled = dir.join(DISABLED).exists();
        match discover::load(&dir) {
            Ok(entry) => {
                let was_running = self.running.contains_key(folder);
                let info = self.plugins.entry(folder.to_owned()).or_insert_with(Info::default);
                info.entry = Some(entry.clone());
                info.disabled = disabled;
                if disabled {
                    self.stop(folder);
                    let info = self.plugins.get_mut(folder).expect("present");
                    info.state = PluginState::Disabled;
                    info.detail.clear();
                    self.publish();
                    return;
                }
                let rev = self.running.get(folder).map(|p| p.rev).unwrap_or(0) + 1;
                // Stop a half-started revision from an earlier edit; the previous revision (if any) keeps running.
                if let Some(mut old) = self.pending.remove(folder) {
                    let _ = old.child.start_kill();
                }
                match self.spawn(folder, &entry, &dir, rev, proc_tx) {
                    Ok(p) => {
                        self.pending.insert(folder.to_owned(), p);
                        let info = self.plugins.get_mut(folder).expect("present");
                        info.state = if was_running { PluginState::Running } else { PluginState::Starting };
                        info.detail = if was_running { "reloading…".into() } else { String::new() };
                    }
                    Err(e) => self.failed(folder, e, true),
                }
            }
            Err(e) => {
                if let Some(info) = self.plugins.get_mut(folder) {
                    info.entry = None;
                    info.state = PluginState::Failed;
                    info.detail = e.clone();
                }
                self.failed(folder, e, true);
            }
        }
        self.publish();
    }

    // ---- processes --------------------------------------------------------------------------------------------------------

    fn spawn(&mut self, id: &str, entry: &Path, dir: &Path, rev: u64, proc_tx: &UnboundedSender<ProcMsg>) -> Result<Proc, String> {
        // The child gets a clean environment: on purpose, so BUZZ_PRIVATE_KEY / BUZZ_AUTH_TAG in buzzr's own environment
        // cannot leak into a plugin. Only what a program needs to run is passed on.
        // The entry is run through its own shebang, so the only thing buzzr decides is which file that is. Canonicalise
        // it: the child is started after buzzr changes into the plugin folder, so a relative path would land elsewhere.
        let entry = std::fs::canonicalize(entry).unwrap_or_else(|_| entry.to_path_buf());
        let mut cmd = Command::new(&entry);
        cmd.kill_on_drop(true)
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(self.log_file(id))
            .env_clear();
        for key in ["PATH", "HOME", "LANG", "LC_ALL", "TZ"] {
            if let Ok(v) = std::env::var(key) {
                cmd.env(key, v);
            }
        }
        cmd.env("BUZZR_PLUGIN_ID", id);
        cmd.env("BUZZR_PLUGIN_DIR", dir);
        if let Some(dir) = self.root.parent() {
            cmd.env("BUZZR_CONFIG_DIR", dir);
        }
        cmd.env("TMPDIR", std::env::temp_dir());
        let mut child = cmd.spawn().map_err(|e| format!("could not start {}: {e}", entry.display()))?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let (line_tx, line_rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(write_stdin(stdin, line_rx));
        let plugin = id.to_owned();
        let tx = proc_tx.clone();
        tokio::spawn(async move { read_stdout(stdout, plugin, rev, tx).await });
        let _ = line_tx.send(
            ToPlugin::Hello { protocol: proto::PROTOCOL, api_version: proto::API_VERSION, host: concat!("buzzr/", env!("CARGO_PKG_VERSION")), plugin: id }.line(),
        );
        self.log(id, &format!("started revision {rev}: {}", entry.display()));
        Ok(Proc { rev, child, stdin: line_tx, started: Instant::now() })
    }

    fn stop(&mut self, id: &str) {
        self.ticks.remove(id);
        for proc in [self.running.remove(id), self.pending.remove(id)].into_iter().flatten() {
            let mut proc = proc;
            let _ = proc.stdin.send(ToPlugin::Bye.line());
            let _ = proc.child.start_kill();
        }
    }

    fn stop_all(&mut self) {
        for id in self.running.keys().chain(self.pending.keys()).cloned().collect::<Vec<_>>() {
            self.stop(&id);
        }
    }

    fn check_deadlines(&mut self) {
        let late: Vec<String> = self.pending.iter().filter(|(_, p)| p.started.elapsed() > HANDSHAKE).map(|(id, _)| id.clone()).collect();
        for id in &late {
            if let Some(mut proc) = self.pending.remove(id) {
                let _ = proc.child.start_kill();
            }
            self.failed(id, "no ready message".into(), true);
        }
        if !late.is_empty() {
            self.publish();
        }
    }

    // ---- messages from plugins -------------------------------------------------------------------------------------------

    fn on_proc(&mut self, msg: ProcMsg) {
        match msg {
            ProcMsg::Line { plugin, rev, line } => self.on_line(&plugin, rev, &line),
            ProcMsg::Eof { plugin, rev } => self.on_exit(&plugin, rev, "the process exited".into()),
            ProcMsg::Failed { plugin, rev, why } => self.on_exit(&plugin, rev, why),
        }
    }

    fn on_line(&mut self, plugin: &str, rev: u64, line: &str) {
        let parsed = match proto::parse(line) {
            Ok(Some(p)) => p,
            Ok(None) => {
                self.log(plugin, &format!("ignored an unknown message: {}", truncate(line, 200)));
                return;
            }
            Err(e) => {
                self.log(plugin, &format!("bad message ({e}): {}", truncate(line, 200)));
                return;
            }
        };
        match parsed {
            FromPlugin::Ready { declaration, contributions } => {
                let Some(proc) = self.pending.remove(plugin).filter(|p| p.rev == rev) else {
                    self.log(plugin, "ready for a revision that is no longer pending");
                    return;
                };
                if let Some(mut old) = self.running.remove(plugin) {
                    let _ = old.stdin.send(ToPlugin::Bye.line());
                    let _ = old.child.start_kill();
                    self.log(plugin, "replaced the previous revision");
                }
                let name = if declaration.name.is_empty() { plugin.to_owned() } else { declaration.name.clone() };
                if let Some(info) = self.plugins.get_mut(plugin) {
                    info.state = PluginState::Running;
                    info.detail.clear();
                    info.commands = contributions.commands.len();
                    // What the plugin declared about itself in its own code, kept for `/plugins` and the permission checks.
                    info.name = name.clone();
                    info.permissions = declaration.permissions.clone();
                    info.tick_ms = declaration.tick_ms;
                }
                self.emit(PluginEvent::Ready { id: plugin.to_owned(), name, contributions });
                if let Some(snapshot) = &self.last_snapshot {
                    self.seq += 1;
                    let _ = proc.stdin.send(ToPlugin::Snapshot { seq: self.seq, projection: snapshot }.line());
                }
                self.running.insert(plugin.to_owned(), proc);
                // The first heartbeat comes one interval after `ready`, not the moment the plugin loaded.
                self.ticks.insert(plugin.to_owned(), Instant::now());
                self.publish();
            }
            other => {
                // Only a running revision may speak: a pending one has not registered anything yet.
                if !self.running.get(plugin).map(|p| p.rev == rev).unwrap_or(false) {
                    self.log(plugin, "a message from a revision that is not running was ignored");
                    return;
                }
                match other {
                    FromPlugin::Status { segments } => self.emit(PluginEvent::Status { id: plugin.to_owned(), segments }),
                    FromPlugin::Pane(pane) => self.emit(PluginEvent::Pane { id: plugin.to_owned(), pane }),
                    FromPlugin::Panel(panel) => self.emit(PluginEvent::Panel { plugin: plugin.to_owned(), panel }),
                    FromPlugin::Flash { text } => self.emit(PluginEvent::Flash { text }),
                    FromPlugin::Draft { text } => self.emit(PluginEvent::Draft { plugin: plugin.to_owned(), text }),
                    FromPlugin::Send { channel, text } => self.forward_send(plugin, channel, text),
                    FromPlugin::Prompt { id, title, hint, multiline } => {
                        self.emit(PluginEvent::Prompt { plugin: plugin.to_owned(), id, title, hint, multiline })
                    }
                    FromPlugin::ListAgents { id } => self.emit(PluginEvent::ListAgents { plugin: plugin.to_owned(), id }),
                    FromPlugin::CreateAgent { id, name, instructions } => {
                        self.emit(PluginEvent::CreateAgent { plugin: plugin.to_owned(), id, name, instructions })
                    }
                    FromPlugin::Log { level, text } => {
                        self.log(plugin, &format!("{level}: {text}"));
                        if level == "error" {
                            self.emit(PluginEvent::Log { id: plugin.to_owned(), level, text });
                        }
                    }
                    FromPlugin::Ready { .. } => unreachable!("handled above"),
                }
            }
        }
    }

    fn on_exit(&mut self, plugin: &str, rev: u64, why: String) {
        let mut gone = false;
        if let Some(proc) = self.pending.remove(plugin) {
            if proc.rev == rev {
                self.failed(plugin, why.clone(), false);
            }
        }
        if let Some(proc) = self.running.remove(plugin) {
            if proc.rev == rev {
                gone = true;
                self.ticks.remove(plugin);
                if let Some(info) = self.plugins.get_mut(plugin) {
                    info.state = PluginState::Failed;
                    info.detail = why.clone();
                    info.commands = 0;
                }
                self.log(plugin, &format!("stopped: {why}"));
                self.emit(PluginEvent::Gone { id: plugin.into(), why });
            }
        }
        if gone {
            self.publish();
        }
    }

    fn forward_send(&mut self, plugin: &str, channel: Option<String>, text: String) {
        if text.trim().is_empty() {
            return;
        }
        if self.read_only {
            self.emit(PluginEvent::Flash { text: format!("plugin {plugin} tried to send in read-only mode") });
            return;
        }
        let allowed = self.plugins.get(plugin).map(|i| i.permissions.iter().any(|p| p == "messages:write")).unwrap_or(false);
        if !allowed {
            self.emit(PluginEvent::Flash { text: format!("plugin {plugin} needs the messages:write permission to send") });
            return;
        }
        let now = Instant::now();
        let times = self.sends.entry(plugin.to_owned()).or_default();
        times.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
        if times.len() >= SENDS_PER_MINUTE || times.last().map(|t| now.duration_since(*t) < SEND_INTERVAL).unwrap_or(false) {
            self.emit(PluginEvent::Flash { text: format!("plugin {plugin} is sending too fast") });
            return;
        }
        times.push(now);
        self.emit(PluginEvent::Send { plugin: plugin.to_owned(), channel, text });
    }

    // ---- heartbeats ------------------------------------------------------------------------------------------------------

    /// A running plugin whose code set `tickMs` is sent a `tick` on that interval, no faster than `TICK_CHECK`. It is
    /// how a pane that shows something time-based (a clock, a build) redraws without the plugin running its own timer.
    fn send_ticks(&mut self) {
        if self.running.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut due: Vec<String> = Vec::new();
        for id in self.running.keys() {
            let every = self.plugins.get(id).map(|i| i.tick_ms).unwrap_or(0);
            if every == 0 {
                continue;
            }
            let last = *self.ticks.entry(id.clone()).or_insert(now);
            if now.duration_since(last) >= Duration::from_millis(every) {
                due.push(id.clone());
            }
        }
        for id in due {
            self.ticks.insert(id.clone(), now);
            self.tick_seq += 1;
            if let Some(proc) = self.running.get(&id) {
                let _ = proc.stdin.send(ToPlugin::Tick { seq: self.tick_seq }.line());
            }
        }
    }

    fn failed(&mut self, id: &str, why: String, flash: bool) {
        if let Some(info) = self.plugins.get_mut(id) {
            info.detail = why.clone();
            if !self.running.contains_key(id) {
                info.state = PluginState::Failed;
                info.commands = 0;
            }
        }
        self.log(id, &why);
        if flash {
            self.emit(PluginEvent::Flash { text: format!("plugin {id}: {why}") });
        }
    }

    // ---- from the app ----------------------------------------------------------------------------------------------------

    fn snapshot(&mut self, projection: Projection) {
        if self.last_snapshot.as_ref() == Some(&projection) {
            return;
        }
        self.last_snapshot = Some(projection.clone());
        if self.running.is_empty() {
            return;
        }
        self.seq += 1;
        let line = ToPlugin::Snapshot { seq: self.seq, projection: &projection }.line();
        for proc in self.running.values() {
            let _ = proc.stdin.send(line.clone());
        }
    }

    fn command(&mut self, command: PluginCmd) {
        match command {
            PluginCmd::Invoke { plugin, command, args } => {
                self.invoke += 1;
                if let Some(proc) = self.running.get(&plugin) {
                    let _ = proc.stdin.send(ToPlugin::Invoke { id: self.invoke, command: &command, args: &args }.line());
                }
            }
            PluginCmd::PanelAction { plugin, id, row, value } => {
                if let Some(proc) = self.running.get(&plugin) {
                    let _ = proc.stdin.send(ToPlugin::PanelAction { id: &id, row, value: value.as_deref() }.line());
                }
            }
            PluginCmd::PromptAnswer { plugin, id, text } => {
                if let Some(proc) = self.running.get(&plugin) {
                    let _ = proc.stdin.send(ToPlugin::PromptAnswer { id: &id, text: text.as_deref() }.line());
                }
            }
            PluginCmd::Agents { plugin, id, agents } => {
                if let Some(proc) = self.running.get(&plugin) {
                    let _ = proc.stdin.send(ToPlugin::Agents { id: &id, agents: &agents }.line());
                }
            }
            PluginCmd::Created { plugin, id, result } => {
                if let Some(proc) = self.running.get(&plugin) {
                    let (agent, error) = match &result {
                        Ok(a) => (Some(a), None),
                        Err(e) => (None, Some(e.as_str())),
                    };
                    let _ = proc.stdin.send(ToPlugin::Created { id: &id, agent, error }.line());
                }
            }
        }
    }

    // ---- bookkeeping -----------------------------------------------------------------------------------------------------

    fn publish(&mut self) {
        let meta: Vec<PluginMeta> = self
            .plugins
            .iter()
            .map(|(id, info)| PluginMeta {
                id: id.clone(),
                name: if info.name.is_empty() { id.clone() } else { info.name.clone() },
                state: info.state,
                detail: info.detail.clone(),
                permissions: info.permissions.clone(),
                commands: info.commands,
            })
            .collect();
        if meta != self.published {
            self.published = meta.clone();
            self.emit(PluginEvent::Meta(meta));
        }
    }

    fn emit(&self, event: PluginEvent) {
        let _ = self.events.send(AppEvent::Plugin(event));
    }

    fn log(&self, id: &str, line: &str) {
        let _ = std::fs::create_dir_all(&self.logs);
        let path = self.logs.join(format!("{}.log", crate::contrib::valid_id(id).then(|| id).unwrap_or("invalid")));
        if std::fs::metadata(&path).map(|m| m.len() > MAX_LOG).unwrap_or(false) {
            let _ = std::fs::remove_file(&path);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(file, "{} {}", chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ"), line.replace('\n', " "));
        }
    }

    fn log_file(&self, id: &str) -> Stdio {
        let _ = std::fs::create_dir_all(&self.logs);
        let path = self.logs.join(format!("{}.log", crate::contrib::valid_id(id).then(|| id).unwrap_or("invalid")));
        match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            Ok(file) => Stdio::from(file),
            Err(_) => Stdio::null(),
        }
    }
}

fn truncate(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

async fn write_stdin(mut stdin: ChildStdin, mut rx: UnboundedReceiver<String>) {
    while let Some(line) = rx.recv().await {
        if stdin.write_all(line.as_bytes()).await.is_err() || stdin.write_all(b"\n").await.is_err() || stdin.flush().await.is_err() {
            return;
        }
    }
    let _ = stdin.shutdown().await;
}

async fn read_stdout(stdout: ChildStdout, plugin: String, rev: u64, tx: UnboundedSender<ProcMsg>) {
    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if tx.send(ProcMsg::Line { plugin: plugin.clone(), rev, line }).is_err() {
                    return;
                }
            }
            Ok(None) => break,
            Err(e) => {
                let _ = tx.send(ProcMsg::Failed { plugin, rev, why: e.to_string() });
                return;
            }
        }
    }
    let _ = tx.send(ProcMsg::Eof { plugin, rev });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("buzzr-test-{name}-{}", uuid::Uuid::new_v4()))
    }

    /// Install a plugin folder the way one ships now: a single executable entry file and nothing else.
    fn install(root: &Path, id: &str, script: &str) {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("main.sh");
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// Copy a plugin folder out of `examples/plugins`, entry file and all (`fs::copy` keeps the executable bit).
    fn install_example(root: &Path, id: &str) -> PathBuf {
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/plugins").join(id);
        let dst = root.join(id);
        std::fs::create_dir_all(&dst).unwrap();
        for entry in std::fs::read_dir(&src).unwrap().flatten() {
            std::fs::copy(entry.path(), dst.join(entry.file_name())).unwrap();
        }
        dst
    }

    /// Is a program on `PATH`? The TypeScript example needs a JS runtime, which a build host may not have.
    fn on_path(program: &str) -> bool {
        std::env::var_os("PATH").map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file())).unwrap_or(false)
    }

    async fn next_plugin(rx: &mut UnboundedReceiver<AppEvent>) -> PluginEvent {
        loop {
            match tokio::time::timeout(Duration::from_secs(6), rx.recv()).await {
                Ok(Some(AppEvent::Plugin(e))) => return e,
                Ok(Some(_)) => continue,
                Ok(None) => panic!("the event channel closed"),
                Err(_) => panic!("the plugin never answered"),
            }
        }
    }

    const ECHO: &str = r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"type":"hello"'*) printf '%s\n' '{"type":"ready","name":"Echo","contributions":{"commands":[{"id":"hi","title":"Say hi"}],"status":[{"text":"up","tone":"ok"}]}}' ;;
    *'"type":"invoke"'*) printf '%s\n' '{"type":"panel","id":"hi","title":"Hello","rows":[{"text":"from the plugin","action":"ok"}]}' ;;
  esac
done
"#;

    #[tokio::test]
    #[cfg(unix)]
    async fn a_plugin_declares_itself_in_code_says_ready_answers_a_command_and_is_stopped() {
        let root = temp("plugin");
        install(&root, "echo", ECHO);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let host = Host::start(root.clone(), root.join("logs"), tx, false);
        host.snapshot(Projection { me: "me".into(), connection: "live".into(), view: "channel".into(), ..Projection::default() });

        // Discovery publishes the "starting" state before the child can say ready, so skip Meta here.
        loop {
            match next_plugin(&mut rx).await {
                PluginEvent::Ready { id, name, contributions } => {
                    assert_eq!(id, "echo", "the id is always the folder name");
                    assert_eq!(name, "Echo", "the name comes from the plugin's own `ready`, not from a file");
                    assert_eq!(contributions.status[0].text, "up");
                    assert_eq!(contributions.commands[0].id, "hi");
                    break;
                }
                PluginEvent::Meta(_) => continue,
                other => panic!("expected ready, got {other:?}"),
            }
        }

        host.command(PluginCmd::Invoke { plugin: "echo".into(), command: "hi".into(), args: String::new() });
        loop {
            match next_plugin(&mut rx).await {
                PluginEvent::Panel { plugin, panel } => {
                    assert_eq!(plugin, "echo");
                    assert_eq!(panel.rows[0].text, "from the plugin");
                    break;
                }
                _ => continue,
            }
        }
        host.shutdown();
        std::fs::remove_dir_all(root).ok();
    }

    const TICKER: &str = r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"type":"hello"'*) printf '%s\n' '{"type":"ready","name":"Ticker","tickMs":250,"contributions":{"pane":{"title":"Clock","rows":[{"text":"one","tone":"accent"}]}}}' ;;
    *'"type":"tick"'*) printf '%s\n' '{"type":"pane","title":"Clock","rows":[{"text":"two","tone":"accent"}]}' ;;
  esac
done
"#;

    /// A plugin may ask for a heartbeat in its own code; it then gets a `tick` and can replace its pane from it.
    #[tokio::test]
    #[cfg(unix)]
    async fn a_plugin_that_asks_for_a_heartbeat_in_its_code_gets_ticks_and_can_refresh_its_pane() {
        let root = temp("ticker");
        install(&root, "ticker", TICKER);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let host = Host::start(root.clone(), root.join("logs"), tx, false);

        loop {
            if let PluginEvent::Ready { contributions, .. } = next_plugin(&mut rx).await {
                let pane = contributions.pane.expect("the ticker declared a pane");
                assert_eq!(pane.rows[0].text, "one");
                break;
            }
        }
        loop {
            if let PluginEvent::Pane { id, pane: Some(pane) } = next_plugin(&mut rx).await {
                assert_eq!(id, "ticker");
                assert_eq!(pane.rows[0].text, "two", "the heartbeat reached the plugin");
                break;
            }
        }
        host.shutdown();
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn a_plugin_is_reloaded_when_its_files_change_and_a_broken_one_keeps_the_old() {
        let root = temp("reload");
        install(&root, "echo", ECHO);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let host = Host::start(root.clone(), root.join("logs"), tx, false);
        let mut seen = 0;
        while seen < 1 {
            if matches!(next_plugin(&mut rx).await, PluginEvent::Ready { .. }) {
                seen += 1;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(root.join("echo/main.sh"), ECHO.replace("\"up\"", "\"up again\"")).unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        let mut reloaded = false;
        while tokio::time::Instant::now() < deadline && !reloaded {
            if let PluginEvent::Ready { contributions, .. } = next_plugin(&mut rx).await {
                if contributions.status[0].text == "up again" {
                    reloaded = true;
                }
            }
        }
        assert!(reloaded, "the edited plugin was not reloaded");
        host.shutdown();
        std::fs::remove_dir_all(root).ok();
    }

    /// The demo in `examples/plugins/clock`, built on the TypeScript SDK: it declares a pane and refreshes it from the
    /// heartbeat it asked for in code. Needs a JS runtime on `PATH`, which a build host may not have.
    #[tokio::test]
    #[cfg(unix)]
    async fn the_clock_example_declares_a_pane_and_refreshes_it_on_the_heartbeat() {
        if !on_path("node") {
            eprintln!("skipping: examples/plugins/clock needs node on PATH");
            return;
        }
        let root = temp("clock");
        install_example(&root, "clock");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let host = Host::start(root.clone(), root.join("logs"), tx, false);

        loop {
            if let PluginEvent::Ready { id, name, contributions } = next_plugin(&mut rx).await {
                assert_eq!(id, "clock");
                assert_eq!(name, "Clock");
                let pane = contributions.pane.expect("the clock declares a pane");
                assert_eq!(pane.title, "Clock");
                assert_eq!(pane.rows.len(), 5, "the time is five rows of block digits");
                let top = &pane.rows[0].text;
                assert!(top.contains('█'), "the digits are drawn with block characters: {top:?}");
                assert!(top.chars().count() <= 28, "and fit the channel column: {top:?}");
                break;
            }
        }
        loop {
            if let PluginEvent::Pane { id, pane: Some(pane) } = next_plugin(&mut rx).await {
                assert_eq!(id, "clock");
                assert_eq!(pane.rows.len(), 5, "the heartbeat redrew the clock");
                break;
            }
        }
        host.shutdown();
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn a_plugin_that_exits_is_reported_as_gone() {
        let root = temp("crash");
        install(&root, "crash", "#!/bin/sh\nread line\nprintf '%s\\n' '{\"type\":\"ready\",\"contributions\":{}}'\nexit 3\n");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let host = Host::start(root.clone(), root.join("logs"), tx, false);
        let mut ready = false;
        let mut gone = false;
        for _ in 0..6 {
            match next_plugin(&mut rx).await {
                PluginEvent::Ready { id, .. } if id == "crash" => ready = true,
                PluginEvent::Gone { id, .. } if id == "crash" => {
                    gone = true;
                    break;
                }
                _ => continue,
            }
        }
        assert!(ready, "the plugin never became ready");
        assert!(gone, "the plugin's exit was not reported");
        host.shutdown();
        std::fs::remove_dir_all(root).ok();
    }

    /// A folder that cannot be loaded is still listed, named after the folder, and says why in its own row.
    #[tokio::test]
    #[cfg(unix)]
    async fn a_folder_that_would_not_load_is_shown_in_the_plugin_list() {
        let root = temp("broken");
        // No entry file at all: the folder is not a plugin.
        std::fs::create_dir_all(root.join("empty")).unwrap();
        // A folder name that could reach a path or a JSON key: refused before anything runs.
        std::fs::create_dir_all(root.join("Bad Name")).unwrap();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let host = Host::start(root.clone(), root.join("logs"), tx, false);
        let mut seen = 0;
        for _ in 0..4 {
            if let PluginEvent::Meta(meta) = next_plugin(&mut rx).await {
                if let Some(m) = meta.iter().find(|m| m.id == "empty") {
                    assert_eq!(m.state, PluginState::Failed);
                    assert_eq!(m.name, "empty", "a folder that never said its name is named after the folder");
                    assert!(m.detail.contains("no entry file"), "{:?}", m.detail);
                    seen += 1;
                }
                if let Some(m) = meta.iter().find(|m| m.id == "Bad Name") {
                    assert_eq!(m.state, PluginState::Failed);
                    assert!(m.detail.contains("lowercase"), "{:?}", m.detail);
                    seen += 1;
                }
                if seen == 2 {
                    break;
                }
            }
        }
        assert_eq!(seen, 2, "both broken folders were reported");
        host.shutdown();
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_plugin_cannot_put_an_escape_in_a_log_file_name() {
        assert!(!crate::contrib::valid_id("../../etc/passwd"));
        assert!(crate::contrib::valid_id("todo"));
    }

    /// The demo in `examples/plugins/hello` must keep working with the real host.
    #[tokio::test]
    #[cfg(unix)]
    async fn the_example_plugin_works_with_the_host() {
        let root = temp("example");
        install_example(&root, "hello");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let host = Host::start(root.clone(), root.join("logs"), tx, false);
        host.snapshot(Projection { me: "me".into(), connection: "live".into(), view: "channel".into(), ..Projection::default() });

        loop {
            if let PluginEvent::Ready { id, contributions, .. } = next_plugin(&mut rx).await {
                assert_eq!(id, "hello");
                assert_eq!(contributions.commands[0].id, "hello");
                assert_eq!(contributions.status[0].text, "hello");
                break;
            }
        }
        host.command(PluginCmd::Invoke { plugin: "hello".into(), command: "hello".into(), args: String::new() });
        loop {
            if let PluginEvent::Panel { panel, .. } = next_plugin(&mut rx).await {
                assert!(panel.rows.iter().any(|r| r.action.is_some()), "the demo panel has an actionable row");
                break;
            }
        }
        host.shutdown();
        std::fs::remove_dir_all(root).ok();
    }
}
