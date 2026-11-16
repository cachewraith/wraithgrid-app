#!/usr/bin/env bash
# Builds the GPUI app's Windows installer with electron-builder's NSIS asset name
# (Wraithgrid-Setup-<version>-x64.exe), so the in-app updater finds it
# (wraithgrid_core::update::asset_name) and an Electron install upgrades in place
# (wraithgrid.nsi), plus latest.yml for electron-updater (the bridge) and SHA256SUMS.txt.
# Layout as Electron's: Wraithgrid.exe, resources\wraithgrid-host.exe.
# Needs the release builds of the app and the host, and NSIS 3 (makensis). Runs in Git Bash on
# Windows; makensis on Linux/macOS also works, given the Windows binaries.
# Usage: package.sh
# TODO(signing): sign Wraithgrid.exe, resources\wraithgrid-host.exe and the installer
# (signtool / Azure Trusted Signing) once there is a certificate, e.g. from
# WINDOWS_CERTIFICATE (base64 .pfx) + WINDOWS_CERTIFICATE_PASSWORD. electron-updater checks
# the publisher only if the installed app-update.yml names one; the Electron builds don't.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
out="$repo/app-rs/target/dist"
stage="$repo/app-rs/target/package/windows"
version=$(grep -m1 '^version' "$repo/app-rs/crates/app/Cargo.toml" | sed -E 's/.*"(.*)".*/\1/')
app="$repo/app-rs/target/release/wraithgrid.exe"
host="$repo/host-rs/target/release/wraithgrid-host.exe"
[[ -f $app ]] || { echo "build the app first: cargo build --release -p wraithgrid" >&2; exit 1; }
[[ -f $host ]] || { echo "build the host first: cargo build --release in host-rs" >&2; exit 1; }

makensis=$(command -v makensis || true)
for p in "/c/Program Files (x86)/NSIS/makensis.exe" "/c/Program Files/NSIS/makensis.exe"; do
  if [[ -z $makensis && -f $p ]]; then makensis=$p; fi
done
[[ -n $makensis ]] || { echo "makensis not found: install NSIS 3" >&2; exit 1; }
# Paths for makensis: Windows form under Git Bash.
native() { if command -v cygpath >/dev/null; then cygpath -w "$1"; else printf '%s' "$1"; fi; }

rm -rf "$stage"
mkdir -p "$stage/resources" "$out"
cp "$app" "$stage/Wraithgrid.exe"
cp "$host" "$stage/resources/wraithgrid-host.exe"
cp "$repo/LICENSE" "$stage/LICENSE.txt"

file="Wraithgrid-Setup-$version-x64.exe"
nums=${version%%[-+]*} # 2.0.0-alpha.1 → 2.0.0 (VIProductVersion takes four numbers)
"$makensis" -V3 -WX -INPUTCHARSET UTF8 \
  "-DVERSION=$version" "-DVERSION_WIN=$nums.0" \
  "-DSTAGE=$(native "$stage")" "-DICON=$(native "$here/wraithgrid.ico")" \
  "-DOUTFILE=$(native "$out/$file")" "$(native "$here/wraithgrid.nsi")"

# Electron installs update into this build through it (the bridge).
"$here/../electron-latest-yml.sh" "$out" "$version" latest.yml "$file"
# The updater checks every download against this file before installing it.
(cd "$out" && sha256sum -- * 2>/dev/null | grep -v -e SHA256SUMS -e '\.yml$' >SHA256SUMS.txt)
ls -l "$out"
