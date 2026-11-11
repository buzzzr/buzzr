<h1 align="center">buzzr</h1>

<p align="center">
  A fast, keyboard-driven terminal client for <a href="https://github.com/block/buzz">Buzz</a>,
  the Nostr-based chat for people and agents.<br>
  One static binary. Live push, not polling. Real typing indicators.
</p>

<p align="center">
  <a href="https://github.com/buzzzr/buzzr/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/buzzzr/buzzr/actions/workflows/ci.yml/badge.svg"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
  <img alt="Rust 1.88+" src="https://img.shields.io/badge/rust-1.88%2B-orange.svg">
</p>

<p align="center">
  <img src="docs/demo.gif" alt="A recording of buzzr: opening a channel, reading a thread, replying in it and searching" width="900">
</p>

> The screenshots and the recording in this README show a scripted demo community, not real conversations.

<p align="center">
  <b>Install:</b> <code>curl -fsSL https://raw.githubusercontent.com/buzzzr/buzzr/main/install.sh | sh</code>
</p>

## Why buzzr

- **It talks to the relay directly.** No bridge, no helper daemon, no polling loop: buzzr opens a WebSocket and the screen updates the
  moment something happens.
- **Built for the terminal.** Everything is a key or a click, it works over SSH and inside tmux, and it starts instantly.
- **Careful with your key.** The private key is sealed at rest with scrypt and AES-256-GCM by default, never appears on a command
  line, and never reaches the logs or the screen.
- **Small and boring on purpose.** One crate and one binary, nothing else to install. Extensions run as separate processes, so the core stays small.

## Features

- **Channels and DMs** with unread counts, in sections: Solo chats, Channels (private and open) and DMs. DMs are labelled with the other
  person; start one with `/dm name`, or from search. An open channel is marked `#` and a private one `*`.
- **Archive a channel.** `x` takes the channel off the list without leaving it (leaving changes your membership on the relay);
  Ctrl+F finds it again and Enter brings it back. Its unread is kept, but it no longer counts in the status line.
- **Unread that survives a restart.** The last-read position is saved per channel and per thread, so the badges are right again
  next time and a `new messages` line shows where you left off. `u` marks a message unread until you `/read` the channel.
- **Threads.** A channel shows top-level messages only, as the Buzz app does, each with its reply count and the time of the last reply;
  a thread with replies since you last opened it shows `● 2 new`. Enter opens a thread (and reads it), Esc returns to the same place.
  The channel list keeps the two kinds of unread apart as well: unseen messages count as `(2)`, unseen replies inside a thread show as
  `●`, and a channel with both shows `(2) *`.
- **Live.** New messages, edits, deletions, reactions, reply counts and `Bob is typing…` arrive as they happen. If the connection
  drops, buzzr reconnects and reloads.
- **Reads like a chat.** Markdown (bold, italic, lists, code, quotes, headings), clickable links, reactions, and edited messages say so.
- **Keeps your place.** New messages never move the view unless you were at the bottom, and older history loads above without a jump.
- **Write, edit, delete.** A multi-line input with bracketed paste and `@name` mentions. Edit or delete your own messages with `e` and `d`.
- **Search everything.** Ctrl+F searches channel names, people and every message you can read, with full text from the relay. Enter on a
  person opens the DM you already have with them, or starts a new one.
- **Create channels** from the input with `/new`.
- **Join and leave channels.** A channel you are not in opens read-only: its composer is a `[ Join ]` button, and Enter on it joins the
  channel. `/join NAME` does the same by name, and `/leave` takes you out of the open channel after asking. Ctrl+F shows what a
  channel is for, how many people are in it and whether you are one of them.
- **People and profiles.** `/whois name` shows a short card (name, npub, what they wrote about themselves); `/dm name` opens your DM with
  someone or starts a new one. Both take a name from a channel or an npub.
- **Invite people into a channel.** `/invite name` adds someone to the channel on screen (or the group DM you are in). A message
  that `@mentions` someone who is not in the channel asks first: `y` invites them and sends, `n` sends without inviting, Esc
  keeps the message in the input.
- **Plugins.** Add your own `/commands`, status items, badges, a pane under the channel list and panels from any language. See [PLUGIN.md](PLUGIN.md).
- **Themes.** The default `buzz-dark` uses the dark palette of the Buzz desktop app. `BUZZR_THEME=dark` keeps your terminal's own background, `BUZZR_THEME=buzz-light` is the desktop app's light palette, and `BUZZR_THEME=light` is for a light terminal. `BUZZR_THEME=high-contrast` is black-on-white at WCAG AAA (7:1), and `NO_COLOR` or `TERM=dumb` turns colour off entirely.
- **Read-only mode** (`--read-only`) for identities you should not speak for.

<p align="center">
  <img src="docs/screenshots/thread.png" alt="A thread open next to the channel list, in buzz-dark" width="900">
</p>

## Install

**One line** (Linux and macOS; checks the archive against the release's `SHA256SUMS` and installs to `~/.local/bin`):

```bash
curl -fsSL https://raw.githubusercontent.com/buzzzr/buzzr/main/install.sh | sh
```

Options go to the shell that runs the script — `| sh -s -- --version v0.1.1 --dir /usr/local/bin` — and `--help` lists them.
Running it again replaces the binary, so it is also the upgrade.

**Homebrew** (tested on macOS; the formula also lists the Linux binaries, untested):

```bash
brew install buzzzr/buzzr/buzzr
```

Upgrade with `brew update && brew upgrade buzzr`.

**A prebuilt binary:** download the archive for your system from the [latest release](https://github.com/buzzzr/buzzr/releases/latest)
(macOS and Linux, arm64 and x86_64; the Linux ones are static), check it against `SHA256SUMS`, and put `buzzr` somewhere on your `PATH`.

**With cargo** (Rust 1.88 or newer; not on crates.io yet):

```bash
cargo install --git https://github.com/buzzzr/buzzr
```

**From source:**

```bash
git clone https://github.com/buzzzr/buzzr
cd buzzr
./build.sh release        # optimised binary at dist/buzzr
```

`build.sh` uses `cargo` if it is installed and otherwise a Rust container, which produces a static musl binary. See
[Build](#build) for details.

## Quick start

```bash
buzzr
```

The first time, buzzr asks for your relay address and private key, and how to keep the key. After that it opens straight into your channels.
Press **F1** for the keys and **F2** for the commands. Press **Tab** to move between the channel list, the messages and the input; the arrow keys get between the messages and the input too — `Up` from an empty input lands on the newest message, `Down` on the newest message goes back to the input.

The default colour scheme is `buzz-dark`; set `BUZZR_THEME=buzz-light` for the light one:

<p align="center">
  <img src="docs/screenshots/buzz-light.png" alt="The same channel in the buzz-light colour scheme" width="900">
</p>

## First run

If buzzr has no relay or key yet, it asks:

1. **The relay address**, for example `wss://chat.example.com` (a bare host gets `wss://`). Saved in `~/.config/buzzr/config.json`.
2. **Your private key** (hex or `nsec1...`; hidden as you type). buzzr shows the public key it belongs to so you can check it is yours.
3. **How to keep the key:**
   - **Protected by a password** (the default): `key.enc`, sealed with scrypt (512 MiB, a couple of seconds per guess) and
     AES-256-GCM. You type the password each time buzzr starts.
   - **Without a password**: `key`, a plain file readable only by you (mode 600, directory 700). buzzr starts straight away, but anyone
     who can read that file or a backup of it can act as you. buzzr refuses the file if other users can read it.
   - **Not stored**: buzzr asks for the key every time.

Everything is in `~/.config/buzzr` (`$XDG_CONFIG_HOME/buzzr`, or `$BUZZR_CONFIG_DIR`). `buzzr --setup` asks again and replaces the old
choice; `buzzr --forget-key` deletes the saved key. `--relay URL` or `BUZZ_RELAY_URL`, and `BUZZ_PRIVATE_KEY`, override the saved
values and skip the questions (for scripts and containers; the key is never accepted on the command line).

## Details

The full behaviour, for when the feature list above is not enough.

- **Channels and DMs** on the left (a DM is labelled with the other person, `@name`), with unread counts. An open channel is
  marked `#`, a private (restricted) one `*` and a DM `@`; all three are ASCII, so no terminal font can fail to draw them. A
  channel with unseen messages shows their count (`(2)`); one whose only unseen news is inside a thread shows `●` instead, since
  a reply is not a timeline row and a count for it could never be cleared by opening the channel; a channel with both shows
  `(2) *`. The selected channel has a full-width grey tint and a bar on
  the left, like the selected message; the bar is the accent colour while the list has focus and grey when it does not, so the accent
  marks where the focus is. The bar sits on the channel on screen; while the list has focus it follows the cursor, and leaving the
  list without opening a channel puts it back. The keyboard starts on the channel list, so Up/Down and Enter work as soon as buzzr opens.
  `/dm name` opens the DM you already have with someone, or starts a new one; a name from a channel or an npub both work.
- **Unread that survives a restart:** the last-read position per channel and per thread lives in
  `~/.config/buzzr/read-state.json`, so it is there again next time (each channel's count is asked of the relay on startup)
  and a `new messages` line marks where the unseen ones begin. Opening a channel marks it read; `/read` does it again and
  `/read all` clears every channel and its threads. `u` on a message goes the other way: it forces *that* message unread on
  this device, so the badge and the `new messages` line come back above it. Unlike the position, a mark is not moved on by
  reading: it works on a message of your own too, and only `/read` clears it. The channel list tells the two kinds of unread
  apart: `(2)` is unseen top-level messages, `●` is unseen replies inside a thread, and `(2) *` is both. The `●` needs the
  channel's messages loaded, because the news it stands for is inside a thread — the count a relay answers on startup is about
  the timeline only.
- **Archive a channel:** `x` on the list keeps a channel out of it, without leaving it — leaving changes your membership on the
  relay and loses your place. An archived channel is still loaded and still counts unread, but it is left out of the status line's
  unread total, so no badge points at a row you cannot see. Ctrl+F (or `/search`) still matches it; its row says `archived`, and
  Enter brings it back and opens it. The set lives in `~/.config/buzzr/archived.json` and survives a restart.
- **A channel shows only its top-level messages**, as the Buzz app does, each with `💬 3 replies · last Thu 13:33` when it has a
  thread. The relay supplies the counts and says whether older messages exist.
- **Threads:** Enter or a click opens one, Esc returns to the same place. The thread's root sits at the top as one header line —
  its number, time and author, then the first line of what it says, cut with `…` when there is more — and the replies are indented
  two columns under it, so the pane reads as a thread rather than a flat list. Opening a thread reads its replies, so a thread with
  replies since you last looked at it shows `● 2 new` after its reply count. `r` replies in the thread of the selected message.
- **Live:** new messages, edits, deletions, reactions, reply counts and typing indicators arrive over a WebSocket as they happen
  (`Bob is typing…`), and the header shows the connection state. If the connection drops it reconnects and reloads.
- **Reads like a chat:** Markdown (bold, italic, lists, code, quotes, headings), bare URLs become links, reactions are shown,
  edited messages say so. A blank line separates messages and the selected one has a grey tint and a bar on the left.
- **Keeps your place:** new messages never move the view unless you were at the bottom; older messages load above without a jump
  (Up at the top, Ctrl+O or `/more`). A `↓ more below` marker sits in the bottom corner of the message pane while the newest
  message is below the view, so it is clear the list goes on; it goes away as soon as the newest message is on screen.
- **Links:** a click on a link copies its URL to your clipboard (terminal clipboard sequence, see below). `c` copies the selected
  message.
- **Writing:** a multi-line box that grows; Enter sends. Alt+Enter and Ctrl+J add a line in every terminal, Shift+Enter in the ones
  that report it (`kitty`, `foot`, `WezTerm`, `Ghostty`, `iTerm2`) — the footer names the key that works where you are;
  bracketed paste; `@name` mentions members. Word and line editing keys (Ctrl+W or Alt+Backspace deletes the word before the
  cursor).
- **Edit and delete your own messages:** select one and press `e` (it loads into the input; Enter saves, Esc cancels) or `d` (asks
  `y` to confirm). Others see edits and deletions live. `/edit N` and `/delete N` do the same by number.
- **React to a message:** select one and press `a` — a picker opens over the message: choose an emoji from the list, or type a name
  to search it (or an emoji the list does not have). Enter sends it as a NIP-25 `kind:7` and Esc closes. The picker opens on the
  reaction you already have there, and its first row takes it back; everyone sees the reaction, and its removal, live.
- **Search everything:** Ctrl+F (or `/search words`) searches channel names (instantly), people you have seen (from their profiles) and
  every message you can read (full text, from the relay); Enter opens the result — a message or channel in its channel or thread, a
  person in the DM with them (starting one if there is none).
- **New channels:** `/new [private] name [description]` creates one and opens it.
- **Join and leave channels:** Ctrl+F finds a channel by name, and its row says what it is for, how many people are in it and whether
  you are one of them (the count and the membership come from NIP-29 `kind:39002`). Enter opens it; a channel you are not in opens
  read-only, with a `[ Join ]` button in place of the composer — Enter on the button asks the relay to join you (`kind:9021`), and its
  answer turns the button back into the composer. A private channel is not offered — a self-join needs an open channel, so it is
  refused ("needs an invitation"). `/join NAME` (a name, `#name` or a channel id) joins or opens one directly; `/leave` asks first
  (`y` confirms) and sends `kind:9022`, then drops the channel from the list. The list and the membership are read again after a join
  or a leave, so they follow what the relay accepted.
- **People:** `/whois name` shows a short card for someone — their name, npub and about line, filled in as the relay answers. `/dm name`
  and `/whois name` take a display name from a channel, or a key (`npub1…`, `nostr:` URI or hex); an ambiguous or unknown name is
  reported rather than guessed.
- **Invite people:** `/invite name` adds someone — a display name from a channel, or an npub — to the channel on screen, or to
  the group DM you are in; they join as a plain member (no role is asked for). A message you send that `@mentions` someone who
  is not in the channel asks before it goes out: `y` invites them and sends, `n` sends it without inviting, Esc puts the message
  back in the input. A name that is already a member is treated as that member, so a plain mention never asks; a DM is a
  conversation rather than a room to join, so it never asks either; and nothing is offered before the channel's member list has
  loaded. Only names the profiles have given us can be recognised this way.
- **Your typing shows:** others see `<your name> is typing…` while you write (once every three seconds; not for `/commands` or
  edits). `--no-typing` turns it off.
- **Help on screen:** F1 lists the keys, F2 the commands. Esc closes the command palette first, then a panel, then cancels a reply,
  then leaves a thread; after that it steps back a pane at a time — from the input to the message list, and from the message list
  to the channel list.
- **Plugins:** a plugin is a separate process (in any language) that adds `/commands`, a status item, header badges, per-channel and
  per-message marks, a pane under the channel list, and panels; buzzr hot-reloads it when its files change. A plugin is one folder
  with an executable entry file and **no manifest**: the folder name is its id, and it declares its own name, permissions and
  heartbeat in its code, when it is ready. A heartbeat (`tickMs`) refreshes something time-based — that is how the clock example
  keeps the time. Put the folder in `~/.config/buzzr/plugins` (or point `--plugins-dir`/`BUZZR_PLUGINS_DIR` elsewhere). `/plugins`
  lists what loaded, and `buzzr plugins list|enable|disable` works from the shell. `--no-plugins` (or `BUZZR_SAFE_MODE=1`) turns the
  whole system off. See [PLUGIN.md](PLUGIN.md); tiny examples are in `examples/plugins/hello` (the raw protocol, in shell) and
  `examples/plugins/clock` (TypeScript, built on the SDK in `sdk/typescript/`).
- **Upgrading:** `/update` in the input says how to upgrade this copy, worked out from where the binary lives — `brew update &&
  brew upgrade buzzr` for a Homebrew install, `cargo install --force --git https://github.com/buzzzr/buzzr` for a
  `cargo install`, or the release page otherwise.
  Nothing is downloaded or changed. Once a day buzzr also asks GitHub's release feed whether a newer release exists; when there is
  one the status line says so (`buzzr 0.2.0 available · /update`) and the same upgrade line is printed after you quit.
  `BUZZR_NO_UPDATE_CHECK=1` turns the check off.

### Keys

| | |
|---|---|
| Tab / Shift+Tab | switch between channels, messages and the input |
| Up / Down | select a channel or message (Up at the top of a channel loads older messages) |
| Up / Down, at the edges | nothing typed in the input, Up goes to the messages on the newest one; on the newest message, Down goes back to the input |
| Enter | open the channel / open the thread (which reads it) / send |
| ← / → | walk a thread: → opens the thread of the selected message, ← goes to the thread's root and then out of it (→ on a message with no replies does nothing) |
| y, n, Esc (invite prompt) | invite the mentioned person and send, send without inviting them, keep the message in the input |
| x | archive the channel under the bar (it leaves the list; Ctrl+F or `/search` finds it again) |
| Enter, in a channel you are not in | join it (the composer is a `[ Join ]` button) |
| r, c, u, e, d | reply in the thread, copy, mark unread, edit, delete the selected message (e and d: yours only) |
| a | react to the selected message: pick an emoji, or type to search one (Enter sends, Esc closes) |
| / | start a `/command`: a palette lists the commands above the input and narrows as you type (Enter runs the one on the cursor, Tab completes it, Esc closes the list and keeps what was typed) |
| Ctrl+F | search channels, people and messages |
| Esc | close the palette or a panel, cancel a reply, leave a thread, then step back to the channel list |
| F1, F2 | keys, help |
| Ctrl+R, Ctrl+O | reload, load older |
| Ctrl+Q, Ctrl+C | quit |

Commands in the input (N is the number shown as `#N`): `/r N text`, `/t N`, `/c`, `/copy N`, `/edit N`, `/delete N`, `/search text`, `/whois NAME`, `/dm NAME`, `/invite NAME`, `/new [private] name [about]`, `/join NAME`, `/leave`, `/plugins`, `/update`, `/read [all]`, `/archive`, `/more`, `/refresh`, `/help`, `/q`. Typing `/` lists them above the input and narrows the list as you type: Enter runs the command on the cursor line, Tab fills its name in for one that takes an argument. Plugin commands appear in the list too.

## Clipboard

`c` and link clicks use the terminal's clipboard escape sequence (OSC 52), which works over SSH when the terminal allows it:
iTerm2 ("Applications in terminal may access clipboard"), kitty, WezTerm, Ghostty, Windows Terminal. In tmux add
`set -g set-clipboard on`. macOS Terminal.app ignores it. Run buzzr on your own machine (not over SSH) and links are opened in the
browser as well.

## Options

`--relay URL` (default `$BUZZ_RELAY_URL`, else the saved relay), `--setup`, `--forget-key`, `--read-only` (never send), `--no-typing`
(do not announce typing), `--no-mouse` (leave the mouse to the terminal), `--no-plugins` (start without plugins), `--plugins-dir DIR`
(where plugins live), `--selftest` (below). Environment: `BUZZR_TZ` for the display
time zone (default: the computer's), `BUZZR_THEME` (`buzz-dark`, `buzz-light`, `high-contrast`, `dark`, `light`), `BUZZR_CONFIG_DIR`, `BUZZR_PLUGINS_DIR`,
`BUZZR_SAFE_MODE=1` (same as `--no-plugins`), `BUZZR_NO_UPDATE_CHECK=1` (skip the once-a-day release check), `BUZZR_SCRYPT_LOG_N` (cost of a new
key file, 10-20, default 19).

## Build

You need Rust 1.88 or newer (or Docker). `build.sh` uses `cargo` if it is installed, otherwise a Rust container; see the top of the
script (it can also build on another machine over ssh).

```bash
./build.sh test        # unit tests
./build.sh release     # optimised binary at dist/buzzr
cargo install --path . # or install with cargo
```
The Buzz crates (`buzz-core`, `buzz-sdk`, `buzz-ws-client`) are pulled from the Buzz repository at the commit pinned in `Cargo.toml`;
the relay you connect to should be recent enough to speak the same protocol.

## Checking it against a relay

`buzzr --selftest [--listen 12] [--probe-typing CHANNEL]` is a read-only check for a machine with an identity but no terminal (for
example a container; set `BUZZ_RELAY_URL` and `BUZZ_PRIVATE_KEY`): it authenticates, lists channels, loads a channel window and a thread, and listens on the live connection.
`--probe-search TEXT` runs a full-text search. `--probe-write` is the one check that writes: it creates a **private throwaway channel** (only the
testing identity can see it), posts, edits and deletes a message in it, then deletes the channel. `--probe-typing` publishes one typing indicator (ephemeral, never stored) through the same signed HTTP path chat messages use and
expects it to come back live.

## Status and limitations

buzzr is young (0.1) and under active development. It works against real Buzz relays, but expect rough edges. Not done yet:
adding reactions, attachments and images (shown as links only).

If something is missing that you need, please [open an issue](https://github.com/buzzzr/buzzr/issues).

## Contributing

Bug reports, ideas and pull requests are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) first, then [ARCHITECTURE.md](ARCHITECTURE.md) for
how the code fits together. A change is done when `./build.sh test` is green and the build has no warnings. To report a security issue,
please do not open a public issue; see [SECURITY.md](SECURITY.md).

## Acknowledgements

buzzr builds on the [Buzz](https://github.com/block/buzz) crates (`buzz-core`, `buzz-sdk`, `buzz-ws-client`), [ratatui](https://ratatui.rs) and
[Nostr](https://nostr.com). It is an independent community project and is not affiliated with or endorsed by the Buzz team.

## License

MIT, see [LICENSE](LICENSE). Contributions: see [CONTRIBUTING.md](CONTRIBUTING.md).
