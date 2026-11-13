# AGENTS.md: working on buzzr

buzzr is a terminal client for Buzz (a Nostr-based chat for people and agents) that speaks to the relay directly. Read this file and
`ARCHITECTURE.md` before changing code.

## Layout

See the module table in `ARCHITECTURE.md`. Tests sit next to the code (`#[cfg(test)]`), plus `src/app/tests.rs`.
`tests/e2e.rs` is the end-to-end suite: it starts the real binary in a pty against
[`buzz-fake-relay`](https://github.com/buzzzr/buzz-fake-relay) and checks the screen it draws — run it with
`tests/e2e/run.sh` or `./build.sh e2e` (see `tests/e2e/README.md`). A change to navigation, to what a pane
or overlay shows, or to a key binding adds a case there.
`tests/fixtures/key-v2.json` is a key file written by an earlier build (see principle 4). `PLUGIN.md` is the contract for writing a
plugin and `examples/plugins/hello` a working one.

## Build and test

```bash
./build.sh check       # fast: cargo check --all-targets
./build.sh test        # all unit tests; a change is not done until this is green and warning-free
./build.sh e2e         # the real binary in a pty against buzz-fake-relay (tests/e2e/run.sh)
./build.sh release     # optimised binary at dist/buzzr
```
`build.sh` runs cargo locally if it is installed, otherwise in a Rust container (Docker), or on another machine with
`BUZZR_BUILD_HOST=<ssh host>`. See the comments at the top of the script.

### Trying it against a real relay

Use an identity you are allowed to test with, in a throwaway config directory so your own settings are untouched:
```bash
export BUZZR_CONFIG_DIR=$(mktemp -d) BUZZ_RELAY_URL=wss://your-relay BUZZ_PRIVATE_KEY=...
buzzr --selftest --listen 12 --probe-typing some-quiet-channel   # headless, read-only apart from one ephemeral typing event
buzzr --read-only                                                 # the UI, cannot send
```
`--read-only` for every UI session with an identity that is not yours to speak for. To drive the UI from a script use `tmux`
(`send-keys`; Alt+Enter is `M-Enter`; mouse clicks are SGR sequences, e.g. `printf '\033[<0;5;5M\033[<0;5;5m'` through `send-keys -l`) and
`capture-pane -p` to read the screen. `--probe-typing` makes the identity "type" for a few seconds in the named channel: use a quiet one.
Test the first-run questions in a real terminal with an empty `BUZZR_CONFIG_DIR` and the three variables unset.

## Principles

1. **`App` is pure.** Behaviour goes in `App::handle(AppEvent) -> Vec<Cmd>`; the loop in `main.rs` only runs commands and feeds results
   back. No network, clock-dependent decisions beyond `Instant`, or terminal access inside `app.rs`. Every behaviour gets a test in
   `app/tests.rs`.
2. **Protocol knowledge lives in `relay.rs` and `live.rs`** (filters, endpoints, auth, tags), and event semantics in `model.rs`. Use the
   Buzz crates (`buzz-sdk` builders, `buzz-ws-client` parsing/auth) instead of re-deriving formats; they are pinned to the relay's commit.
3. **Message text is hostile.** Everything from the network that reaches the screen goes through `markdown::render` or is plain text
   in a `Span`; never write an escape sequence built from message data (the only one is OSC 52 with base64 of text the user asked to
   copy). Keep `strip_controls` first in the pipeline. Plugin output is hostile in the same way: it goes through
   `contrib::clean_line`/`clean_block` and is clipped to the space left before it reaches a `Span`.
4. **The key never reaches argv, logs or the screen, and is stored only by the user's choice.** `Zeroizing` for key material;
   `rpassword` for prompts. The setup (`setup.rs`) offers a password-protected file (default), a plain mode-600 file with a warning, or
   nothing; a plain file other users can read is refused. Any new place that stores or reads the key must keep those properties. The
   key file format is versioned: changes must keep `opens_a_file_written_by_an_earlier_build` passing (or bump the version and keep
   reading the old one).
5. **Do not move the reader.** Any change to the content goes through `App::relayout` (anchor, change, restore). Do not rebuild `layout`
   or touch `state.scroll` elsewhere without the same care. `list.rs` has the rules and tests.
6. **Never block the loop.** All I/O is in spawned tasks; results arrive as events. No `std::thread::sleep`, no blocking reads.
7. **Fail visibly.** Errors become a flash message or the connection state; nothing the user typed is dropped silently (a failed send
   is reported). Background retries are fine.
8. **Small and boring.** One crate, no plugin code in the core: plugins are out-of-process and off by default (`--no-plugins`,
   `BUZZR_SAFE_MODE=1`). The only config is the relay in `config.json`. Prefer deleting code to adding options.
9. **Questions go through `setup::Io`.** Anything that asks the user something before the UI starts must use that trait so the flow can
   be tested with scripted answers; no `read_line` elsewhere.

## Conventions

- Rust 2021, match the surrounding style (long lines, about 140 columns; there is no rustfmt config, so do not run `cargo fmt` over the whole tree), comments say why. No new `unwrap` on data from the network.
- A new key, command or overlay: update `ui.rs` (`KEYS`, `HELP`, footer), `README.md` and the tests in the same change.
- Wide characters: measure with `unicode_width`, never `len()`/`chars().count()` for layout. The test backend shows an emoji as symbol +
  blank cell, so assertions should not depend on the exact gap.
- Commits: small, imperative subject. Follow `CONTRIBUTING.md`.

## Things that already bit us

- **Thread replies:** a nested reply has both a `root` and a `reply` tag; a direct reply only a `reply` tag. Use `Ev::root_tag/reply_tag`,
  not "the first `e` tag". A reply can arrive before its parent: it is listed (marked) until the parent loads.
- **Events without an `h` tag** (deletions, some reactions): `App::route` finds the channel through the message they point at.
- **NIP-CW window:** `has_more` and `next_cursor` come from the relay's `39006` overlay and only count if its `d` tag ends with the cursor we
  sent; never infer exhaustion from the row count. A head reload must not reset the cursor of a channel we already paged into.
- **Typing events are ephemeral:** not stored, delivered only to live subscribers; so there is nothing to fetch on startup. They carry the
  channel in `h`. Our own are ignored in the UI.
- **ratatui:** `Line::style` fills the line's whole width (used for the selection tint); the first column of the message area is the selection
  gutter, so link columns are offset by one when hit-testing.
- **crossterm:** Shift+Enter is only reported by terminals that speak the kitty keyboard protocol (we push `DISAMBIGUATE_ESCAPE_CODES` when
  supported). Elsewhere it arrives as Enter, so Alt+Enter and Ctrl+J (`Char('j')` with CONTROL in raw mode) stay as the portable newline.
- **tmux** does not pass OSC 52 to the outer terminal unless `set -g set-clipboard on`; `tmux show-buffer` shows what arrived.
- **Docker volumes are root-owned:** in the ssh/rsync build mode `rsync --delete` must exclude `dist/` and `target/` (build.sh does).
- Relay errors: a key that is not a member of the community gets `403 You must be a relay member to access this relay`.

- **Search** is debounced (350 ms after the last key, at least two characters) and every answer carries a sequence number; answers to an
  older query are ignored. A hit is applied to its channel's store before the channel is opened, and `pending_select` makes `relayout`
  select it as soon as it is in the layout (a reply opens its thread first).
- **Delete** uses `kind:9005` like `buzz messages delete`; the confirmation (`confirm_delete`) swallows the next key, so it must stay the first
  thing `key()` checks. The relay's deletion comes back live and removes the message; nothing is removed locally before that.
- **Edits** replace content only for the author (`Store::apply` checks it); an edit sent while the message is not loaded is applied when it loads.
- **Plugin JSON is camelCase** (`apiVersion`, `messageMarks`, `channelMarks`); the Rust structs `Manifest` and `Contributions` carry
  `#[serde(rename_all = "camelCase")]`. A new field without it deserialises to its default and is silently ignored, which looks like
  a plugin that "did nothing".
- **Starting a program that was just written:** the kernel refuses to `exec` a file that is still open for writing (`ETXTBSY`, "Text
  file busy"). A plugin reloaded right after an edit hits it, and so does a test that writes a fake program and runs it while another
  test forks a child — the child inherits the write handle until its own `exec`. Start such a program through `spawn::spawn`
  (`src/spawn.rs`), which tries again for a few milliseconds; a plain `Command::spawn` flakes instead.

## Known gaps (good next tasks)

- React to messages; attachments and images (shown as links); people search; creating DMs; joining/leaving channels.
- Edit/delete/new channel/typing are verified by builder unit tests and (typing, search) against a relay; the write path of edit, delete and
  channel creation is only exercised by `--selftest --probe-write`, which has to be run by someone happy for that identity to create and
  delete a private test channel.
- Mention completion while typing; recalling sent messages with Up in the input.
- Only one harness (Claude Code, `claude-agent-acp`): a second one (codex, say) is a new entry in the harness table (`src/harness/mod.rs`) and nothing else.
- Verify the relay's signatures on overlay events (`39005`, `39006`); today the authenticated HTTPS transport is trusted.
