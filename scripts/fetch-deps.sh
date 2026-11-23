#!/usr/bin/env bash
# Fetch the compiler (with its standard library) and the sigil-db core at the SHAs in deps/pins.env.
# Each checkout is verified to be exactly the pinned commit.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=../deps/pins.env
source "$root/deps/pins.env"
dest="${1:-$root/.deps}"
mkdir -p "$dest"

fetch() {
  local name="$1" url="$2" sha="$3" dir="$dest/$1"
  if [ ! -d "$dir/.git" ]; then
    git init --quiet "$dir"
    git -C "$dir" remote add origin "$url"
  fi
  if [ "$(git -C "$dir" rev-parse --verify --quiet HEAD || true)" != "$sha" ]; then
    git -C "$dir" fetch --quiet --depth=1 origin "$sha"
    git -C "$dir" checkout --quiet --detach "$sha"
  fi
  if [ "$(git -C "$dir" rev-parse HEAD)" != "$sha" ]; then
    echo "error: $name is not at $sha" >&2
    exit 1
  fi
  if [ -n "$(git -C "$dir" status --porcelain)" ]; then
    echo "error: $name has local changes" >&2
    exit 1
  fi
  echo "$name $sha"
}

fetch sigil-compiler https://github.com/goodnasubi/sigil.git "$SIGIL_COMPILER_SHA"
fetch sigil-db-core https://github.com/goodnasubi/sigil-db.git "$SIGIL_DB_CORE_SHA"
