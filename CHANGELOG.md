# Changelog

All notable changes to buzzr are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versions follow [Semantic Versioning](https://semver.org/) (before 1.0, a minor bump may change behaviour).

## [Unreleased]

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

### Fixed

- A `… is typing` indicator was shown for the whole channel it arrived in: typing sent inside a thread was drawn in the channel
  timeline, and typing sent in the channel was drawn inside every thread. An indicator is now shown only in the channel or the
  thread it was sent in.

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

[Unreleased]: https://github.com/buzzzr/buzzr/compare/v0.2.0-rc.1...HEAD
[0.2.0-rc.1]: https://github.com/buzzzr/buzzr/compare/v0.2.0-rc.0...v0.2.0-rc.1
[0.2.0-rc.0]: https://github.com/buzzzr/buzzr/compare/v0.1.1...v0.2.0-rc.0
[0.1.1]: https://github.com/buzzzr/buzzr/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/buzzzr/buzzr/releases/tag/v0.1.0
