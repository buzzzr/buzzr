#!/usr/bin/env sh
# End-to-end UI tests: build buzz-fake-relay, then run the real buzzr against it in a pty.
#
#   tests/e2e/run.sh                 # fetch + build the relay from its default branch
#   tests/e2e/run.sh <cargo args>    # extra arguments go to `cargo test`
#
# Environment:
#   BUZZ_FAKE_RELAY_BIN   a relay binary to use; skips fetching and building (set it to reuse a build)
#   BUZZ_FAKE_RELAY_REPO  instead of the default https://github.com/buzzzr/buzz-fake-relay
#   BUZZ_FAKE_RELAY_REF   a branch/tag/commit to check out (default: main)
#   BUZZ_FAKE_RELAY_DIR   where to clone it (default: target/e2e/buzz-fake-relay)
#
# The relay must answer `GET /`, `POST /query` and `POST /events`, which is how buzzr reads history and
# sends. Without BUZZ_FAKE_RELAY_BIN this script gets it from the relay's own repository.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)

if [ -z "${BUZZ_FAKE_RELAY_BIN:-}" ]; then
    repo=${BUZZ_FAKE_RELAY_REPO:-https://github.com/buzzzr/buzz-fake-relay}
    ref=${BUZZ_FAKE_RELAY_REF:-main}
    dir=${BUZZ_FAKE_RELAY_DIR:-$root/target/e2e/buzz-fake-relay}
    if [ ! -d "$dir/.git" ]; then
        mkdir -p "$(dirname "$dir")"
        echo "cloning $repo -> $dir"
        git clone --quiet "$repo" "$dir"
    fi
    # Offline (or a ref that is already there): use what is checked out rather than failing.
    if git -C "$dir" fetch --quiet origin "$ref" 2>/dev/null; then
        git -C "$dir" checkout --quiet FETCH_HEAD
    fi
    echo "building buzz-fake-relay ($ref)"
    cargo build --quiet --manifest-path "$dir/Cargo.toml"
    BUZZ_FAKE_RELAY_BIN=$dir/target/debug/buzz-fake-relay
fi

export BUZZ_FAKE_RELAY_BIN
exec cargo test --manifest-path "$root/Cargo.toml" --test e2e "$@"
