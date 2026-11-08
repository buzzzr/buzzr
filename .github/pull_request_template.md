<!--
Thanks for the pull request. Please read CONTRIBUTING.md ("Opening a pull request") first.
Delete the comments and any section that truly does not apply, but say so rather than leaving it blank.
-->

## What and why

<!-- What changes, and why. One or two paragraphs. Link the issue: "Closes #123". -->

## Type of change

- [ ] Bug fix
- [ ] New feature or behaviour
- [ ] User-visible change (anything that draws differently or reacts to keys or the mouse differently)
- [ ] Protocol or relay change (`relay.rs`, `live.rs`, `model.rs`)
- [ ] Key, config or storage change (`keyfile.rs`, `setup.rs`, `config.rs`)
- [ ] Docs, CI or tooling only

## Screenshots (required for user-visible changes)

<!--
Run your branch against buzzzr/buzz-fake-relay (see CONTRIBUTING.md) and attach before and after
screenshots, or terminal captures pasted in a ```text block. Add the light theme if you touched colours,
and a narrow terminal (about 80x24) if you touched layout. Say which scenario you drove, e.g.
"general, open the thread on message #3, press r".
Not user-visible? Write "n/a: not user-visible" and say why.
-->

| Before | After |
| --- | --- |
|  |  |

## How it was tested

<!-- The commands you ran and what you saw. Tests you added, and the fake relay scenario for UI changes. -->

## Checklist

- [ ] `./build.sh test` passes and the build has no new warnings
- [ ] Behaviour has tests (`src/app/tests.rs`, the `ui.rs` tests, `relay.rs` builders, `setup.rs` scripted answers)
- [ ] Changed a key, command or overlay: on-screen help (`KEYS`, `HELP`, footer in `ui.rs`) and `README.md` updated
- [ ] Touched protocol code: said above what was run against a relay (`buzzr --selftest`, see `AGENTS.md`)
- [ ] No private keys, real relay addresses or personal data in code, tests, screenshots or captures
- [ ] The diff is only what this change needs (no unrelated reformatting; no whole-tree `cargo fmt`)
