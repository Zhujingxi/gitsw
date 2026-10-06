#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
destination=${1:-"$HOME/.local/bin"}
cd "$root"
cargo build --release --locked
mkdir -p "$destination"
# Install through a temporary file so the existing command stays usable until replacement.
staged=$(mktemp "$destination/.gitsw-install.XXXXXX")
trap 'rm -f "$staged"' EXIT HUP INT TERM
install -m 755 target/release/gitsw "$staged"
mv -f "$staged" "$destination/gitsw"
printf 'Installed %s/gitsw\nEnsure %s is in your PATH.\n' "$destination" "$destination"
