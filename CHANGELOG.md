# Changelog

All notable changes to buzzr are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versions follow [Semantic Versioning](https://semver.org/) (before 1.0, a minor bump may change behaviour).

## [Unreleased]

## [0.2.0-rc.4] - 2026-11-13

### Security

- A local agent answers **only its owner**: a mention from anyone else is ignored. Earlier thread messages are marked as untrusted in the prompt.
- Every event is checked for a valid id and signature when it comes in; a forged or altered event is dropped.

### Added (unreleased)

- **Edit a local agent**: `agents` has an `edit` row per agent to change its name and instructions. A new name is republished to the relay (profile and listing) before the record changes. Plugins get `update_agent`/`updated`, and agents in the list carry `instructions`.

### Changed

- A relay **notice** is shown as a message (`relay: …`) and no longer marks the connection as offline; reconnect delays have jitter.
- Internal: `app.rs` split by feature, tag lookups go through `Ev`, one clock, a `request` helper for commands, agent publish/delete live in
  `agents.rs` with a typed `agents::Error`, and the state files are written in order off the loop. `docs/DESIGN_RULES.md` lists the antipatterns to avoid.

### Added

- An agent can be **deleted** from the `agents` list: Enter on the agent opens its menu (edit, delete, back), and `delete…` there asks to confirm, then takes the agent off the relay — its own
  profile (signed with its own key) and your listing of it, both by NIP-09 — and deletes its key and record from `agents.json`, leaving the
  folder it worked in alone. The relay is asked first, so a relay that refuses it deletes nothing, and the agent stays listed and running.
  Plugins reach it through the new `delete_agent` binding (needs `agents:write`), answered with `deleted`.
- A local agent mentioned in a **thread** is answered with the thread in hand: the turn's prompt carries the thread's earlier messages — the
  root and the replies that came before the mention, oldest first, the newest twelve, each cut to 4000 characters — so the agent reads the
  conversation it is joining instead of one message out of it, the same window the Buzz app gives its agents.
- The **command palette** takes a whole line, not just a command name: type `dm @Alice` and Enter runs `dm` with `@Alice` in one step,
  with no separate argument prompt. The list still narrows to the command at the head of the line, a bare name is still a pick that
  asks for its argument, and a leading `@` on a name is how somebody is addressed rather than part of the name (`join`, `new` and the
  other non-person arguments are unchanged).

### Fixed

- A program that buzzr starts right after it was written to disk is no longer refused with `Text file busy`: the start is
  tried again for a moment. This is what a plugin reloaded straight after an edit ran into, and what a harness being
  updated underneath `agents create` can run into.

## [0.2.0-rc.3] - 2026-11-12

### Added

- Ctrl+F finds a channel by name across every channel the relay lists, joined or not, and its row says what the channel is for, how
  many people are in it and whether you are one of them (the membership and the counts come from NIP-29 `kind:39002`). A channel you
  are not in opens read-only: in place of the composer there is a `[ Join ]` button, reached with Tab the way the input is, and Enter
  on it asks the relay to join you (`kind:9021`) — its answer turns the button back into the composer. A private channel is not
  offered, because a self-join needs an open one. `join NAME` (a name, `#name` or a channel id) joins or opens one directly, and
  `leave` asks first (`y` confirms) before it sends `kind:9022` and drops the channel from the list. The list and the membership are
  read again after a join or a leave.
- A plugin can keep a **pane** under the channel list: a small block that is always on screen and takes no focus, key or click.
  `examples/plugins/clock` shows the time in large digits there and redraws it from an opt-in heartbeat.
- A plugin is now declared **entirely in its own code, with no manifest**: the folder name is the plugin's id, buzzr runs the first
  executable entry file it finds in the folder through that file's own `#!` line (so the language is the author's choice, and a
  JavaScript or TypeScript runtime needs nothing added to buzzr), and the plugin sends its display name, its permissions and its
  heartbeat (`tickMs`) in its `ready` message. The contract version is now `apiVersion` 2. A folder that will not load — no entry
  file, no executable bit, an id that is not safe — is listed with the reason instead of failing quietly.
- **One plugin framework for two kinds of plugin.** A plugin is now either **runtime-loaded** (the separate process above, any
  language, hot-reloaded) or **compiled-in** (a Rust type in buzzr's own tree, or in a crate of its own that depends on the `buzzr`
  library, registered with `register_plugin!` and driven in-process). Both speak the same `PluginInput`/`PluginOutput` contract and
  pass the same permission and screen gates, so the transport never changes what a plugin may do. A new **prompt** binding lets either
  kind ask buzzr to put a question in the composer — one line, or a block of text — and hand the answer back, and two new bindings and
  permissions (`agents:read`, `agents:write`) let either kind list the local agents, ask buzzr to start a harness and read what it
  offers, and ask buzzr to make and publish one.
- **`/agents` is now the first compiled-in plugin** (`src/plugins/agents/`), written against that same contract: it lists the
  local agents as a panel, asks for a name and then for the instructions through prompts, asks the host to start the harness and read
  what it offers, and asks the host to make the agent. The `/agents` overlay and its bespoke state are gone from the core — the
  feature is a plugin.
- A **typed SDK for TypeScript** (`sdk/typescript/buzzr.ts`): one dependency-free file that turns the protocol into a single
  `definePlugin({ name, permissions, tickMs, ready, onTick, onCommand, … })` call, with `status`, `pane`, `panel`, `flash`, `draft`,
  `send` and `log` on the handle it gives back. `examples/plugins/clock` is written with it.
- The **channel list tells the two kinds of unread apart**: a channel with unseen top-level messages shows their count (`(2)`),
  one whose only unseen news is inside a thread shows `●`, and a channel with both shows `(2) ●`. A thread reply still does not
  add to the counted unread — a count for it could never be cleared by opening the channel, because a reply is not a timeline row.
- **A draft belongs to the place it was typed**: switching channel, or going into or out of a thread, empties the box instead of
  carrying a half-written message along, and the text is back when you return to that channel or thread. Sending clears that
  place's draft. They are kept in `drafts.json` next to the read positions, so they survive a restart.
- **Agents.** `/agents` lists the agents this copy made; `/agents create` — or the “new agent…” row in that list — turns the composer into a
  flow that asks for a name, then for what the agent should do, then starts the harness and reads what it offers over ACP (its models, and a
  model's thinking levels) so a model and a level can be picked, and makes it an agent: a fresh key of its own, a folder of its own to work in,
  and a record in `agents.json` (mode 600, next to the read positions, since it holds the agent's key). It is published to the relay, the way
  the Buzz app does it: the agent's own kind-0 profile, signed with its own key and carrying this identity's NIP-OA attestation of it, and this
  identity's `kind:30177` listing of it, so the owner's other Buzz clients show it. The harness is asked what it offers, and the relay asked to
  take the agent, before anything is written down: a harness that will not start or will not answer, or an agent the relay refuses, stops the
  flow and makes nothing — no key, no folder, no record — and the flow says why. The harness it is made for is a local Claude Code driven over
  ACP (`claude-agent-acp`); a second one (codex, say) is a new entry in the harness table (`src/harness/mod.rs`) and nothing else. Read-only
  mode refuses to make one.
- **A local agent now answers the mentions that name it.** An agent was an identity and a folder with nothing running it; a **runner**
  (`src/runner.rs`, started with the app) gives every agent in `agents.json` a watch of its own and answers its mentions: each message that
  carries the agent in a `p` tag is one turn of its harness, started in the agent's own folder, with what the harness streams back published to
  the channel the mention came from — as the agent's own message, signed with the agent's own key and carrying its owner's attestation, the way
  its profile is. One session is kept between mentions, so the agent stays in the conversation it is in, and the status line says that it
  replied, or why it could not (a harness that is not installed, a relay that refuses the answer, a turn that streams back nothing). A mention
  names an agent **in a channel**, so the agent has to be in that channel to be mentioned at all — `/invite <npub>`, the npub `/agents` shows,
  puts it there — and only mentions that arrive while the runner is watching are answered: an older backlog is not. A harness starting a turn
  is buzzr's job, never a plugin's, and a run that may not send (`--read-only`, `BUZZR_SAFE_MODE=1`) starts no runner.
- **A name is looked up in the relay's directory when no loaded channel has shown it.** `/invite`, `/dm` and `/whois` still take a
  name they have already met, and an npub, but a name the local table does not hold is no longer answered with “nobody here is
  called …”: it is asked of the relay (a NIP-50 `search` over `kind:0` profiles) and the command goes on with the one person that
  fits. When several profiles fit, they are listed by name and short key instead of one being guessed. The matches are checked here
  as well as asked for at the relay, so a directory that ignores `search` and answers everyone cannot make the command act on the
  wrong key. The name that answers is remembered, so the next command resolves it without a round-trip; a directory that cannot be
  reached, or that knows nobody by that name, still says so.

### Changed

- **Commands moved to a `Ctrl+K` command palette.** `Ctrl+K` opens a modal palette with a query of its own: type to narrow the
  commands to the ones whose name starts with what you typed, the arrows move the highlight, and Enter runs the one on the cursor. A
  command that takes an argument asks for it in a step of its own (Esc backs out of the argument step, a second Esc closes the
  palette). The palette never touches the composer or its draft, and a `/` typed into the message box is now ordinary text — the
  `/command` form is gone, for the commands plugins add too.
- `←` / `→` walk a thread from the message list: with a message row selected, `→` opens its thread (a message with no replies has
  nothing under it, so there the key does nothing) and `←` leaves a thread for the timeline it came from, putting the cursor back on
  the message the thread hangs off. Esc still backs out the same way, wherever the cursor is.
- The thread pane reads as a thread: its root is pinned to the top as one header line — the message number, the time, the author
  and the first line of what was said, cut with `…` when there is more — and it stays on screen while the replies, indented two
  columns, scroll under it. The root keeps message number 1, so the numbers `r N` and `t N` take are the ones on screen, and
  links in an indented reply are still where they look.

## [0.2.0-rc.2] - 2026-11-11

### Added

- `x` on the channel list archives the channel under the cursor (`/archive` archives the channel you are in): it drops out of the
  list without leaving it, so the relay membership and the read position are untouched. Ctrl+F or `/search` still matches it, its
  row says `archived`, and Enter brings it back and opens it. The archive set lives in `archived.json` next to the read positions
  and survives a restart; an archived channel keeps its unread count but is left out of the status line's total.
- Typing `/` in the input shows a slash-command palette above it: the list narrows to the commands whose name starts with
  what has been typed, Enter runs the command on the cursor line, and Tab fills its name in for one that takes an argument.
  The commands loaded plugins registered appear in the list too.
- `a` opens a reaction picker on the selected message: choose an emoji from the list, or type a name to search it (or an emoji the
  list does not have). Enter sends it as a NIP-25 `kind:7` and Esc closes. The picker opens on the reaction you already have there,
  and its first row takes it back; changing an emoji undoes the one it replaces first, so a message keeps one reaction from you.
- `install.sh` installs buzzr in one line: `curl -fsSL https://raw.githubusercontent.com/buzzzr/buzzr/main/install.sh | sh`
  downloads the archive for the machine it runs on, checks it against the release's `SHA256SUMS` before unpacking anything, and
  puts the binary in `~/.local/bin`. `--version` pins a release (a prerelease too, which `/releases/latest` skips) and `--dir`
  moves the destination; running it again replaces the binary, so it is also the upgrade.
- `/invite NAME` adds someone — a display name from a channel, or an npub — to the channel on screen (or the group DM you are in)
  as a plain member. A message you send that `@mentions` someone who is not in the channel is held back and the status line asks:
  `y` invites them and sends, `n` sends without inviting, Esc keeps the message in the input. Mentioning a member never asks, and
  a DM never asks; nothing is offered before the channel's member list has loaded, and only names the profiles have given us are
  recognised this way.

### Changed

- The arrow keys move between the messages and the input: `Up` in an empty composer leaves it for the message list and lands on
  the newest message with the bottom scrolled into view, and `Down` on the newest message goes back to the input. The channel
  cursor is no longer dragged along, and with text in the input `Up`/`Down` still move inside what is being written.

### Fixed

- A `… is typing` indicator was shown for the whole channel it arrived in: typing sent inside a thread was drawn in the channel
  timeline, and typing sent in the channel was drawn inside every thread. An indicator is now shown only in the channel or the
  thread it was sent in.
- A DM row opened with `✉ ` (U+2709), an envelope that a terminal font without it draws as a box. A DM is now marked `@`, ASCII
  like the `#` and `*` of an open and a private channel, and no line that names a DM carries the glyph any more.

## [0.2.0-rc.1] - 2026-11-09

### Added

- A `↓ more below` marker sits in the bottom corner of the message pane while the newest message is below the view, and goes
  away as soon as the newest message is on screen.

### Changed

- `u` forces *that* message unread instead of marking the channel unread from it: it works on your own message and on the
  newest one, the `new messages` line is drawn above the marked message, and reading on does not clear it — only `/read`
  (or `/read all`) does. The read position is left alone, and a mark never hides a higher unread count.

### Fixed

- A channel's unread badge counted thread replies, which the channel timeline never shows, so the count came back after every
  restart even once the channel had been read. A badge now counts top-level rows only; a thread keeps its own unread mark.

## [0.2.0-rc.0] - 2026-11-09

### Added

- Repository health files: issue templates, `SECURITY.md`, `CODE_OF_CONDUCT.md`, this changelog, Dependabot, and a scheduled
  `cargo audit`.
- Channels are listed in sections (Solo, Channels, DMs), and an open channel is marked `#` and a private one `*`.
- Start a DM with `/dm name`, find people with Ctrl+F, and see a short profile card with `/whois name`.
- `NO_COLOR`, `TERM=dumb` and `BUZZR_THEME=high-contrast` turn colour off or up.
- An end-to-end suite (`./build.sh e2e`) drives the real binary in a pty against
  [`buzz-fake-relay`](https://github.com/buzzzr/buzz-fake-relay) and checks the screen it draws.
- `u` marks the selected message unread: the channel's badge, the header count and the `new messages` divider come back, and
  the position survives a restart.
- A thread whose replies are newer than your last visit is marked unread on its message, and opening it clears the mark.

### Changed

- Startup keeps the keyboard on the channel list (the first channel is still opened), and Escape steps back one pane at a time:
  input, then messages, then channels.
- The selected channel row is tinted across the whole pane, and the accent bar marks which pane has focus, as in the message list.
- Alt+Backspace deletes the word before the cursor.
- The input footer names the new-line key the terminal can actually report: Shift+Enter where the terminal speaks the kitty
  keyboard protocol, Alt+Enter everywhere else.

### Fixed

- `/update` named a `cargo install` command that cannot work; it names the repository install instead.
- A removed reaction now disappears from the message: a `kind:5` that names a reaction event undoes that reaction.

### Documentation

- A demo recording at the top of the README.

## [0.1.1] - 2026-11-08

### Added

- `buzz-dark` (the default) and `buzz-light` colour schemes matching the Buzz desktop app.

### Documentation

- A pull request template and the requirements for opening a pull request.

## [0.1.0] - 2026-11-08

First public version.

### Added

- A terminal client for Buzz that talks to the relay over WebSocket: channels, threads, DMs, reactions, live updates and typing
  indicators.
- Unread counts that survive a restart, and `/read`.
- A password-protected key file by default (scrypt and AES-256-GCM), with plain-file and no-storage options.
- An out-of-process plugin system, off with `--no-plugins` or `BUZZR_SAFE_MODE=1` (see `PLUGIN.md`).
- A notice in the UI when a newer version is published, with the upgrade command.
- Release builds for Linux (musl, x86_64 and aarch64) and macOS (Apple silicon and Intel), published with `SHA256SUMS`.

[Unreleased]: https://github.com/buzzzr/buzzr/compare/v0.2.0-rc.4...HEAD
[0.2.0-rc.4]: https://github.com/buzzzr/buzzr/compare/v0.2.0-rc.3...v0.2.0-rc.4
[0.2.0-rc.3]: https://github.com/buzzzr/buzzr/compare/v0.2.0-rc.2...v0.2.0-rc.3
[0.2.0-rc.2]: https://github.com/buzzzr/buzzr/compare/v0.2.0-rc.1...v0.2.0-rc.2
[0.2.0-rc.1]: https://github.com/buzzzr/buzzr/compare/v0.2.0-rc.0...v0.2.0-rc.1
[0.2.0-rc.0]: https://github.com/buzzzr/buzzr/compare/v0.1.1...v0.2.0-rc.0
[0.1.1]: https://github.com/buzzzr/buzzr/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/buzzzr/buzzr/releases/tag/v0.1.0
