#!/usr/bin/env bash
# Builds the Linux packages of the GPUI app with the Electron packages' names and package
# name (`wraithgrid`), so installing one upgrades the Electron install in place and the
# in-app updater finds its file (wraithgrid_core::update::asset_name). Needs fpm (deb, rpm,
# pacman) and appimagetool (AppImage). Usage: package.sh [deb|rpm|pacman|appimage|all]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
out="$repo/app-rs/target/dist"
version=$(grep -m1 '^version' "$repo/desktop-rs/Cargo.toml" | sed -E 's/.*"(.*)".*/\1/')
arch=$(uname -m)
mkdir -p "$out"

# Run-time libraries of the binary (readelf -d … NEEDED), by each distro's package names.
# The tray icon also uses libayatana-appindicator3 when present (loaded at run time).
deb_deps=(libwebkit2gtk-4.1-0 libgtk-3-0 libsoup-3.0-0 libdbus-1-3 xdg-utils)
rpm_deps=(webkit2gtk4.1 gtk3 libsoup3 dbus-libs xdg-utils)
pacman_deps=(webkit2gtk-4.1 gtk3 libsoup3 dbus xdg-utils)

fpm_build() {
  local type=$1 file=$2; shift 2
  local root="$repo/app-rs/target/package/$type"
  "$here/stage.sh" "$root" "$type"
  local deps=()
  for d in "$@"; do deps+=(--depends "$d"); done
  fpm -s dir -t "$type" -C "$root" -f -p "$out/$file" \
    --name wraithgrid --version "${version//-/\~}" --license MIT \
    --vendor cachewraith --maintainer cachewraith --url https://github.com/cachewraith/wraithgrid \
    --description "Run several claude CLI sessions in one window, each with its own account (CLAUDE_CONFIG_DIR) and project folder." \
    --category Development \
    --after-install "$here/after-install.sh" --after-remove "$here/after-remove.sh" \
    "${deps[@]}" .
}

case_arch() { # deb, rpm, pacman, AppImage spellings of this machine's arch
  case "$arch" in
    x86_64) echo "amd64 x86_64 x64 x86_64" ;;
    aarch64) echo "arm64 aarch64 aarch64 arm64" ;;
    *) echo "$arch $arch $arch $arch" ;;
  esac
}
read -r a_deb a_rpm a_pacman a_appimage <<<"$(case_arch)"

build_appimage() {
  local root="$repo/app-rs/target/package/appimage"
  "$here/stage.sh" "$root" appimage
  local dir="$repo/app-rs/target/package/Wraithgrid.AppDir"
  rm -rf "$dir"
  mkdir -p "$dir"
  cp -a "$root/opt/Wraithgrid/." "$dir/"
  cp "$root/usr/share/applications/wraithgrid.desktop" "$dir/wraithgrid.desktop"
  sed -i 's#^Exec=.*#Exec=wraithgrid %U#' "$dir/wraithgrid.desktop"
  cp "$root/usr/share/icons/hicolor/512x512/apps/wraithgrid.png" "$dir/wraithgrid.png"
  cp -a "$root/usr" "$dir/usr"
  ln -sf wraithgrid "$dir/AppRun"
  ARCH=$arch appimagetool --no-appstream "$dir" "$out/Wraithgrid-$version-$a_appimage.AppImage"
}

what=${1:-all}
[[ $what == deb || $what == all ]] && fpm_build deb "wraithgrid-$version-$a_deb.deb" "${deb_deps[@]}"
[[ $what == rpm || $what == all ]] && fpm_build rpm "wraithgrid-$version-$a_rpm.rpm" "${rpm_deps[@]}"
[[ $what == pacman || $what == all ]] && fpm_build pacman "wraithgrid-$version-$a_pacman.pacman" "${pacman_deps[@]}"
[[ $what == appimage || $what == all ]] && build_appimage
# Electron installs update into this build through these (the bridge).
if [[ $what == all ]]; then
  "$here/../electron-latest-yml.sh" "$out" "$version" latest-linux.yml \
    "Wraithgrid-$version-$a_appimage.AppImage" "wraithgrid-$version-$a_deb.deb" \
    "wraithgrid-$version-$a_rpm.rpm" "wraithgrid-$version-$a_pacman.pacman"
fi
# The updater checks every download against this file before installing it.
(cd "$out" && sha256sum -- * 2>/dev/null | grep -v -e SHA256SUMS -e '\.yml$' >SHA256SUMS.txt)
ls -l "$out"
