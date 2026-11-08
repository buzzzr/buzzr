# buzzr architecture

## Shape

```
terminal ◄──────────────── ui.rs (ratatui)  draws App; reports the geometry it used
   │ keys, mouse, paste                          ▲
   ▼                                             │ reads
main.rs: event loop ── AppEvent ──► app.rs: App::handle(event) -> Vec<Cmd>   (pure: no network, no terminal)
   ▲   │                                         │
   │   └── runs each Cmd as a tokio task ◄───────┘ Cmd::{LoadWindow, LoadThread, Send, Subscribe, ...}
   │                  │
   │                  ▼
   │          relay.rs  HTTP: POST /query, POST /events (NIP-98 signed)      ──► Buzz relay
   └── live.rs  WebSocket: NIP-42 auth, one REQ for all channels, reconnects ◄── pushes events
```
Everything the user can do is `App::handle(AppEvent) -> Vec<Cmd>`. The loop turns `Cmd`s into requests and their answers back
into `AppEvent`s. Because `App` never touches the network or the terminal, its behaviour is tested with synthetic events.

## Modules

| File | Role |
|---|---|
| `main.rs` | Arguments, wiring of setup and relay, terminal setup/restore (raw mode, alternate screen, mouse, bracketed paste, kitty keyboard flags), the `select!` loop, `run(Cmd)`. |
| `app.rs` | State and behaviour: channels, stores, view (channel or thread), focus, overlays, selection and scroll, input, commands, mentions, unread, typing, clipboard requests. `app/tests.rs` has the tests. |
| `ui.rs` | Drawing: header, channel list, message list, typing line, status line, input box, footer, key panel, help. |
| `list.rs` | Geometry of the message list: offsets, hit-testing, anchors that keep the reader's place, selection scrolling. Pure. |
| `markdown.rs` | Message text -> wrapped styled lines + link positions. Strips control characters first. |
| `model.rs` | `Ev` (an event), `Channel`, `Summary`, and `Store`: everything loaded for one channel (messages, edits, deletions, reactions, thread summaries) and the views over it (timeline, thread). |
| `editor.rs` | The multi-line input: cursor, word movement, kills, wrapping rows. |
| `relay.rs` | HTTP client: NIP-98 signing, queries (channels, members, names, window, thread), `submit`, message/typing builders. |
| `live.rs` | The WebSocket task: NIP-42, subscription, reconnect with backoff, channel list changes. |
| `config.rs` | Where settings live (`~/.config/buzzr`), the saved relay, relay address normalisation, private file writes, the last-read position per channel (`read-state.json`), `BUZZR_*` variables. |
| `setup.rs` | First-run setup and key loading: asks for what is missing (relay, key, how to keep it) through an `Io` trait so the flow is tested with scripted answers. |
| `keyfile.rs` | The password-protected key file format (scrypt + AES-256-GCM): `encrypt` and `decrypt`, no I/O. |
| `selftest.rs` | `--selftest`: a read-only check against a real relay. |
| `contrib.rs` | What a plugin is allowed to put on the screen (segments, panels, marks) and the `Registry` the app holds; also the bounded `Projection` sent to plugins and the `PluginCmd`/`PluginEvent` that cross between `App` and the plugin runtime. |
| `plugins/` | The out-of-process plugin host: `proto.rs` is the newline-delimited-JSON wire, `manifest.rs` finds plugin folders and their hot-reload stamps, `host.rs` is the one task that supervises every child process. See `PLUGIN.md`. |
| `update.rs` | How the running copy was installed (Homebrew's Cellar, `~/.cargo/bin`, a checkout build, anything else) and the shell command that upgrades it, for `/update`. Path inspection only, no network. |
| `release.rs` | The release feed: one throttled GET of the repository's latest release (at most once a day; `BUZZR_NO_UPDATE_CHECK` skips it), the `State` remembered between runs, and the notice printed after the UI closes. Failures are silent. |

## Plugins

A plugin is a separate process that speaks newline-delimited JSON on stdio, in any language; buzzr owns every cell of the
terminal. This keeps the core's shape: a plugin is a **third edge** next to `relay.rs` and `live.rs`.

```
                       ┌─────────────────────────┐
plugin process ◄───────┤ host.rs (one tokio task) ├───────┐
  stdio / NDJSON       │ spawn, handshake,       │       │
   (any language)      │ hot reload, timeouts    │       │
                       └─────────────────────────┘       │
        ▲                                                 ▼
        │ AppEvent::Plugin                      Cmd::Plugin
        └─────────────── app.rs: App::handle (pure) ──────┘
```

Plugin input arrives as `AppEvent::Plugin` and plugin requests leave as `Cmd::Plugin`, so `App` stays plugin-unaware and every
behaviour is testable with synthetic events. Inbound is a bounded `Projection`: only the rows on screen, never the key, the relay
address or another channel. Outbound is default-deny — `draft`, a gated `send` (only in the open channel, needs `messages:write`,
rate-limited), and panel actions, all refused under `--read-only`. Contributions are plain data (status items, badges, message and
channel marks, panels) mapped onto surfaces that already exist, so a plugin never draws, never sees the key (the host signs as the
user when something must be sent) and can never emit an escape sequence. Plugins are off with `--no-plugins` or `BUZZR_SAFE_MODE=1`.
The contract is in `PLUGIN.md` and a working plugin in `examples/plugins/hello`.

## Protocol (what is asked of the relay)

All reads are `POST /query` with a JSON array of Nostr filters and an `Authorization: Nostr <base64 NIP-98 event>` header (fresh nonce
per request, `payload` = SHA-256 of the body). An agent identity also sends its NIP-OA tag in `x-auth-tag`.

| Need | Filter |
|---|---|
| channels | `{"kinds":[39000],"limit":500}` (`d` = id, `name`, `about`; the name `DM` and the `t` label `buzz.solo` pick the list section: Solo, Channels, DMs) |
| members | `{"kinds":[39002],"#d":[channel],"limit":1}` (`p` tags, role in the 4th element) |
| names | `{"kinds":[0],"authors":[...]}` (`display_name`, else `name`) |
| timeline page | `{"kinds":[9,40002,40008,45001,45003],"#h":[channel],"limit":50,"top_level":true,"include_summaries":true,"include_aux":true}` plus `until`+`before_id` for older pages. Answer: rows, relay-signed `39005` thread summaries (`reply_count`, `last_reply_at`), reactions/edits/deletions of those rows, and one `39006` bounds event (`has_more`, `next_cursor`) whose `d` tag must end with the cursor we sent (NIP-CW) |
| unread count | `{"kinds":[9,40002,40008,45001,45003],"#h":[channel],"since":<last read + 1>,"limit":200}`, counted on startup for each channel that has a stored read position; only non-own messages count, and 200 is the ceiling (`relay::UNREAD_LIMIT`) |
| thread | `[{"kinds":[9,40002,40003,40008,45003],"#h":[c],"#e":[root]}, {"ids":[root]}]`, then `{"kinds":[7,5,9005],"#e":[ids]}` for reactions and deletions |
| search | `{"kinds":[9,40002,45001,45003],"search":text,"limit":25}` (NIP-50 full text, most relevant first) |
| edit / delete / new channel | `POST /events` with `kind:40003` (`h`, `e`), `kind:9005` (`h`, `e`), `kind:9007` (`h`, `name`, `visibility`, `channel_type`, `about`), built with the same `buzz-sdk` builders the CLI uses |
| typing | `kind:20002` (`h`, in a thread also `e` = root) over `POST /events`, at most once per 3 s while typing |
| send | `POST /events` with the signed kind-9 event (`h` tag; thread replies add `["e", root, "", "reply"]`; mentions add `p` tags) |

Live: connect, answer the NIP-42 `AUTH` challenge, then one `REQ` with all channel ids in `#h`, `since` = now, for kinds
messages, edits (40003), deletions (5, 9005), reactions (7), thread summaries (39005) and typing (20002). Typing events are
ephemeral: the relay never stores them, so they only exist on this connection. On every reconnect the open view is reloaded.

## Threads

Replies carry `["e", id, "", "reply"]`; nested replies also carry `["e", root, "", "root"]`. `Ev::is_top_level` follows the relay's
rule (no reply/root tags, or a broadcast direct reply). A channel lists top-level messages plus replies whose parent is not
loaded (so nothing disappears); a thread is the root plus every message whose root it is (`Store::thread_root`, which also climbs
through replies from older clients that only tagged their parent). Reply counts come from the relay's `39005` summary, falling
back to what is loaded.

## State

`App` holds one `Store` per channel (loaded messages and everything attached to them), the current `View` (`Channel` or
`Thread(root)`), a `ListState` (`selected` message id, `scroll` in lines) per view (saved in `saved` so Esc returns to the same
place), the input `Editor`, an optional `reply_target` (thread root), connection state, per-channel unread counts and the
last-read position each one was computed from, typing indicators (`(channel, pubkey) -> Instant`, shown for 7 s) and a
short-lived flash message.

## Layout and scrolling

`Layout` is the list of `Rendered` messages (header, body, reactions, then one blank separator line) at a given width, with line
offsets. `App::relayout` rebuilds it after any change and re-applies an `Anchor` taken just before: the message at the top of the
screen and how many of its lines were scrolled off. Result: new messages follow only when the reader was at the bottom; older
messages loaded above, edits and changed counts keep the same message at the same screen position. Selecting scrolls just enough.
Clicks map `(row, col)` -> message line -> link via the same layout the drawing used.

## Safety

Message text is hostile. `markdown::strip_controls` removes ESC, BEL and other control characters before parsing, and the renderer
only produces text and styles, never terminal sequences. The one escape sequence the program emits from message data is OSC 52
(clipboard), whose payload is base64, and which is built only for text the user asked to copy. Links are followed only for
`http`, `https` and `mailto`. The private key is never passed on a command line and never logged; it is held in `Zeroizing` buffers until the signing key is made.
It is stored only if the user chooses to: encrypted (the key file authenticates its own parameters and refuses hostile ones before
doing any work) or, as an explicit choice with a warning, as a plain file with mode 600 in a mode 700 directory (a plain file that
others can read is refused). The default choice in the setup is the password.

## Failure behaviour

Relay errors show in the status line (`could not load messages: ...`); the live task retries with backoff (1 s doubling to 20 s)
and the header shows `offline, retrying`; sends that fail show `send failed: ...` and are not retried. A panic restores the terminal
first.

## Testing

- `./build.sh test`: unit tests for every module; `app/tests.rs` drives `App` with synthetic keys, mouse events and relay events;
  `ui.rs` tests draw into ratatui's `TestBackend`; `keyfile.rs` opens a fixture written by an earlier build, so a format change cannot go unnoticed; `setup.rs` runs the whole first-run flow with scripted answers in a temporary directory.
- `buzzr --selftest --probe-typing CHANNEL` with an identity but no terminal (for example a container): real authentication, queries, thread, live
  connection, and a typing indicator submitted over HTTP that must come back over the WebSocket.
- A tmux drive of the real UI with `--read-only` (see `AGENTS.md`).
