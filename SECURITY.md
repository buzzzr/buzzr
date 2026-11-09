# Security policy

buzzr handles your Nostr private key, so we take reports seriously.

## Reporting a vulnerability

**Please do not open a public issue or pull request for a security problem.**

Report it privately through GitHub: go to the [Security tab](https://github.com/buzzzr/buzzr/security/advisories/new) and choose
"Report a vulnerability". If that is not available to you, open an issue that says only that you have a security report (no
details) and a maintainer will arrange a private channel.

Please include the version (`buzzr --version`), what you did, what happened, and the impact you see. A proof of concept helps.
Use throwaway keys and a relay you control; never send us a real private key.

You can expect an acknowledgement within a few days. We will keep you informed, agree a disclosure date with you, fix the
issue in a release, and credit you in the advisory if you wish.

## Supported versions

Only the latest release gets security fixes. Until 1.0 that means the newest `0.x` release.

## Scope

Things we want to hear about:

- the private key reaching argv, logs, the screen, the terminal scrollback or a file other users can read, or being stored other
  than as the user chose (see [AGENTS.md](AGENTS.md), principle 4)
- weaknesses in how the key file is sealed or read (`src/keyfile.rs`)
- message text, names or plugin output that can inject terminal escape sequences or otherwise run something (principle 3)
- a plugin getting more than its manifest and `PLUGIN.md` allow, or `--no-plugins` / `BUZZR_SAFE_MODE=1` not turning plugins off
- the update check (`src/release.rs`) or a release download fetching or running something unverified
- signing or sending events the user did not ask for

Out of scope: problems in the relay or in the Buzz server (report those to [Buzz](https://github.com/block/buzz)), attacks that
need an already-compromised machine or account, and a user who chose the plain-file key option and then shared the file.

## Dependencies

CI runs `cargo audit` against the RustSec advisory database, and Dependabot proposes updates. If you find a vulnerable
dependency that buzzr actually exposes, report it as above.
