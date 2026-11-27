#!/usr/bin/env bash
# Builds the GPUI app's macOS packages as electron-builder's (electron-builder.yml `mac:`):
# Wraithgrid.app with the same bundle id (dev.cachewraith.wraithgrid), executable name, icon
# and the pty host in Contents/Resources (Electron's process.resourcesPath), then per CPU a dmg
# (to install) and a zip (what electron-updater reads), Wraithgrid-<version>-mac-<arch>.<ext>,
# plus latest-mac.yml (the bridge) and SHA256SUMS.txt. Run on a Mac.
# Needs: the app built per arch (desktop-rs/target/<triple>/release/wraithgrid; for this Mac's
# own arch desktop-rs/target/release/wraithgrid also works) and the universal host
# (scripts/build-mac-host.sh → host-rs/target/release/wraithgrid-host), as the Electron build.
# Usage: package.sh [x64] [arm64]   (default: both)
#
# Signing: ad-hoc ("-") like the Electron builds; Apple Silicon refuses unsigned code.
# TODO(signing): with a Developer ID, set MACOS_SIGN_IDENTITY ("Developer ID Application: …",
# imported into a keychain from MACOS_CERTIFICATE (base64 .p12) + MACOS_CERTIFICATE_PASSWORD),
# which also turns on the hardened runtime, then notarize the dmg and zip with
#   xcrun notarytool submit … --apple-id "$APPLE_ID" --password "$APPLE_APP_SPECIFIC_PASSWORD" \
#     --team-id "$APPLE_TEAM_ID" --wait && xcrun stapler staple Wraithgrid.app
# (electron-builder's env names). Until then in-app updates on macOS open the release page.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
out="$repo/app-rs/target/dist"
work="$repo/app-rs/target/package/macos"
version=$(grep -m1 '^version' "$repo/desktop-rs/Cargo.toml" | sed -E 's/.*"(.*)".*/\1/')
host="$repo/host-rs/target/release/wraithgrid-host"
identity=${MACOS_SIGN_IDENTITY:--}
[[ -x $host ]] || { echo "build the host first: scripts/build-mac-host.sh" >&2; exit 1; }
archs=("$@")
[[ ${#archs[@]} -gt 0 ]] || archs=(x64 arm64)
mkdir -p "$out" "$work"

# The icon, as electron-builder makes it from build/icon.png.
icns="$work/icon.icns"
iconset="$work/icon.iconset"
rm -rf "$iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$repo/build/icon.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  if ((double <= 512)); then
    sips -z "$double" "$double" "$repo/build/icon.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
  fi
done
iconutil -c icns "$iconset" -o "$icns"

sign() {
  local opts=(--force --sign "$identity" --timestamp=none)
  [[ $identity == - ]] || opts=(--force --sign "$identity" --options runtime --timestamp)
  codesign "${opts[@]}" "$@"
}

bundle() { # <arch> <binary>: Wraithgrid.app for one CPU
  local arch=$1 bin=$2
  local app="$work/$arch/Wraithgrid.app"
  rm -rf "${work:?}/${arch:?}"
  mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
  install -m755 "$bin" "$app/Contents/MacOS/Wraithgrid"
  install -m755 "$host" "$app/Contents/Resources/wraithgrid-host"
  cp "$icns" "$app/Contents/Resources/icon.icns"
  cp "$repo/LICENSE" "$app/Contents/Resources/LICENSE"
  printf 'APPL????' >"$app/Contents/PkgInfo"
  cat >"$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleDisplayName</key><string>Wraithgrid</string>
  <key>CFBundleExecutable</key><string>Wraithgrid</string>
  <key>CFBundleIconFile</key><string>icon.icns</string>
  <key>CFBundleIdentifier</key><string>dev.cachewraith.wraithgrid</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>Wraithgrid</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleURLTypes</key>
  <array>
    <dict>
      <key>CFBundleURLName</key><string>Wraithgrid link</string>
      <key>CFBundleURLSchemes</key><array><string>wraithgrid</string></array>
    </dict>
  </array>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSHumanReadableCopyright</key><string>Copyright © 2026 cachewraith</string>
  <key>NSRequiresAquaSystemAppearance</key><false/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST
  plutil -lint "$app/Contents/Info.plist" >/dev/null
  # Inside out: the host, then the bundle (which seals Resources).
  sign "$app/Contents/Resources/wraithgrid-host"
  sign "$app"
  codesign --verify --verbose=1 "$app"
}

zips=()
dmgs=()
for arch in "${archs[@]}"; do
  case $arch in
    arm64) triple=aarch64-apple-darwin cpu=arm64 ;;
    x64) triple=x86_64-apple-darwin cpu=x86_64 ;;
    *) echo "unknown arch $arch (arm64, x64)" >&2; exit 1 ;;
  esac
  bin="$repo/desktop-rs/target/$triple/release/wraithgrid"
  if [[ ! -x $bin && $(uname -m) == "$cpu" ]]; then
    bin="$repo/desktop-rs/target/release/wraithgrid"
  fi
  [[ -x $bin ]] || { echo "build the app first: cargo build --release --target $triple --manifest-path desktop-rs/Cargo.toml" >&2; exit 1; }
  lipo "$bin" -verify_arch "$cpu" || { echo "$bin is not $cpu" >&2; exit 1; }
  lipo "$host" -verify_arch "$cpu" || { echo "$host lacks $cpu: run scripts/build-mac-host.sh" >&2; exit 1; }
  bundle "$arch" "$bin"

  name="Wraithgrid-$version-mac-$arch"
  rm -f "$out/$name.zip" "$out/$name.dmg"
  # ditto keeps the bundle's symlinks, modes and signature, as Squirrel.Mac expects.
  ditto -c -k --sequesterRsrc --keepParent "$work/$arch/Wraithgrid.app" "$out/$name.zip"
  dmgroot="$work/$arch/dmg"
  mkdir -p "$dmgroot"
  cp -R "$work/$arch/Wraithgrid.app" "$dmgroot/"
  ln -s /Applications "$dmgroot/Applications"
  # hdiutil fails now and then on CI runners ("Resource busy"): a few tries.
  for try in 1 2 3; do
    if hdiutil create -volname "Wraithgrid $version" -srcfolder "$dmgroot" -fs HFS+ \
      -format UDZO -ov "$out/$name.dmg" >/dev/null; then
      break
    fi
    [[ $try -lt 3 ]] || exit 1
    sleep 5
  done
  zips+=("$name.zip")
  dmgs+=("$name.dmg")
done

# Electron installs read this (the bridge). electron-updater takes the arm64 files on Apple
# Silicon and the others on Intel. Zips first, then dmgs, `path` = the first zip: the order
# electron-builder wrote for v1.26.0 (x64 first, the default order of this script).
"$here/../electron-latest-yml.sh" "$out" "$version" latest-mac.yml "${zips[@]}" "${dmgs[@]}"
# The updater checks every download against this file before installing it.
(cd "$out" && shasum -a 256 -- * 2>/dev/null | grep -v -e SHA256SUMS -e '\.yml$' >SHA256SUMS.txt)
ls -l "$out"
