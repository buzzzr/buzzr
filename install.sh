#!/bin/sh
# Install buzzr: download the release for this machine, check it against the release's
# SHA256SUMS, and put the binary in ~/.local/bin.
#
#   curl -fsSL https://raw.githubusercontent.com/buzzzr/buzzr/main/install.sh | sh
#
# Options go to the shell that runs the script, e.g. `| sh -s -- --version v0.1.1`:
#   --version TAG   release to install (default: the latest one), e.g. v0.1.1
#   --dir PATH      where the binary goes (default: ~/.local/bin)
#   --help
# BUZZR_VERSION and BUZZR_INSTALL_DIR say the same as the two options.
set -eu

repo=buzzzr/buzzr
version=${BUZZR_VERSION:-}
dir=${BUZZR_INSTALL_DIR:-}

die() { printf 'install: %s\n' "$*" >&2; exit 1; }
say() { printf '%s\n' "$*"; }

usage() {
  cat <<'EOF'
Install the latest buzzr release for this machine.

  curl -fsSL https://raw.githubusercontent.com/buzzzr/buzzr/main/install.sh | sh

  --version TAG   install this release instead of the latest, e.g. --version v0.1.1
  --dir PATH      install into PATH instead of ~/.local/bin
  -h, --help      this text

Options are passed to the shell that runs the script:

  curl -fsSL https://raw.githubusercontent.com/buzzzr/buzzr/main/install.sh | sh -s -- --dir /usr/local/bin

The archive is checked against the release's SHA256SUMS before anything is unpacked, and the binary
is put in place in one step. Running this again replaces it, so it is also the upgrade.
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version) [ $# -ge 2 ] || die "--version needs a tag, e.g. --version v0.1.1"; version=$2; shift 2 ;;
    --dir)     [ $# -ge 2 ] || die "--dir needs a path"; dir=$2; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    -*)        die "unknown option: $1 (try --help)" ;;
    *)         die "unexpected argument: $1 (try --help)" ;;
  esac
done

if [ -z "$dir" ]; then
  [ -n "${HOME:-}" ] || die "HOME is not set: say where to install with --dir"
  dir=$HOME/.local/bin
fi

command -v curl >/dev/null 2>&1 || die "curl is needed to download the release"

# The destination is checked before anything is downloaded, so a machine that cannot write there
# says so instead of fetching an archive it will throw away.
if [ ! -d "$dir" ]; then
  mkdir -p "$dir" || die "could not create $dir"
fi
if [ ! -w "$dir" ]; then
  die "$dir is not writable: use --dir, or the rights it needs"
fi
if [ -e "$dir/buzzr" ] && [ ! -w "$dir/buzzr" ]; then
  die "$dir/buzzr is not writable: use --dir, or the rights it needs"
fi

# One static build per architecture, so the Linux archive runs on any distribution.
case "$(uname -s)" in
  Linux)  os=unknown-linux-musl ;;
  Darwin) os=apple-darwin ;;
  *) die "no release for $(uname -s): the archives cover Linux and macOS. Build from source: https://github.com/$repo#build" ;;
esac
case "$(uname -m)" in
  x86_64|amd64)  cpu=x86_64 ;;
  arm64|aarch64) cpu=aarch64 ;;
  *) die "no release for $(uname -m): the archives cover x86_64 and arm64. Build from source: https://github.com/$repo#build" ;;
esac
target="$cpu-$os"

if [ -z "$version" ]; then
  # github.com/.../releases/latest redirects to the newest tag: one request, and no API token
  # (the API is rate-limited per address, which breaks on shared machines).
  version=$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$repo/releases/latest") \
    || die "could not reach github.com to find the latest release"
  version=${version##*/}
fi
case "$version" in
  v[0-9]*) ;;
  *) die "could not work out a release tag (github said '$version'); name one with --version. https://github.com/$repo/releases" ;;
esac

name="buzzr-${version#v}-$target"
base="https://github.com/$repo/releases/download/$version"

work=$(mktemp -d) || die "could not create a temporary directory"
trap 'rm -rf "$work"' EXIT

say "Downloading $version for $target"
curl -fsSL -o "$work/$name.tar.gz" "$base/$name.tar.gz" || die "could not download $base/$name.tar.gz"
curl -fsSL -o "$work/SHA256SUMS" "$base/SHA256SUMS" || die "could not download $base/SHA256SUMS"

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | awk '{print $1}'
  elif command -v openssl >/dev/null 2>&1; then openssl dgst -sha256 "$1" | awk '{print $NF}'
  else return 1
  fi
}

want=$(awk -v file="$name.tar.gz" '$NF == file { print $1; exit }' "$work/SHA256SUMS")
[ -n "$want" ] || die "SHA256SUMS has no line for $name.tar.gz"
got=$(sha256_of "$work/$name.tar.gz") || die "no sha256sum, shasum or openssl to check the download with"
if [ "$got" != "$want" ]; then
  die "the download does not match SHA256SUMS (expected $want, got $got); nothing was installed"
fi

tar -xzf "$work/$name.tar.gz" -C "$work" || die "could not unpack $name.tar.gz"
[ -f "$work/$name/buzzr" ] || die "no buzzr binary inside $name.tar.gz"

# Write beside the target and rename, so a copy that is running is replaced, not truncated.
staged="$dir/.buzzr.new.$$"
cp "$work/$name/buzzr" "$staged" || die "could not write into $dir"
chmod 755 "$staged"
mv -f "$staged" "$dir/buzzr" || die "could not replace $dir/buzzr"

say "Installed $("$dir/buzzr" --version 2>/dev/null || say buzzr) to $dir/buzzr"

case ":${PATH:-}:" in
  *":$dir:"*) ;;
  *) say ""
     say "$dir is not on your PATH. Add it to your shell's startup file, for example:"
     say "  export PATH=\"$dir:\$PATH\"" ;;
esac
