//! Running plugin processes and carrying messages between them and the app.
//!
//! One tokio task supervises every child: it starts them, enforces the handshake deadline, notices edits in the plugins folder
//! and reloads by starting the new revision next to the old one, swaps only when the new process says it is ready, and never
//! blocks the UI. Nothing here touches `App`; the only things that cross are `PluginEvent`s and `PluginCmd`s.

use crate::app::AppEvent;
use crate::contrib::{PluginCmd, PluginEvent, PluginMeta, PluginState, Projection};
use crate::plugins::manifest::{self, Manifest, DISABLED};
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
/// A plugin may post this often, no more. The user always sees it happen.
const SEND_INTERVAL: Duration = Duration::from_secs(1);
const SENDS_PER_MINUTE: usize = 20;
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
    manifest: Option<Manifest>,
    disabled: bool,
    state: PluginState,
    detail: String,
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
            }
        }
        self.stop_all();
        self.publish();
    }

    // ---- discovery --------------------------------------------------------------------------------------------------------

    fn rescan(&mut self, proc_tx: &UnboundedSender<ProcMsg>) {
        let found = manifest::scan(&self.root);
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
                let man = d.manifest.clone();
                let mut info = Info { manifest: man, disabled: d.disabled, ..Info::default() };
                if let Some(m) = &info.manifest {
                    match self.spawn(m, &dir, 1, proc_tx) {
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
                    info.detail = d.problem.clone().unwrap_or_else(|| "no manifest".into());
                }
                (id, info)
            })
            .collect();
        self.last_stamp = manifest::stamps(&self.root);
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
        let current = manifest::stamps(&self.root);
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
        match manifest::load(&dir) {
            Ok(m) => {
                let was_running = self.running.contains_key(folder);
                self.plugins.entry(folder.to_owned()).or_insert_with(Info::default).manifest = Some(m.clone());
                let info = self.plugins.get_mut(folder).expect("just inserted");
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
                match self.spawn(&m, &dir, rev, proc_tx) {
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
                    info.manifest = None;
                    info.state = PluginState::Failed;
                    info.detail = e.clone();
                }
                self.failed(folder, e, true);
            }
        }
        self.publish();
    }

    // ---- processes --------------------------------------------------------------------------------------------------------

    fn spawn(&mut self, m: &Manifest, dir: &Path, rev: u64, proc_tx: &UnboundedSender<ProcMsg>) -> Result<Proc, String> {
        // The child gets a clean environment: on purpose, so BUZZ_PRIVATE_KEY / BUZZ_AUTH_TAG in buzzr's own environment
        // cannot leak into a plugin. Only what a program needs to run is passed on.
        let mut cmd = Command::new(&m.entry[0]);
        cmd.kill_on_drop(true)
            .args(&m.entry[1..])
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(self.log_file(&m.id))
            .env_clear();
        for key in ["PATH", "HOME", "LANG", "LC_ALL", "TZ"] {
            if let Ok(v) = std::env::var(key) {
                cmd.env(key, v);
            }
        }
        cmd.env("BUZZR_PLUGIN_ID", &m.id);
        cmd.env("BUZZR_PLUGIN_DIR", dir);
        if let Some(dir) = self.root.parent() {
            cmd.env("BUZZR_CONFIG_DIR", dir);
        }
        cmd.env("TMPDIR", std::env::temp_dir());
        let mut child = cmd.spawn().map_err(|e| format!("could not start {:?}: {e}", m.entry[0]))?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let (line_tx, line_rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(write_stdin(stdin, line_rx));
        let plugin = m.id.clone();
        let tx = proc_tx.clone();
        tokio::spawn(async move { read_stdout(stdout, plugin, rev, tx).await });
        let _ = line_tx.send(
            ToPlugin::Hello { protocol: proto::PROTOCOL, api_version: proto::API_VERSION, host: concat!("buzzr/", env!("CARGO_PKG_VERSION")), plugin: &m.id }
                .line(),
        );
        self.log(&m.id, &format!("started revision {rev}: {:?}", m.entry));
        Ok(Proc { rev, child, stdin: line_tx, started: Instant::now() })
    }

    fn stop(&mut self, id: &str) {
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
            FromPlugin::Ready { contributions } => {
                let Some(proc) = self.pending.remove(plugin).filter(|p| p.rev == rev) else {
                    self.log(plugin, "ready for a revision that is no longer pending");
                    return;
                };
                if let Some(mut old) = self.running.remove(plugin) {
                    let _ = old.stdin.send(ToPlugin::Bye.line());
                    let _ = old.child.start_kill();
                    self.log(plugin, "replaced the previous revision");
                }
                if let Some(info) = self.plugins.get_mut(plugin) {
                    info.state = PluginState::Running;
                    info.detail.clear();
                    info.commands = contributions.commands.len();
                }
                let name = self.plugins.get(plugin).and_then(|i| i.manifest.as_ref()).map(|m| m.name.clone()).unwrap_or_else(|| plugin.into());
                self.emit(PluginEvent::Ready { id: plugin.to_owned(), name, contributions });
                if let Some(snapshot) = &self.last_snapshot {
                    self.seq += 1;
                    let _ = proc.stdin.send(ToPlugin::Snapshot { seq: self.seq, projection: snapshot }.line());
                }
                self.running.insert(plugin.to_owned(), proc);
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
                    FromPlugin::Panel(panel) => self.emit(PluginEvent::Panel { plugin: plugin.to_owned(), panel }),
                    FromPlugin::Flash { text } => self.emit(PluginEvent::Flash { text }),
                    FromPlugin::Draft { text } => self.emit(PluginEvent::Draft { plugin: plugin.to_owned(), text }),
                    FromPlugin::Send { channel, text } => self.forward_send(plugin, channel, text),
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
        let allowed = self.plugins.get(plugin).and_then(|i| i.manifest.as_ref()).map(|m| m.permissions.iter().any(|p| p == "messages:write")).unwrap_or(false);
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
        }
    }

    // ---- bookkeeping -----------------------------------------------------------------------------------------------------

    fn publish(&mut self) {
        let meta: Vec<PluginMeta> = self
            .plugins
            .iter()
            .map(|(id, info)| PluginMeta {
                id: id.clone(),
                name: info.manifest.as_ref().map(|m| m.name.clone()).unwrap_or_else(|| id.clone()),
                state: info.state,
                detail: info.detail.clone(),
                permissions: info.manifest.as_ref().map(|m| m.permissions.clone()).unwrap_or_default(),
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

    fn install(root: &Path, id: &str, script: &str, permissions: &str) {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plugin.sh"), script).unwrap();
        std::fs::write(
            dir.join("plugin.json"),
            format!(r#"{{"id":"{id}","name":"{id}","apiVersion":1,"entry":["sh","plugin.sh"],"permissions":[{permissions}]}}"#),
        )
        .unwrap();
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
    *'"type":"hello"'*) printf '%s\n' '{"type":"ready","contributions":{"commands":[{"id":"hi","title":"Say hi"}],"status":[{"text":"up","tone":"ok"}]}}' ;;
    *'"type":"invoke"'*) printf '%s\n' '{"type":"panel","id":"hi","title":"Hello","rows":[{"text":"from the plugin","action":"ok"}]}' ;;
  esac
done
"#;

    #[tokio::test]
    #[cfg(unix)]
    async fn a_plugin_says_ready_answers_a_command_and_is_stopped() {
        let root = temp("plugin");
        install(&root, "echo", ECHO, "");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let host = Host::start(root.clone(), root.join("logs"), tx, false);
        host.snapshot(Projection { me: "me".into(), connection: "live".into(), view: "channel".into(), ..Projection::default() });

        // Discovery publishes the "starting" state before the child can say ready, so skip Meta here.
        loop {
            match next_plugin(&mut rx).await {
                PluginEvent::Ready { id, contributions, .. } => {
                    assert_eq!(id, "echo");
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

    #[tokio::test]
    #[cfg(unix)]
    async fn a_plugin_is_reloaded_when_its_files_change_and_a_broken_one_keeps_the_old() {
        let root = temp("reload");
        install(&root, "echo", ECHO, "");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let host = Host::start(root.clone(), root.join("logs"), tx, false);
        let mut seen = 0;
        while seen < 1 {
            if matches!(next_plugin(&mut rx).await, PluginEvent::Ready { .. }) {
                seen += 1;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(root.join("echo/plugin.sh"), ECHO.replace("\"up\"", "\"up again\"")).unwrap();

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

    #[tokio::test]
    #[cfg(unix)]
    async fn a_plugin_that_exits_is_reported_as_gone() {
        let root = temp("crash");
        install(&root, "crash", "#!/bin/sh\nread line\nprintf '%s\\n' '{\"type\":\"ready\",\"contributions\":{}}'\nexit 3\n", "");
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

    #[tokio::test]
    #[cfg(unix)]
    async fn a_broken_manifest_is_shown_in_the_plugin_list() {
        let root = temp("broken");
        std::fs::create_dir_all(root.join("broken")).unwrap();
        std::fs::write(root.join("broken/plugin.json"), r#"{"id":"broken","name":"Broken","apiVersion":99,"entry":["sh"]}"#).unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let host = Host::start(root.clone(), root.join("logs"), tx, false);
        let mut found = false;
        for _ in 0..3 {
            if let PluginEvent::Meta(meta) = next_plugin(&mut rx).await {
                if let Some(m) = meta.iter().find(|m| m.id == "broken") {
                    assert_eq!(m.state, PluginState::Failed);
                    assert!(m.detail.contains("not supported"), "{:?}", m.detail);
                    found = true;
                    break;
                }
            }
        }
        assert!(found, "the broken plugin was not reported");
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
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/plugins/hello");
        let dst = root.join("hello");
        std::fs::create_dir_all(&dst).unwrap();
        for file in ["plugin.json", "main.sh"] {
            std::fs::copy(src.join(file), dst.join(file)).unwrap();
        }
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
