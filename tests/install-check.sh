#!/bin/sh
# Install and uninstall with install.sh from a release-like folder, in a temporary HOME
# whose path has a space, with stand-ins for sudo, systemctl and udev. Nothing outside
# the temporary folder is touched.
#
#   sh tests/install-check.sh [BINARY]
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
bin=${1:-$root/target/release/tuxdeck}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

# The release archive layout (see .github/workflows/release.yml).
rel=$tmp/release
mkdir -p "$rel/packaging/icon"
if [ -f "$bin" ]; then cp "$bin" "$rel/tuxdeck"; else printf '#!/bin/sh\n' >"$rel/tuxdeck"; fi
cp "$root/packaging/install.sh" "$root/README.md" "$root/LICENSE" "$root/THIRD-PARTY-LICENSES.md" "$root/assets/fonts/LICENSE-DejaVu.txt" "$rel/"
cp "$root/docs/README.md" "$rel/HANDBOOK.md"
cp "$root/packaging/70-tuxdeck.rules" "$root/packaging/71-tuxdeck-uinput.rules" "$root/packaging/tuxdeck-uinput.conf" \
   "$root/packaging/install-helper.sh" "$root/packaging/tuxdeck.desktop" "$root/packaging/tuxdeck.service" "$rel/packaging/"
cp "$root"/assets/icon/tuxdeck.svg "$root"/assets/icon/tuxdeck-*.png "$rel/packaging/icon/"
chmod 755 "$rel/tuxdeck" "$rel/install.sh"

# Stand-ins: sudo runs the command, the rest only log.
stubs=$tmp/stubs
mkdir -p "$stubs"
printf '#!/bin/sh\n"$@"\n' >"$stubs/sudo"
for c in systemctl udevadm modprobe update-desktop-database gtk-update-icon-cache; do
    printf '#!/bin/sh\necho "%s $*" >>"%s/calls"\n' "$c" "$tmp" >"$stubs/$c"
done
chmod 755 "$stubs"/*

home="$tmp/home dir"
mkdir -p "$home/.config"
printf '[Default Applications]\ntext/html=firefox.desktop\nx-scheme-handler/streamdeck=tuxdeck-url-handler.desktop\n' >"$home/.config/mimeapps.list"
run() {
    env -i HOME="$home" PATH="$stubs:/usr/bin:/bin:/run/current-system/sw/bin" UDEVRULESDIR="$tmp/udev" MODULESLOADDIR="$tmp/modules" \
        sh "$rel/install.sh" "$@" >>"$tmp/out" 2>&1 || { cat "$tmp/out"; fail "install.sh $*"; }
}

run --with-uinput
share=$home/.local/share
[ -x "$home/.local/bin/tuxdeck" ] || fail "binary"
for f in README.md HANDBOOK.md THIRD-PARTY-LICENSES.md LICENSE LICENSE-DejaVu.txt; do
    [ -f "$share/doc/tuxdeck/$f" ] || fail "doc $f"
done
grep -q "^Exec=\"$home/.local/bin/tuxdeck\" ui" "$share/applications/tuxdeck.desktop" || fail "desktop Exec: $(grep ^Exec "$share/applications/tuxdeck.desktop")"
grep -q "^ExecStart=\"$home/.local/bin/tuxdeck\" run --no-browser" "$home/.config/systemd/user/tuxdeck.service" || fail "service ExecStart"
grep -q '^RestartPreventExitStatus=3' "$home/.config/systemd/user/tuxdeck.service" || fail "RestartPreventExitStatus"
[ -f "$share/icons/hicolor/scalable/apps/tuxdeck.svg" ] || fail "icon"
[ ! -e "$share/icons/hicolor/icon-theme.cache" ] || fail "no icon cache of our own"
[ -f "$tmp/udev/70-tuxdeck.rules" ] && [ -f "$tmp/udev/71-tuxdeck-uinput.rules" ] || fail "udev rules"
[ -f "$tmp/modules/tuxdeck-uinput.conf" ] || fail "uinput module listed for loading"
grep -q '^modprobe uinput' "$tmp/calls" || fail "uinput module loaded"

run --uninstall
[ ! -e "$home/.local/bin/tuxdeck" ] && [ ! -e "$share/applications/tuxdeck.desktop" ] && [ ! -e "$home/.config/systemd/user/tuxdeck.service" ] || fail "uninstall"
[ ! -e "$share/doc/tuxdeck" ] || fail "docs removed"
[ ! -e "$tmp/udev/70-tuxdeck.rules" ] && [ ! -e "$tmp/modules/tuxdeck-uinput.conf" ] || fail "rules removed"
grep -q '^text/html=firefox.desktop' "$home/.config/mimeapps.list" || fail "other default applications kept"
! grep -q streamdeck "$home/.config/mimeapps.list" || fail "streamdeck:// handler removed"
echo "install.sh: ok"
