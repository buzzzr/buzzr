# Contributing to buzzr

Thanks for helping. Read `AGENTS.md` (principles, pitfalls) and `ARCHITECTURE.md` (how it fits together) first; they apply to people
as much as to coding agents.

- **Build and test:** `./build.sh test` (uses `cargo`, or Docker if there is no cargo). A change is done when all tests pass and
  the build has no warnings. Match the surrounding style (long lines, about 140 columns; there is no rustfmt config, so do not run `cargo fmt` over the whole tree), comments say why.
- **Tests come with behaviour:** UI behaviour in `src/app/tests.rs` (synthetic events, no network or terminal), drawing in `ui.rs`
  tests, protocol builders in `relay.rs`, first-run questions in `setup.rs` (scripted answers).
- **Keys and secrets:** never log, print or put a private key on a command line, and keep it in `Zeroizing`. Do not commit real
  keys or relay addresses, not even in tests (use `chat.example.com` and obviously fake keys).
- **Message text is hostile:** anything from the network that reaches the screen goes through the Markdown renderer, which strips
  control characters.
- **Changing a key, command or overlay:** update the on-screen help (`KEYS`, `HELP`, footer in `ui.rs`) and the README in the same change.
- **Commits and pull requests:** small, imperative subject line; say what changed and why. Check against a real relay when you touch
  protocol code (`buzzr --selftest`, see `AGENTS.md`) and say what you ran.
- **Security issues:** please do not open a public issue; contact the maintainers privately.

## Releasing

Releases are cut by pushing a tag; `.github/workflows/release.yml` does the rest.

1. Set `version` in `Cargo.toml` (and refresh `Cargo.lock`), merge it to `main`.
2. `git tag v0.2.0 && git push origin v0.2.0` (the tag must be `v` + the Cargo version, or the workflow stops). A suffix such
   as `v0.2.0-rc1` publishes a pre-release.
3. The workflow builds static Linux (musl, x86_64 and aarch64) and macOS (Apple silicon and Intel) binaries, checks that
   `buzzr --version` prints the version and that the Linux ones are statically linked, and publishes
   `buzzr-<version>-<target>.tar.gz` for each plus `SHA256SUMS` on a GitHub release with generated notes.

A pull request that touches the workflow builds and tests the binaries without publishing anything.

By contributing you agree that your work is licensed under the MIT License (see `LICENSE`).
