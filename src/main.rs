use anyhow::{anyhow, Context, Result};
use buzzr::{agents, config, harness, keyfile, live, plugin, plugins, relay, release, runner, selftest, setup, ui, update};
use buzzr::app::{App, AppEvent, Cmd};
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
use tokio::sync::mpsc::UnboundedSender;

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
    let log_n = config::env("SCRYPT_LOG_N").and_then(|v| v.parse().ok()).unwrap_or(keyfile::DEFAULT_LOG_N);
    let given_relay = args.relay.or_else(|| std::env::var("BUZZ_RELAY_URL").ok().filter(|v| !v.trim().is_empty()));
    let given_key = std::env::var("BUZZ_PRIVATE_KEY").ok();
    let setup::Resolved { relay: url, key } =
        setup::resolve(&mut setup::Terminal, &paths, given_relay, given_key, args.setup, log_n).map_err(|e| anyhow!("{e}"))?;
    let keys = Keys::parse(&key).context("that is not a valid hex or nsec private key (run `buzzr --setup` to enter it again)")?;
    drop(key);
    // BUZZ_AUTH_TAG is only for agents (NIP-OA)
    let auth_tag = std::env::var("BUZZ_AUTH_TAG").ok();
    let relay = Arc::new(Relay::new(&url, keys, auth_tag)?);
    if args.selftest {
        selftest::run(relay.clone(), args.listen, args.probe_typing, args.probe_search).await?;
        if args.probe_write {
            selftest::write_probe(relay).await?;
        }
        return Ok(());
    }
    let tz = config::env("TZ").and_then(|t| t.parse().ok()).unwrap_or_else(local_timezone);
    let plugins_on = !args.no_plugins && config::env("SAFE_MODE").as_deref() != Some("1");
    let dirs = plugins_on.then(|| (plugins_dir, paths.plugin_logs()));
    let notice = run_tui(relay, args.read_only, !args.no_typing, !args.no_mouse, tz, dirs, &paths).await?;
    if let Some(notice) = notice {
        eprintln!("{notice}"); // after the alternate screen is gone, so it does not fight the UI
    }
    Ok(())
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

async fn run_tui(relay: Arc<Relay>, read_only: bool, send_typing: bool, mouse: bool, tz: chrono_tz::Tz, plugin_dirs: Option<(PathBuf, PathBuf)>, paths: &config::Paths) -> Result<Option<String>> {
    // restore the terminal before a panic message is printed, or it would be unreadable
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), DisableMouseCapture, DisableBracketedPaste, LeaveAlternateScreen);
        default_hook(info);
    }));

    let guard = TerminalGuard::enter(mouse)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut app = App::new(relay.me(), read_only, tz);
    app.send_typing = send_typing;
    // Shift+Enter only reaches us where the terminal speaks the kitty keyboard protocol; the footer names the key that works here.
    app.keyboard_enhanced = guard.enhanced;
    app.update_source = update::InstallSource::detect();
    // Where each channel was left last time; the badge counts come back over the relay (see `Cmd::LoadUnread`).
    app.seed_read(config::load_read_state(&paths.read_state()));
    // The channels archived on this device: they have no row in the channel list until a search brings one back.
    app.seed_archived(config::load_archived(&paths.archived()));
    // What was typed but not sent, per channel and thread: it comes back into the box where it was written.
    app.seed_drafts(config::load_drafts(&paths.drafts()));
    // The local agents buzzr made: the identity and the folder each one works in, from `agents.json` (see `agents.rs`).
    app.seed_agents(agents::load(&paths.agents()));
    // The compiled-in plugins this build registered (see `plugin`): their commands, contributions and permissions, kept
    // beside the external ones the `plugins::Host` supervises. Both are driven through the same contract, and both kinds
    // are off together — `plugin_dirs` is `None` under `--no-plugins` and `BUZZR_SAFE_MODE=1`, so this seeds nothing then.
    if plugin_dirs.is_some() {
        app.seed_plugins(plugin::PluginRuntime::new());
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    // The release feed: seed the notice from what the last check learned, and ask GitHub again only when that is stale. A failure
    // (offline, no release yet) is silent — see `release.rs`.
    if release::enabled() {
        let state = release::State::load(&paths.release_state());
        if let Some(v) = state.newer_than(release::CURRENT) {
            app.latest = Some(v.to_owned());
        }
        if state.is_due(release::now_secs()) {
            let (tx, path) = (tx.clone(), paths.release_state());
            tokio::spawn(async move {
                if let Some(v) = release::check_and_store(&path).await {
                    let _ = tx.send(AppEvent::Update(v));
                }
            });
        }
    }
    let host = match &plugin_dirs {
        Some((dir, logs)) => plugins::Host::start(dir.clone(), logs.clone(), tx.clone(), read_only),
        None => plugins::Host::none(),
    };
    let mut live = live::spawn(relay.clone(), Vec::new());
    // The local agents: a mention of one is answered by its harness (see `runner.rs`). An answer is a message, so a
    // read-only run, and safe mode, start no watcher at all — an agent that could not send would only report failures.
    if !read_only && config::env("SAFE_MODE").as_deref() != Some("1") {
        runner::spawn(relay.clone(), paths.clone(), tx.clone());
    }
    for cmd in app.start() {
        run(cmd, &relay, &tx, &live.channels, &host, paths);
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
                run(cmd, &relay, &tx, &live.channels, &host, paths);
            }
            host.shutdown();
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
                        run(cmd, &relay, &tx, &live.channels, &host, paths);
                    }
                    host.shutdown();
                    return Ok(exit_notice(&app));
                }
            },
            e = rx.recv() => e,
            l = live.rx.recv() => l.map(AppEvent::Live),
            _ = tick.tick() => Some(AppEvent::Tick),
        };
        if let Some(ev) = event {
            let ticked = matches!(ev, AppEvent::Tick);
            for cmd in app.handle(ev) {
                run(cmd, &relay, &tx, &live.channels, &host, paths);
            }
            // A compiled-in plugin's heartbeat, on the same tick the host drives an external one's.
            if ticked {
                for cmd in app.plugin_tick() {
                    run(cmd, &relay, &tx, &live.channels, &host, paths);
                }
            }
        }
        // Only what changed is sent, and only to plugins that are ready; a stalled plugin never slows the loop. The
        // compiled-in plugins are shown the same projection, on the same "only what changed" rule.
        for cmd in app.plugin_snapshot(app.projection()) {
            run(cmd, &relay, &tx, &live.channels, &host, paths);
        }
        host.snapshot(app.projection());
    }
}

/// The stderr notice for a session that has ended: only when the feed named a newer release, and only once the screen is back.
fn exit_notice(app: &App) -> Option<String> {
    app.latest.as_ref().map(|v| release::notice(release::CURRENT, v, app.update_source))
}

/// Make a local agent and publish it to the relay. The relay comes first: the agent is checked and its key made in
/// memory, published, and only written down (its folder and its record) once the relay has taken it — so an agent the
/// relay refuses is not created at all. The harness, model and thinking level the flow picked are named here; the
/// harness is one buzzr knows (or `prepare` refuses it), and the picks may be empty for the harness's own choice.
async fn create_and_publish(
    relay: &Relay,
    paths: &config::Paths,
    name: String,
    instructions: String,
    harness: String,
    model: String,
    effort: String,
) -> Result<agents::Agent, String> {
    let pending = agents::prepare(paths, &name, &instructions, &harness, &model, &effort)?;
    publish_agent(relay, &pending).await?;
    agents::commit(paths, &pending)
}

/// The relay half of `/agents create`: the agent's own kind-0 profile — signed by the agent's key and sent as the
/// agent, carrying this identity's attestation — and then this identity's listing of it. Either failing is reported,
/// and nothing is written locally, so the agent is not created.
async fn publish_agent(relay: &Relay, pending: &agents::Pending) -> Result<(), String> {
    let agent = pending.agent();
    // The listing names the agent type the harness runs (`pluginId/typeId`), so an owner's client can resolve it.
    let runtime = harness::get(&agent.harness).map(|h| h.runtime).unwrap_or(harness::DEFAULT.runtime);
    let attestation = agents::attestation(relay.keys(), &agent.pubkey)?;
    relay.publish_agent_profile(pending.keys(), &agent.name, &attestation).await.map_err(|e| format!("{e:#}"))?;
    relay.publish_agent_listing(&agent.pubkey, &agent.name, runtime).await.map_err(|e| format!("{e:#}"))?;
    Ok(())
}

/// Delete a local agent and take it off the relay: the agent's own kind-0 profile — signed by its own key, and sent as
/// the agent, carrying the owner's attestation, exactly as its profile and its answers are — and then this identity's
/// kind-30177 listing of it. Only once the relay has taken both is the record (with the only copy of its key) deleted
/// from `agents.json`, so a relay that refuses it leaves the agent in place, listed and running. The folder the agent
/// worked in is left alone, as deleting an agent in Buzz leaves its working directory.
async fn delete_and_unpublish(relay: &Relay, paths: &config::Paths, pubkey: &str) -> Result<agents::Agent, String> {
    let Some((agent, keys)) = agents::load_with_keys(&paths.agents()).into_iter().find(|(a, _)| a.pubkey == pubkey) else {
        return Err(format!("{pubkey} is not an agent on this device"));
    };
    let owner = relay.keys().public_key().to_hex();
    let attestation = agents::attestation(relay.keys(), &agent.pubkey)?;
    let own = Relay::new(relay.base_url(), keys, Some(attestation)).map_err(|e| format!("{e:#}"))?;
    own.delete_coordinate(&format!("0:{}:", agent.pubkey)).await.map_err(|e| format!("{e:#}"))?;
    relay.delete_coordinate(&format!("30177:{owner}:{}", agent.pubkey)).await.map_err(|e| format!("{e:#}"))?;
    agents::remove(paths, &agent.pubkey)
}

/// Start the work a command asks for; the result comes back as an `AppEvent`.
fn run(cmd: Cmd, relay: &Arc<Relay>, tx: &UnboundedSender<AppEvent>, subscription: &tokio::sync::watch::Sender<Vec<String>>, host: &plugins::Host, paths: &config::Paths) {
    let (relay, tx) = (relay.clone(), tx.clone());
    let e = |err: anyhow::Error| format!("{err:#}");
    match cmd {
        Cmd::Plugin(c) => {
            host.command(c);
        }
        Cmd::LoadChannels => {
            tokio::spawn(async move {
                let _ = tx.send(AppEvent::Channels(relay.channels().await.map_err(e)));
            });
        }
        Cmd::LoadUnread { channel, since } => {
            // A count that fails (offline, a relay that will not answer) is silent: the badge simply stays empty.
            tokio::spawn(async move {
                if let Ok(count) = relay.unread(&channel, since).await {
                    let _ = tx.send(AppEvent::Unread { channel, count });
                }
            });
        }
        Cmd::SaveRead(state) => {
            // Written here rather than spawned: each save is the whole map, and two writes racing in the runtime could land in
            // the order they were spawned, letting an older position overwrite a newer one. A small file is cheap to write.
            let _ = config::save_read_state(&paths.read_state(), &state);
        }
        Cmd::SaveArchived(ids) => {
            // Written here for the same reason as the read positions: the whole set goes out each time, and a spawned write
            // could race an older one.
            let _ = config::save_archived(&paths.archived(), &ids);
        }
        Cmd::SaveDrafts(drafts) => {
            // Written here too: each save is the whole map, so an older one must not land after a newer one.
            let _ = config::save_drafts(&paths.drafts(), &drafts);
        }
        Cmd::CreateAgent { name, instructions, harness, model, effort } => {
            // The relay first, the disk second: the agent is published (its own kind-0 profile carrying this
            // identity's attestation, and this identity's listing of it) and only then written down. A relay that
            // refuses it means no agent — no key, no folder, no record — and the error is what the overlay shows.
            let paths = paths.clone();
            tokio::spawn(async move {
                let _ = tx.send(AppEvent::AgentCreated(create_and_publish(&relay, &paths, name, instructions, harness, model, effort).await));
            });
        }
        Cmd::DeleteAgent { pubkey } => {
            // The relay first, the disk second, as creation does: the agent is taken off the relay (its own profile,
            // signed by its key, and this identity's listing of it) and only then deleted here. A relay that refuses
            // it means no deletion, so the agent stays listed and running.
            let paths = paths.clone();
            tokio::spawn(async move {
                let _ = tx.send(AppEvent::AgentDeleted(delete_and_unpublish(&relay, &paths, &pubkey).await));
            });
        }
        Cmd::ProbeHarness { harness: id, model } => {
            // A harness that will not start, or will not answer ACP, is reported rather than hung on: the probe blocks
            // (it waits on the harness), so it runs on the blocking pool, off the loop and off the async workers.
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || match harness::get(&id) {
                    Some(h) => harness::offer(h, model.as_deref()),
                    None => Err(format!("{id} is not a harness buzzr knows")),
                })
                .await
                .unwrap_or_else(|e| Err(format!("the harness check failed: {e}")));
                let _ = tx.send(AppEvent::HarnessOffer(result));
            });
        }
        Cmd::LoadWindow { channel, cursor } => {
            tokio::spawn(async move {
                let result = relay.window(&channel, cursor.as_ref().map(|(at, id)| (*at, id.as_str())), relay::PAGE).await.map_err(e);
                let _ = tx.send(AppEvent::Window { channel, cursor, result });
            });
        }
        Cmd::LoadThread { channel, root } => {
            tokio::spawn(async move {
                let result = relay.thread(&channel, &root, relay::THREAD_LIMIT).await.map_err(e);
                let _ = tx.send(AppEvent::Thread { channel, root, result });
            });
        }
        Cmd::LoadMembers { channel } => {
            tokio::spawn(async move {
                let result = relay.members(&channel).await.map_err(e);
                let _ = tx.send(AppEvent::Members { channel, result });
            });
        }
        Cmd::LoadNames(pubkeys) => {
            tokio::spawn(async move {
                let _ = tx.send(AppEvent::Names(relay.names(&pubkeys).await.map_err(e)));
            });
        }
        Cmd::Send { channel, text, root, mentions } => {
            tokio::spawn(async move {
                let result = async { relay.submit(relay::message_builder(&channel, &text, root.as_deref(), &mentions)?).await }.await.map_err(e);
                let _ = tx.send(AppEvent::Sent(result));
            });
        }
        Cmd::Typing { channel, root } => {
            tokio::spawn(async move {
                // best effort: a lost typing indicator is not worth a message
                if let Ok(b) = relay::typing_builder(&channel, root.as_deref()) {
                    let _ = relay.submit(b).await;
                }
            });
        }
        Cmd::Edit { channel, id, text } => {
            tokio::spawn(async move {
                let result = async { relay.submit(relay::edit_builder(&channel, &id, &text)?).await }.await.map_err(e);
                let _ = tx.send(AppEvent::Sent(result));
            });
        }
        Cmd::Delete { channel, id } => {
            tokio::spawn(async move {
                let result = async { relay.submit(relay::delete_builder(&channel, &id)?).await }.await.map_err(e);
                let _ = tx.send(AppEvent::Sent(result));
            });
        }
        Cmd::React { target, emoji, remove } => {
            tokio::spawn(async move {
                // Undo the reaction this one replaces first, so a change never leaves two emoji from the same person.
                let result = async {
                    let mut sent = String::new();
                    if let Some(reaction) = remove.as_deref() {
                        sent = relay.submit(relay::remove_reaction_builder(reaction)?).await?;
                    }
                    if !emoji.is_empty() {
                        sent = relay.submit(relay::reaction_builder(&target, &emoji)?).await?;
                    }
                    Ok::<String, anyhow::Error>(sent)
                }
                .await
                .map_err(e);
                let _ = tx.send(AppEvent::Sent(result));
            });
        }
        Cmd::CreateChannel { name, private, about } => {
            tokio::spawn(async move {
                let result = async {
                    let (builder, id) = relay::create_channel_builder(&name, private, about.as_deref())?;
                    relay.submit(builder).await?;
                    Ok::<String, anyhow::Error>(id)
                }
                .await
                .map_err(e);
                let _ = tx.send(AppEvent::Created(result));
            });
        }
        Cmd::JoinChannel { channel } => {
            tokio::spawn(async move {
                let result = async { relay.submit(relay::join_builder(&channel)?).await }.await.map_err(e);
                let _ = tx.send(AppEvent::Joined { channel, result });
            });
        }
        Cmd::LeaveChannel { channel } => {
            tokio::spawn(async move {
                let result = async { relay.submit(relay::leave_builder(&channel)?).await }.await.map_err(e);
                let _ = tx.send(AppEvent::Left { channel, result });
            });
        }
        Cmd::LoadRoster { channels } => {
            tokio::spawn(async move {
                let _ = tx.send(AppEvent::Roster(relay.roster(&channels).await.map_err(e)));
            });
        }
        Cmd::OpenDm { peer } => {
            tokio::spawn(async move {
                let result = async { relay.submit(relay::dm_open_builder(&peer)?).await.map(|_| ()) }.await.map_err(e);
                let _ = tx.send(AppEvent::DmOpened(result));
            });
        }
        Cmd::Invite { channel, pubkey, dm } => {
            tokio::spawn(async move {
                let result = match relay::invite_builder(&channel, &pubkey, dm) {
                    Ok(builder) => relay.submit(builder).await.map(|_| ()).map_err(e),
                    Err(err) => Err(e(err)),
                };
                let _ = tx.send(AppEvent::Invited { channel, pubkey, result });
            });
        }
        Cmd::LoadProfile(pubkey) => {
            tokio::spawn(async move {
                let result = relay.profile(&pubkey).await.map_err(e);
                let _ = tx.send(AppEvent::Profile { pubkey, result });
            });
        }
        Cmd::FindPeople { query } => {
            tokio::spawn(async move {
                let result = relay.find_people(&query).await.map_err(e);
                let _ = tx.send(AppEvent::People { query, result });
            });
        }
        Cmd::Search { query, seq } => {
            tokio::spawn(async move {
                let result = relay.search(&query, 25).await.map_err(e);
                let _ = tx.send(AppEvent::Search { seq, result });
            });
        }
        Cmd::Subscribe(ids) => {
            let _ = subscription.send(ids);
        }
        Cmd::OpenUrl(url) => {
            let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
            let _ = std::process::Command::new(opener).arg(url).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
        }
    }
}
