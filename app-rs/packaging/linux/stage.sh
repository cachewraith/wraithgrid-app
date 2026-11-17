#!/usr/bin/env bash
# Lays out the Linux install tree the Electron packages use, so an upgrade from them lands in
# the same places: /opt/Wraithgrid/wraithgrid (+ the pty host beside it), the desktop file
# and icons; /usr/bin/wraithgrid is a link made after install. Usage: stage.sh <root> <type>
# where type is deb, rpm, pacman or appimage (written to package-type for the updater).
set -euo pipefail
root=$1
type=$2
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
app="$repo/desktop-rs/target/release/wraithgrid"
host="$repo/host-rs/target/release/wraithgrid-host"
[[ -x $app ]] || { echo "build the app first: bun run tauri:renderer && cargo build --release --manifest-path desktop-rs/Cargo.toml" >&2; exit 1; }
[[ -x $host ]] || { echo "build the host first: cargo build --release in host-rs" >&2; exit 1; }

rm -rf "$root"
install -Dm755 "$app" "$root/opt/Wraithgrid/wraithgrid"
install -Dm755 "$host" "$root/opt/Wraithgrid/wraithgrid-host"
printf '%s\n' "$type" >"$root/opt/Wraithgrid/package-type"
install -Dm644 "$repo/LICENSE" "$root/opt/Wraithgrid/LICENSE"
# Static hosts copied to Linux machines over ssh (scripts/build-remote-hosts.sh), when built.
if [[ -d $repo/host-rs/target/remote ]]; then
  mkdir -p "$root/opt/Wraithgrid/remote"
  cp -a "$repo/host-rs/target/remote/." "$root/opt/Wraithgrid/remote/"
fi
install -Dm644 "$here/wraithgrid.desktop" "$root/usr/share/applications/wraithgrid.desktop"
for size in 16 32 48 64 128 256 512; do
  dir="$root/usr/share/icons/hicolor/${size}x${size}/apps"
  mkdir -p "$dir"
  if command -v magick >/dev/null; then
    magick "$repo/build/icon.png" -resize "${size}x${size}" "$dir/wraithgrid.png"
  elif command -v convert >/dev/null; then
    convert "$repo/build/icon.png" -resize "${size}x${size}" "$dir/wraithgrid.png"
  else
    cp "$repo/build/icon.png" "$dir/wraithgrid.png"
  fi
done
echo "staged $type tree in $root"
