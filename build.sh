#!/usr/bin/env bash
# Build and test buzzr.
#   ./build.sh check        cargo check
#   ./build.sh test         cargo test
#   ./build.sh release      optimised binary (a static musl binary when built in Docker) -> dist/buzzr
#   ./build.sh <cargo args> anything else is passed to cargo
#
# Where cargo runs:
#   - this machine, if cargo is installed (the default)
#   - a Rust container on this machine, if there is no cargo but there is Docker (or BUZZR_BUILD=docker)
#   - a Rust container on another machine, if BUZZR_BUILD_HOST=<ssh host> (the source is copied there; needs rsync, ssh, docker)
# In both container modes the cargo registry and the target directory live in Docker volumes, so rebuilds are incremental.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE"

case "${1:-check}" in
  check) args="check --all-targets" ;;
  test) args="test" ;;
  release) args="build --release" ;;
  *) args="$*" ;;
esac
release=false
[ "${1:-}" = release ] && release=true

IMAGE=buzzr-builder
DOCKERFILE='FROM rust:1.98-alpine3.22
RUN apk add --no-cache build-base cmake perl pkgconf git musl-dev'

# Run cargo (and, for a release, copy the binary to dist/) in the builder image. $1 = source directory for the container.
docker_script() {
  cat <<EOF
docker image inspect $IMAGE >/dev/null 2>&1 || printf '%s\n' '$DOCKERFILE' | docker build -q -t $IMAGE -f - .
docker run --rm -v "$1":/src -v buzzr-cargo:/usr/local/cargo/registry -v buzzr-cargo-git:/usr/local/cargo/git -v buzzr-target:/src/target -w /src -e CARGO_TERM_COLOR=never $IMAGE cargo $args
EOF
  if $release; then
    echo "docker run --rm -v \"$1\":/src -v buzzr-target:/src/target -w /src $IMAGE sh -c 'mkdir -p /src/dist && cp target/release/buzzr /src/dist/buzzr && ls -la /src/dist/buzzr'"
  fi
}

if [ -n "${BUZZR_BUILD_HOST:-}" ]; then
  rsync -a --delete --exclude target --exclude dist --exclude .git "$HERE/" "$BUZZR_BUILD_HOST:buzzr-src/"
  ssh "$BUZZR_BUILD_HOST" "cd buzzr-src && $(docker_script '$HOME/buzzr-src')"
  echo "Built on $BUZZR_BUILD_HOST: ~/buzzr-src/dist/buzzr"
elif [ "${BUZZR_BUILD:-}" != docker ] && command -v cargo >/dev/null 2>&1; then
  # shellcheck disable=SC2086
  cargo $args
  if $release; then mkdir -p dist && cp target/release/buzzr dist/buzzr && ls -la dist/buzzr; fi
elif command -v docker >/dev/null 2>&1; then
  bash -c "$(docker_script "$HERE")"
else
  echo "Need cargo (https://rustup.rs), or Docker, or BUZZR_BUILD_HOST=<ssh host with docker>." >&2
  exit 1
fi
