#!/bin/sh
# As the Electron packages do: the `wraithgrid` command, and the desktop caches refreshed.
ln -sf /opt/Wraithgrid/wraithgrid /usr/bin/wraithgrid
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q /usr/share/applications || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor || true
exit 0
