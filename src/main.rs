use anyhow::{anyhow, Context, Result};
use buzzr::{agents, clock, config, demo, harness, keyfile, live, plugin, plugins, relay, release, runner, selftest, setup, ui, update};
use buzzr::app::{App, AppEvent, Cmd, Conn};
use buzzr::relay::Relay;
use clap::Parser;
use futures_util::StreamExt;
use nostr::Keys;
use ratatui::crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, EventStream, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::crossterm::{execute, ExecutableCommand};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io::{stdout, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::watch;

#[derive(Parser)]
#[command(version, about = "A terminal client for Buzz that talks to the relay directly")]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    /// Run the setup again: ask for the relay and the private key and how to keep it.
    #[arg(long)]
    setup: bool,
    /// Delete the saved private key (the relay setting stays).
    #[arg(long)]
    forget_key: bool,
    /// Relay URL (default: $BUZZ_RELAY_URL, else the one saved by the setup).
    #[arg(long)]
    relay: Option<String>,
    /// Never send anything.
    #[arg(long)]
    read_only: bool,
    /// Do not tell others when you are typing.
    #[arg(long)]
    no_typing: bool,
    /// Do not capture the mouse (then the terminal can select text by itself).
    #[arg(long)]
    no_mouse: bool,
    /// Explore buzzr with a built-in world instead of a relay: nothing is sent anywhere and nothing is written to disk.
    #[arg(long)]
    demo: bool,
    /// Check the connection and exit; sends nothing. Uses the same relay and key as the app (set BUZZ_PRIVATE_KEY to avoid prompts).
    #[arg(long)]
    selftest: bool,
    /// With --selftest: also create a private throwaway channel, post/edit/delete a message in it, and delete the channel.
    #[arg(long)]
    probe_write: bool,
    /// With --selftest: also run this full-text search and show the first hits.
    #[arg(long, value_name = "TEXT")]
    probe_search: Option<String>,
    /// Seconds the selftest listens for live events.
    #[arg(long, default_value_t = 8)]
    listen: u64,
    /// With --selftest: publish one typing indicator in the named channel and check that it comes back live.
    #[arg(long, value_name = "CHANNEL")]
    probe_typing: Option<String>,
    /// Do not load plugins.
    #[arg(long, global = true)]
    no_plugins: bool,
    /// Where plugins live (default: $BUZZR_PLUGINS_DIR, else <config dir>/plugins).
    #[arg(long, value_name = "DIR", global = true)]
    plugins_dir: Option<PathBuf>,
}

#[derive(clap::Subcommand)]
enum Command {
    /// See which plugins are installed, and turn one on or off.
    Plugins {
        #[command(subcommand)]
        cmd: PluginsCmd,
    },
}

#[derive(clap::Subcommand)]
enum PluginsCmd {
    /// Show every plugin folder, whether it would load, and where it lives.
    List,
    /// Turn a plugin on (removes its DISABLED marker).
    Enable { id: String },
    /// Turn a plugin off without deleting it.
    Disable { id: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let paths = config::Paths::from_env();
    let plugins_dir = args.plugins_dir.clone().unwrap_or_else(|| paths.plugins());
    if let Some(Command::Plugins { cmd }) = args.command {
        return plugins_cli(&cmd, &plugins_dir);
    }
    if args.forget_key {
        let removed = setup::forget_key(&paths)?;
        if removed.is_empty() {
            eprintln!("No saved key in {}", paths.dir().display());
        }
        for p in removed {
            eprintln!("Removed {}", p.display());
        }
        return Ok(());
    }
    let tz = config::env("TZ").and_then(|t| t.parse().ok()).unwrap_or_else(local_timezone);
    let notice = if args.demo {
        // A demo asks for no key and reaches no relay: it runs off a world built into the binary (see `demo`).
        let session = Session::Demo { world: demo::Demo::new(), read_only: args.read_only };
        run_tui(session, None, !args.no_mouse, tz, &paths).await?
    } else {
        let log_n = config::env("SCRYPT_LOG_N").and_then(|v| v.parse().ok()).unwrap_or(keyfile::DEFAULT_LOG_N);
        let given_relay = args.relay.or_else(|| std::env::var("BUZZ_RELAY_URL").ok().filter(|v| !v.trim().is_empty()));
        let given_key = std::env::var("BUZZ_PRIVATE_KEY").ok();
        // The key is in memory now: take it out of buzzr's environment, so no program it starts later can inherit it.
        // A plugin child already gets a clean environment (`plugins::host`) and a harness gets one too (`harness::acp`),
        // but this closes the general case rather than relying on each start to remember.
        std::env::remove_var("BUZZ_PRIVATE_KEY");
        let setup::Resolved { relay: url, key } =
            setup::resolve(&mut setup::Terminal, &paths, given_relay, given_key, args.setup, log_n).map_err(|e| anyhow!("{e}"))?;
        let keys = Keys::parse(&key).context("that is not a valid hex or nsec private key (run `buzzr --setup` to enter it again)")?;
        drop(key);
        // BUZZ_AUTH_TAG is only for agents (NIP-OA)
        let auth_tag = std::env::var("BUZZ_AUTH_TAG").ok();
        std::env::remove_var("BUZZ_AUTH_TAG");
        let relay = Arc::new(Relay::new(&url, keys, auth_tag)?);
        if args.selftest {
            selftest::run(relay.clone(), args.listen, args.probe_typing, args.probe_search).await?;
            if args.probe_write {
                selftest::write_probe(relay).await?;
            }
            return Ok(());
        }
        // The live subscription and its handle are made here so the session owns the way `Cmd::Subscribe` reaches it,
        // while the loop keeps the receiver.
        let plugins_on = !args.no_plugins && config::env("SAFE_MODE").as_deref() != Some("1");
        let dirs = plugins_on.then(|| (plugins_dir, paths.plugin_logs()));
        let live = live::spawn(relay.clone(), Vec::new());
        let session = Session::Live(LiveSession {
            relay,
            subscription: live.channels,
            read_only: args.read_only,
            send_typing: !args.no_typing,
            plugin_dirs: dirs,
        });
        run_tui(session, Some(live.rx), !args.no_mouse, tz, &paths).await?
    };
    if let Some(notice) = notice {
        eprintln!("{notice}"); // after the alternate screen is gone, so it does not fight the UI
    }
    Ok(())
}

/// What a run is talking to.
enum Session {
    /// The relay, over the network.
    Live(LiveSession),
    /// The demo: a world built into the binary, which answers every request itself (see `demo`).
    Demo {
        world: demo::Demo,
        read_only: bool,
    },
}

/// Everything a live run talks to, gathered so the loop can hand a command to `run` without six arguments.
struct LiveSession {
    relay: Arc<Relay>,
    /// Send the ids of the channels to follow; the live subscription replaces its subscription when they change.
    subscription: watch::Sender<Vec<String>>,
    read_only: bool,
    send_typing: bool,
    /// Where plugins live and where they log, or `None` when they are off (`--no-plugins`, `BUZZR_SAFE_MODE=1`).
    plugin_dirs: Option<(PathBuf, PathBuf)>,
}

/// `buzzr plugins list | enable <id> | disable <id>`: the same folders the TUI reads, without starting a terminal or a key.
fn plugins_cli(cmd: &PluginsCmd, dir: &std::path::Path) -> Result<()> {
    use plugins::discover;
    match cmd {
        PluginsCmd::List => {
            println!("plugins in {}", dir.display());
            let found = discover::scan(dir);
            if found.is_empty() {
                println!("  (none)");
            }
            for d in found {
                // The display name is the plugin's own to declare, and it only does that once it is running, so the
                // list shows the id and the file buzzr would run.
                let state = if d.disabled {
                    "off"
                } else if d.entry.is_some() {
                    "on"
                } else {
                    "broken"
                };
                match &d.entry {
                    Some(entry) => println!("  {:<20} {state:<6} {}", d.id(), entry.display()),
                    None => println!("  {:<20} {state:<6} {}", d.id(), d.problem.clone().unwrap_or_default()),
                }
            }
            Ok(())
        }
        PluginsCmd::Enable { id } => set_plugin(dir, id, true),
        PluginsCmd::Disable { id } => set_plugin(dir, id, false),
    }
}

fn set_plugin(dir: &std::path::Path, id: &str, on: bool) -> Result<()> {
    use plugins::discover;
    let Some(d) = discover::scan(dir).into_iter().find(|d| d.id() == id) else {
        return Err(anyhow!("no plugin {id} in {}", dir.display()));
    };
    discover::set_enabled(&d.dir, on).with_context(|| format!("could not change {}", d.dir.display()))?;
    println!("{}: {}", d.id(), if on { "on" } else { "off" });
    Ok(())
}

/// The computer's own time zone, else UTC.
fn local_timezone() -> chrono_tz::Tz {
    iana_time_zone::get_timezone().ok().and_then(|name| name.parse().ok()).unwrap_or(chrono_tz::UTC)
}

struct TerminalGuard {
    mouse: bool,
    enhanced: bool,
}

impl TerminalGuard {
    fn enter(mouse: bool) -> Result<TerminalGuard> {
        enable_raw_mode()?;
        let mut out = stdout();
        execute!(out, EnterAlternateScreen, EnableBracketedPaste)?;
        if mouse {
            execute!(out, EnableMouseCapture)?;
        }
        // Terminals that speak the kitty keyboard protocol report Shift+Enter separately from Enter.
        let enhanced = supports_keyboard_enhancement().unwrap_or(false);
        if enhanced {
            out.execute(PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES))?;
        }
        Ok(TerminalGuard { mouse, enhanced })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut out = stdout();
        if self.enhanced {
            let _ = out.execute(PopKeyboardEnhancementFlags);
        }
        if self.mouse {
            let _ = execute!(out, DisableMouseCapture);
        }
        let _ = execute!(out, DisableBracketedPaste, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

async fn run_tui(mut session: Session, mut live_rx: Option<mpsc::UnboundedReceiver<live::Live>>, mouse: bool, tz: chrono_tz::Tz, paths: &config::Paths) -> Result<Option<String>> {
    // restore the terminal before a panic message is printed, or it would be unreadable
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), DisableMouseCapture, DisableBracketedPaste, LeaveAlternateScreen);
        default_hook(info);
    }));

    let guard = TerminalGuard::enter(mouse)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let (me, read_only) = match &session {
        Session::Live(s) => (s.relay.me(), s.read_only),
        Session::Demo { world, read_only } => (world.me().to_owned(), *read_only),
    };
    let mut app = App::new(me, read_only, tz);
    // Shift+Enter only reaches us where the terminal speaks the kitty keyboard protocol; the footer names the key that works here.
    app.keyboard_enhanced = guard.enhanced;
    app.update_source = update::InstallSource::detect();
    match &session {
        Session::Live(s) => {
            app.send_typing = s.send_typing;
            // Where each channel was left last time; the badge counts come back over the relay (see `Cmd::LoadUnread`).
            app.seed_read(config::load_read_state(&paths.read_state()));
            // The channels archived on this device: they have no row in the channel list until a search brings one back.
            app.seed_archived(config::load_archived(&paths.archived()));
            // What was typed but not sent, per channel and thread: it comes back into the box where it was written.
            app.seed_drafts(config::load_drafts(&paths.drafts()));
            // The local agents buzzr made: the identity and the folder each one works in, from `agents.json` (see `agents.rs`).
            app.seed_agents(agents::load(&paths.agents()));
            // The compiled-in plugins this build registered (see `plugin`): their commands, contributions and permissions, kept
            // beside the external ones the `plugins::Host` supervises. Both are driven through the same contract, and both
            // kinds are off together — `plugin_dirs` is `None` under `--no-plugins` and `BUZZR_SAFE_MODE=1`.
            if s.plugin_dirs.is_some() {
                app.seed_plugins(plugin::PluginRuntime::new());
            }
        }
        Session::Demo { world, .. } => {
            // The demo's own world stands in for everything those files hold, and a demo writes none of them: it starts
            // the same way every time and leaves the machine as it found it.
            app.seed_read(world.read_state());
            // The header has to say what this is: there is no relay to connect to, so it never becomes "live".
            app.conn = Conn::Demo;
            app.notice(demo::HINT);
        }
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    // The release feed: seed the notice from what the last check learned, and ask GitHub again only when that is stale. A
    // failure (offline, no release yet) is silent — see `release.rs`. A demo asks nobody anything, so it skips this.
    if let Session::Live(_) = &session {
        if release::enabled() {
            let state = release::State::load(&paths.release_state());
            if let Some(v) = state.newer_than(release::CURRENT) {
                app.latest = Some(v.to_owned());
            }
            if state.is_due(clock::now_secs()) {
                let (tx, path) = (tx.clone(), paths.release_state());
                tokio::spawn(async move {
                    if let Some(v) = release::check_and_store(&path).await {
                        let _ = tx.send(AppEvent::Update(v));
                    }
                });
            }
        }
    }
    let host = match &session {
        Session::Live(s) => match &s.plugin_dirs {
            Some((dir, logs)) => plugins::Host::start(dir.clone(), logs.clone(), tx.clone(), s.read_only),
            None => plugins::Host::none(),
        },
        // A demo loads no plugins: they are written for a session with a relay behind it.
        Session::Demo { .. } => plugins::Host::none(),
    };
    let disk = Disk::start();
    // The local agents: a mention of one is answered by its harness (see `runner.rs`). An answer is a message, so a
    // read-only run, and safe mode, start no watcher at all — an agent that could not send would only report failures.
    // A demo runs none either: it has no relay for an answer to arrive on.
    if let Session::Live(s) = &session {
        if !s.read_only && config::env("SAFE_MODE").as_deref() != Some("1") {
            runner::spawn(s.relay.clone(), paths.clone(), tx.clone());
        }
    }
    // Hand a command to whoever answers this run's requests: the relay, or the demo world. One macro keeps the loop body
    // the same either way, and every arm of `run` stays exactly what a live run does.
    macro_rules! dispatch {
        ($cmd:expr) => {
            match &mut session {
                Session::Live(s) => run($cmd, &Ctx { relay: &s.relay, tx: &tx, subscription: &s.subscription, host: &host, paths, disk: &disk }),
                Session::Demo { world, .. } => world.answer($cmd, &tx),
            }
        };
    }
    for cmd in app.start() {
        dispatch!(cmd);
    }
    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        terminal.draw(|f| ui::draw(f, &mut app))?;
        for seq in app.osc.drain(..) {
            let mut out = stdout();
            out.write_all(seq.as_bytes())?;
            out.flush()?;
        }
        if app.quit {
            // What is still in the box goes to disk before we go: quitting is not a reason to lose a half-written message.
            for cmd in app.stash_draft() {
                dispatch!(cmd);
            }
            host.shutdown();
            disk.flush().await;
            return Ok(exit_notice(&app));
        }
        let event = tokio::select! {
            e = input.next() => match e {
                Some(Ok(Event::Key(k))) => Some(AppEvent::Key(k)),
                Some(Ok(Event::Mouse(m))) => Some(AppEvent::Mouse(m)),
                Some(Ok(Event::Paste(s))) => Some(AppEvent::Paste(s)),
                Some(Ok(_)) => None, // resize and focus events: the next draw adapts
                Some(Err(e)) => {
                    host.shutdown();
                    return Err(e.into());
                }
                None => {
                    for cmd in app.stash_draft() {
                        dispatch!(cmd);
                    }
                    host.shutdown();
                    disk.flush().await;
                    return Ok(exit_notice(&app));
                }
            },
            e = rx.recv() => e,
            l = recv_live(&mut live_rx) => l.map(AppEvent::Live),
            _ = tick.tick() => Some(AppEvent::Tick),
        };
        if let Some(ev) = event {
            let ticked = matches!(ev, AppEvent::Tick);
            for cmd in app.handle(ev) {
                dispatch!(cmd);
            }
            // A compiled-in plugin's heartbeat, on the same tick the host drives an external one's.
            if ticked {
                for cmd in app.plugin_tick() {
                    dispatch!(cmd);
                }
            }
        }
        // Only what changed is sent, and only to plugins that are ready; a stalled plugin never slows the loop. The
        // compiled-in plugins are shown the same projection, on the same "only what changed" rule.
        for cmd in app.plugin_snapshot(app.projection()) {
            dispatch!(cmd);
        }
        host.snapshot(app.projection());
    }
}

/// The next live event, for a run that has a relay to hear it from. A demo passes `None`: it has nothing to wait on, so
/// this never resolves and the loop's live arm never fires.
async fn recv_live(rx: &mut Option<mpsc::UnboundedReceiver<live::Live>>) -> Option<live::Live> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// The stderr notice for a session that has ended: only when the feed named a newer release, and only once the screen is back.
fn exit_notice(app: &App) -> Option<String> {
    app.latest.as_ref().map(|v| release::notice(release::CURRENT, v, app.update_source))
}

/// What a command needs to run: the relay, the way back into the loop, and the disk writer. Built once, so `run` and the loop do
/// not pass six arguments around.
struct Ctx<'a> {
    relay: &'a Arc<Relay>,
    tx: &'a UnboundedSender<AppEvent>,
    subscription: &'a tokio::sync::watch::Sender<Vec<String>>,
    host: &'a plugins::Host,
    paths: &'a config::Paths,
    disk: &'a Disk,
}

type DiskJob = Box<dyn FnOnce() + Send>;

/// Writes the small state files (read positions, archived channels, drafts) one after the other, off the loop. Each save is the
/// whole map, so the writes must land in the order they were asked for: one task takes them from a queue, which a spawn per write
/// would not guarantee. `flush` waits for the queue to empty and must be awaited before the process ends, or a draft stashed on
/// quit would be lost.
struct Disk(UnboundedSender<DiskJob>);

impl Disk {
    fn start() -> Disk {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<DiskJob>();
        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                let _ = tokio::task::spawn_blocking(job).await;
            }
        });
        Disk(tx)
    }

    /// Queue a write. A failed write is dropped, as these files are a convenience (the next save writes the whole state again).
    fn write(&self, job: impl FnOnce() -> std::io::Result<()> + Send + 'static) {
        let _ = self.0.send(Box::new(move || {
            let _ = job();
        }));
    }

    async fn flush(&self) {
        let (done, wait) = tokio::sync::oneshot::channel();
        if self.0.send(Box::new(move || {
            let _ = done.send(());
        })).is_ok() {
            let _ = wait.await;
        }
    }
}

/// Run `work` off the loop and send what it came to back as the event `done` makes. An error becomes the text the UI shows
/// (`{err:#}`, the whole chain). Every relay call a command makes goes through this, so an arm of `run` only says which call
/// and which event.
fn request<T: Send + 'static>(
    tx: &UnboundedSender<AppEvent>,
    work: impl std::future::Future<Output = Result<T>> + Send + 'static,
    done: impl FnOnce(Result<T, String>) -> AppEvent + Send + 'static,
) {
    let tx = tx.clone();
    tokio::spawn(async move {
        let _ = tx.send(done(work.await.map_err(|e| format!("{e:#}"))));
    });
}

/// Sign and send the event a builder makes (a builder that refuses its input is an error here, before anything is sent).
async fn submit(relay: Arc<Relay>, builder: Result<nostr::EventBuilder>) -> Result<String> {
    relay.submit(builder?).await
}

/// Start the work a command asks for; the result comes back as an `AppEvent`. Each arm only names the call and the event it
/// answers with: protocol knowledge belongs in `relay.rs`, and what a result means in `App::handle`.
fn run(cmd: Cmd, ctx: &Ctx) {
    let (relay, tx, paths) = (ctx.relay.clone(), ctx.tx, ctx.paths.clone());
    match cmd {
        Cmd::Plugin(c) => {
            ctx.host.command(c);
        }
        Cmd::LoadChannels => request(tx, async move { relay.channels().await }, AppEvent::Channels),
        Cmd::LoadUnread { channel, since } => {
            // A count that fails (offline, a relay that will not answer) is silent: the badge simply stays empty.
            let tx = tx.clone();
            tokio::spawn(async move {
                if let Ok(count) = relay.unread(&channel, since).await {
                    let _ = tx.send(AppEvent::Unread { channel, count });
                }
            });
        }
        // The whole map goes out each time, so the writes are queued in order (see `Disk`).
        Cmd::SaveRead(state) => ctx.disk.write(move || config::save_read_state(&paths.read_state(), &state)),
        Cmd::SaveArchived(ids) => ctx.disk.write(move || config::save_archived(&paths.archived(), &ids)),
        Cmd::SaveDrafts(drafts) => ctx.disk.write(move || config::save_drafts(&paths.drafts(), &drafts)),
        // The relay first, the disk second: the agent is published and only then written down, and deleted the same way round. A
        // relay that refuses it means no agent (or no deletion), and the error is what the overlay shows.
        Cmd::CreateAgent { name, instructions, harness, model, effort } => request(
            tx,
            async move { agents::create_and_publish(&relay, &paths, &name, &instructions, &harness, &model, &effort).await },
            AppEvent::AgentCreated,
        ),
        Cmd::UpdateAgent { pubkey, name, instructions } => request(
            tx,
            async move { agents::update_and_publish(&relay, &paths, &pubkey, &name, &instructions).await },
            AppEvent::AgentUpdated,
        ),
        Cmd::DeleteAgent { pubkey } => request(tx, async move { agents::delete_and_unpublish(&relay, &paths, &pubkey).await }, AppEvent::AgentDeleted),
        Cmd::ProbeHarness { harness: id, model } => {
            // A harness that will not start, or will not answer ACP, is reported rather than hung on: the probe blocks
            // (it waits on the harness), so it runs on the blocking pool, off the loop and off the async workers.
            request(
                tx,
                async move {
                    tokio::task::spawn_blocking(move || match harness::get(&id) {
                        Some(h) => harness::offer(h, model.as_deref()),
                        None => Err(format!("{id} is not a harness buzzr knows")),
                    })
                    .await
                    .map_err(|e| anyhow!("the harness check failed: {e}"))?
                    .map_err(anyhow::Error::msg)
                },
                AppEvent::HarnessOffer,
            )
        }        Cmd::LoadWindow { channel, cursor } => {
            let (c, k) = (channel.clone(), cursor.clone());
            request(
                tx,
                async move { relay.window(&c, k.as_ref().map(|(at, id)| (*at, id.as_str())), relay::PAGE).await },
                move |result| AppEvent::Window { channel, cursor, result },
            )
        }
        Cmd::LoadThread { channel, root } => {
            let (c, r) = (channel.clone(), root.clone());
            request(tx, async move { relay.thread(&c, &r, relay::THREAD_LIMIT).await }, move |result| AppEvent::Thread { channel, root, result })
        }
        Cmd::LoadMembers { channel } => {
            let c = channel.clone();
            request(tx, async move { relay.members(&c).await }, move |result| AppEvent::Members { channel, result })
        }
        Cmd::LoadNames(pubkeys) => request(tx, async move { relay.names(&pubkeys).await }, AppEvent::Names),
        Cmd::Send { channel, text, root, mentions } => {
            request(tx, submit(relay, relay::message_builder(&channel, &text, root.as_deref(), &mentions)), AppEvent::Sent)
        }
        Cmd::Typing { channel, root } => {
            // best effort: a lost typing indicator is not worth a message
            tokio::spawn(async move {
                let _ = submit(relay, relay::typing_builder(&channel, root.as_deref())).await;
            });
        }
        Cmd::Edit { channel, id, text } => request(tx, submit(relay, relay::edit_builder(&channel, &id, &text)), AppEvent::Sent),
        Cmd::Delete { channel, id } => request(tx, submit(relay, relay::delete_builder(&channel, &id)), AppEvent::Sent),
        Cmd::React { target, emoji, remove } => request(
            tx,
            async move {
                // Undo the reaction this one replaces first, so a change never leaves two emoji from the same person.
                let mut sent = String::new();
                if let Some(reaction) = remove.as_deref() {
                    sent = submit(relay.clone(), relay::remove_reaction_builder(reaction)).await?;
                }
                if !emoji.is_empty() {
                    sent = submit(relay, relay::reaction_builder(&target, &emoji)).await?;
                }
                Ok(sent)
            },
            AppEvent::Sent,
        ),
        Cmd::CreateChannel { name, private, about } => request(
            tx,
            async move {
                let (builder, id) = relay::create_channel_builder(&name, private, about.as_deref())?;
                relay.submit(builder).await?;
                Ok(id)
            },
            AppEvent::Created,
        ),
        Cmd::JoinChannel { channel } => {
            let work = submit(relay, relay::join_builder(&channel));
            request(tx, work, move |result| AppEvent::Joined { channel, result })
        }
        Cmd::LeaveChannel { channel } => {
            let work = submit(relay, relay::leave_builder(&channel));
            request(tx, work, move |result| AppEvent::Left { channel, result })
        }
        Cmd::LoadRoster { channels } => request(tx, async move { relay.roster(&channels).await }, AppEvent::Roster),
        Cmd::OpenDm { peer } => request(tx, async move { submit(relay, relay::dm_open_builder(&peer)).await.map(|_| ()) }, AppEvent::DmOpened),
        Cmd::Invite { channel, pubkey, dm } => {
            let work = submit(relay, relay::invite_builder(&channel, &pubkey, dm));
            request(tx, async move { work.await.map(|_| ()) }, move |result| AppEvent::Invited { channel, pubkey, result })
        }
        Cmd::LoadProfile(pubkey) => {
            let p = pubkey.clone();
            request(tx, async move { relay.profile(&p).await }, move |result| AppEvent::Profile { pubkey, result })
        }
        Cmd::FindPeople { query } => {
            let q = query.clone();
            request(tx, async move { relay.find_people(&q).await }, move |result| AppEvent::People { query, result })
        }
        Cmd::Search { query, seq } => request(tx, async move { relay.search(&query, 25).await }, move |result| AppEvent::Search { seq, result }),
        Cmd::Subscribe(ids) => {
            let _ = ctx.subscription.send(ids);
        }
        Cmd::OpenUrl(url) => {
            let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
            let _ = std::process::Command::new(opener).arg(url).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
        }
    }
}
