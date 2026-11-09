# Changelog

All notable changes to buzzr are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versions follow [Semantic Versioning](https://semver.org/) (before 1.0, a minor bump may change behaviour).

## [Unreleased]

### Added

- Repository health files: issue templates, `SECURITY.md`, `CODE_OF_CONDUCT.md`, this changelog, Dependabot, and a scheduled
  `cargo audit`.
- Channels are listed in sections (Solo, Channels, DMs), and an open channel is marked `#` and a private one `*`.
- Start a DM with `/dm name`, find people with Ctrl+F, and see a short profile card with `/whois name`.
- `NO_COLOR`, `TERM=dumb` and `BUZZR_THEME=high-contrast` turn colour off or up.
- An end-to-end suite (`./build.sh e2e`) drives the real binary in a pty against
  [`buzz-fake-relay`](https://github.com/buzzzr/buzz-fake-relay) and checks the screen it draws.

### Changed

- Startup keeps the keyboard on the channel list (the first channel is still opened), and Escape steps back one pane at a time:
  input, then messages, then channels.
- The selected channel row is tinted across the whole pane, and the accent bar marks which pane has focus, as in the message list.
- Alt+Backspace deletes the word before the cursor.

### Fixed

- `/update` named a `cargo install` command that cannot work; it names the repository install instead.

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

[Unreleased]: https://github.com/buzzzr/buzzr/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/buzzzr/buzzr/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/buzzzr/buzzr/releases/tag/v0.1.0
