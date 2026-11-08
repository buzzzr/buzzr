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
- **Commits and pull requests:** small, imperative subject line; say what changed and why. See "Opening a pull request" below.
- **Security issues:** please do not open a public issue; contact the maintainers privately.

## Opening a pull request

Open pull requests against `main` of `buzzzr/buzzr`, from a branch in your fork. One change per pull request: a small, focused diff gets
reviewed faster than a large one. For anything bigger than a bug fix, open an issue first so the approach can be agreed before you
write it. The pull request template (`.github/pull_request_template.md`) has three sections to fill in (what and why, screenshots, how it was tested); the requirements behind it are:

1. **Say what and why**, and link the issue (`Closes #123`).
2. **Green build:** `./build.sh test` passes and there are no new warnings. CI runs the same.
3. **Tests come with the change** (see the list above). A bug fix includes a test that fails without the fix.
4. **Docs move with the code:** a new or changed key, command or overlay updates the on-screen help and `README.md`.
5. **Protocol changes** (`relay.rs`, `live.rs`, `model.rs`): run `buzzr --selftest` against a relay you may test with (`AGENTS.md`) and
   say what you ran. Never put a real relay address or key in the pull request.
6. **User-visible changes need screenshots**, made against the fake relay (below). That means anything that draws differently or reacts
   to keys or the mouse differently: new or changed views, colours, layout, key bindings, status text, overlays.
   - Show **before and after** for the same scenario, and say which one you drove ("general, open the thread on #3, press `r`").
   - Touched colours: show the dark and the light theme. Touched layout: also show a narrow terminal (about 80x24).
   - A screenshot, or a terminal capture pasted into the description in a ```` ```text ```` block, both work. Attach images to the
     pull request description (drag them in); do not commit them to the repository. Text captures are easier to review and diff.
   - Not user-visible? Write `n/a: not user-visible` and say why.
7. **Keep the diff to the change:** no unrelated reformatting, no whole-tree `cargo fmt`.

### Screenshots against the fake relay

[`buzzzr/buzz-fake-relay`](https://github.com/buzzzr/buzz-fake-relay) is a tiny stand-in for a Buzz relay with fixed seed data (channels,
DMs, profiles, a message per room), so everyone's screenshots show the same thing and no real chat or key ends up in a pull request.

```bash
# terminal 1: the fake relay (default address 127.0.0.1:7447)
git clone https://github.com/buzzzr/buzz-fake-relay && cd buzz-fake-relay && cargo run -- 127.0.0.1:7447

# terminal 2: buzzr from your branch, with a throwaway config and any key (the fake relay does not check it)
export BUZZR_CONFIG_DIR=$(mktemp -d) BUZZ_RELAY_URL=ws://127.0.0.1:7447
export BUZZ_PRIVATE_KEY=1111111111111111111111111111111111111111111111111111111111111111
cargo run
```

Take the screenshot with your terminal or OS tool. To produce a text capture, drive the UI from a script with the fake relay's
`scripts/pty_drive.py`, which presses keys and prints the screen after each one. Check that the capture shows no personal data (your
username in a path, your hostname) before you attach it.

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
