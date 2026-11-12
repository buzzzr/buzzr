# End-to-end UI tests

These tests start the real `buzzr` binary in a pseudo-terminal and read the screen it draws, so they
check the things the unit tests cannot see: which channel opens, what the sidebar lists, what the
arrow keys and Enter do, how the panes cycle, what the overlays say, and that the program quits
cleanly. Everything on screen comes from [`buzz-fake-relay`](https://github.com/buzzzr/buzz-fake-relay),
so nothing is published anywhere and no identity is needed.

## Run them

```bash
tests/e2e/run.sh                      # fetches, builds and runs the relay, then the tests
tests/e2e/run.sh -- --nocapture        # extra arguments go to `cargo test`
BUZZ_FAKE_RELAY_BIN=/path/to/buzz-fake-relay tests/e2e/run.sh   # reuse a relay you already built
```

`./build.sh e2e` does the same thing. Plain `cargo test` (and so CI) skips these tests: they need
`BUZZ_FAKE_RELAY_BIN` to name a built relay, and print a line saying which test was skipped
otherwise. That keeps `cargo test` fast and offline, and the suite opt-in.

The relay must answer `GET /` (NIP-11), `POST /query` and `POST /events` — the plain-HTTP surface
buzzr reads history and sends on. A case that checks what buzzr *published* (the agents flow) also
needs the relay to keep the events it is sent and serve them back from `POST /query`. `run.sh` checks
out the relay's `main`; set `BUZZ_FAKE_RELAY_REF` to try another branch, or `BUZZ_FAKE_RELAY_REPO` for
a fork.

## Every new feature adds a case

**A change to navigation, to what is drawn, or to a key binding is not finished until this suite
covers it.** Add a test to `tests/e2e.rs` next to the others; if the relay needs new data for it
(a channel, a thread, a profile), add that to `buzz-fake-relay` rather than faking it here.

The unit tests still carry the bulk of the behaviour: `src/app/tests.rs` drives `App::handle` with
synthetic events (fast, no terminal) and `ui.rs`'s tests draw into a `TestBackend`. Use those for
logic and geometry, and this suite for what only the real process and a real relay can show.

## How it works

`tests/e2e.rs` opens a pty, starts `buzzr` on it against a relay it starts itself on a free port,
and feeds the output through a `vt100` parser, so a test can wait for text and assert on the screen
(and a failure prints the whole screen with a column ruler). Keys are written as the bytes a
terminal sends (`Esc`, arrows, Tab/Shift+Tab, Ctrl+Q, …); a lone `Esc` waits a moment so the
program can tell it from the start of an escape sequence.

A plain pty answers nothing, so `buzzr` treats it as a terminal that cannot report Shift+Enter.
`Session::start_enhanced` answers the keyboard-enhancement probe (`CSI ? u` and `CSI c`) the way a
kitty-protocol terminal does, so a test can drive the Shift+Enter that kitty, foot, WezTerm, Ghostty
and iTerm2 send.

A test of the agents flow needs a harness — `claude-agent-acp` — which is not installed in a test
environment. `tests/fixtures/claude-agent-acp` is a fake that answers the ACP that flow reads, and the
suite puts it first on `PATH`; the unit tests in `src/harness/acp.rs` drive the same file.
