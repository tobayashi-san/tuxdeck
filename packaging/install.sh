#!/bin/sh
# Install tuxdeck from a release archive (no Rust toolchain needed).
#
#   ./install.sh                 install for the current user (~/.local), ask for sudo
#                                only to install the udev rule (device access)
#   ./install.sh --system        install to /usr/local (run with sudo)
#   ./install.sh --no-udev       skip the udev rule
#   ./install.sh --with-uinput   also install the optional /dev/uinput rule (virtual keyboard
#                                for hotkeys/text on GNOME/KDE Wayland); it lets every program
#                                of the logged-in user inject keystrokes
#   ./install.sh --uninstall     remove again (profiles and settings are kept)
#
# UDEVRULESDIR=… overrides where the udev rules go (default /etc/udev/rules.d).
set -eu

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
helper=$here/packaging/install-helper.sh
mode=user
udev=yes
uinput=no
action=install
for a in "$@"; do
    case "$a" in
        --system) mode=system ;;
        --no-udev) udev=no ;;
        --with-uinput) uinput=yes ;;
        --uninstall) action=uninstall ;;
        -h | --help) sed -n '/^#!/d; /^#/!q; s/^# \{0,1\}//p' "$0"; exit 0 ;;
        *) echo "Unknown option: $a" >&2; exit 2 ;;
    esac
done
if [ "$udev" = no ] && [ "$uinput" = yes ]; then
    echo "--with-uinput installs a udev rule and cannot be combined with --no-udev." >&2
    exit 2
fi
[ -f "$helper" ] || { echo "$helper is missing; run install.sh from the extracted release archive." >&2; exit 1; }

# XDG base directories as the app resolves them: unset, empty or relative values
# (invalid per the spec) fall back to the defaults.
xdg_dir() {
    case $1 in
        /*) printf '%s\n' "$1" ;;
        *) printf '%s\n' "$2" ;;
    esac
}

if [ "$mode" = system ]; then
    [ "$(id -u)" = 0 ] || { echo "--system needs root: sudo $0 $*" >&2; exit 1; }
    bindir=/usr/local/bin
    datadir=/usr/local/share
    unitdir=/usr/local/lib/systemd/user
else
    [ "$(id -u)" != 0 ] || { echo "Please run without sudo (or use --system)." >&2; exit 1; }
    case ${HOME:-} in
        /*) ;;
        *) echo "HOME is not set to an absolute path." >&2; exit 1 ;;
    esac
    bindir=$HOME/.local/bin
    datadir=$(xdg_dir "${XDG_DATA_HOME:-}" "$HOME/.local/share")
    configdir=$(xdg_dir "${XDG_CONFIG_HOME:-}" "$HOME/.config")
    unitdir=$configdir/systemd/user
fi
udevdir=${UDEVRULESDIR:-/etc/udev/rules.d}
rules=70-tuxdeck.rules
uinput_rules=71-tuxdeck-uinput.rules

have() { command -v "$1" >/dev/null 2>&1; }

as_root() {
    if [ "$(id -u)" = 0 ]; then "$@"; else sudo "$@"; fi
}

if [ "$action" = uninstall ]; then
    if [ "$mode" = user ] && have systemctl; then
        systemctl --user disable --now tuxdeck.service 2>/dev/null || true
    fi
    rm -f "$bindir/tuxdeck" "$datadir/applications/tuxdeck.desktop" "$unitdir/tuxdeck.service"
    rm -rf "$datadir/doc/tuxdeck"
    if [ "$mode" = user ]; then
        # streamdeck:// handler registered by the app at startup (src/urlhandler.rs)
        sh "$helper" unregister-url-handler "$datadir" "$configdir"
        if have systemctl; then
            systemctl --user daemon-reload || true
        fi
    fi
    if [ "$udev" = yes ]; then
        removed=no
        for f in "$udevdir/$rules" "$udevdir/$uinput_rules"; do
            if [ -e "$f" ]; then
                echo "Removing $f (needs root) …"
                if as_root rm -f "$f"; then
                    removed=yes
                else
                    echo "Could not remove $f; delete it as root." >&2
                fi
            fi
        done
        if [ "$removed" = yes ]; then
            as_root udevadm control --reload || true
        fi
    fi
    if [ "$mode" = system ]; then
        echo "Removed. Users who used it: run ./install.sh --uninstall --no-udev as that user"
        echo "to remove their service link and streamdeck:// handler."
    else
        echo "Removed. Profiles and settings are kept in $datadir/tuxdeck and $configdir/tuxdeck."
    fi
    exit 0
fi

[ -f "$here/tuxdeck" ] || { echo "$here/tuxdeck not found; run install.sh from the extracted release archive." >&2; exit 1; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
trap 'exit 1' HUP INT TERM
sh "$helper" desktop "$here/packaging/tuxdeck.desktop" "$bindir/tuxdeck" >"$tmp/tuxdeck.desktop"
sh "$helper" service "$here/packaging/tuxdeck.service" "$bindir/tuxdeck" "$datadir/doc" >"$tmp/tuxdeck.service"

install -Dm755 "$here/tuxdeck" "$bindir/tuxdeck"
install -Dm644 "$here/README.md" "$datadir/doc/tuxdeck/README.md"
[ -f "$here/THIRD-PARTY-LICENSES.md" ] && install -Dm644 "$here/THIRD-PARTY-LICENSES.md" "$datadir/doc/tuxdeck/THIRD-PARTY-LICENSES.md"
install -Dm644 "$tmp/tuxdeck.desktop" "$datadir/applications/tuxdeck.desktop"
install -Dm644 "$tmp/tuxdeck.service" "$unitdir/tuxdeck.service"
if have update-desktop-database; then
    update-desktop-database "$datadir/applications" 2>/dev/null || true
fi
echo "Installed: $bindir/tuxdeck"

udev_changed=no
install_rule() {
    if [ -f "$udevdir/$1" ] && cmp -s "$here/packaging/$1" "$udevdir/$1"; then
        echo "The udev rule $udevdir/$1 is already installed."
        return 0
    fi
    echo "Installing the udev rule $udevdir/$1 (needs root) …"
    if [ -f "$here/packaging/$1" ] && as_root install -Dm644 "$here/packaging/$1" "$udevdir/$1"; then
        udev_changed=yes
    else
        echo "udev rule not installed; later: sudo install -Dm644 '$here/packaging/$1' $udevdir/$1" >&2
    fi
}

if [ "$udev" = yes ]; then
    if [ -f "$udevdir/$rules" ] && grep -q '^KERNEL=="uinput"' "$udevdir/$rules" && [ "$uinput" = no ]; then
        echo "Note: the previously installed rule also granted /dev/uinput access. That is now"
        echo "      opt-in (./install.sh --with-uinput); the access already granted ends with a reboot."
    fi
    install_rule "$rules"
    if [ "$uinput" = yes ]; then
        install_rule "$uinput_rules"
    elif [ -f "$udevdir/$uinput_rules" ]; then
        echo "Note: the optional uinput rule $udevdir/$uinput_rules stays installed."
    fi
    if [ "$udev_changed" = yes ]; then
        as_root udevadm control --reload || true
        as_root udevadm trigger --subsystem-match=hidraw || true
        as_root udevadm trigger --subsystem-match=usb --attr-match=idVendor=0fd9 || true
        if [ "$uinput" = yes ]; then
            as_root udevadm trigger --subsystem-match=misc --sysname-match=uinput || true
        fi
        echo "If the Stream Deck is not detected, unplug it and plug it back in."
    fi
fi

if [ "$mode" = user ] && have systemctl; then
    systemctl --user daemon-reload || true
    echo
    if systemctl --user is-active --quiet tuxdeck.service 2>/dev/null; then
        echo "Restart to use the new version:  systemctl --user restart tuxdeck"
    else
        echo "Start with your session:  systemctl --user enable --now tuxdeck"
    fi
elif [ "$mode" = system ]; then
    echo
    echo "Each user, to start it with the session:  systemctl --user daemon-reload && systemctl --user enable --now tuxdeck"
fi
case ":$PATH:" in
    *":$bindir:"*) ;;
    *) echo "Note: $bindir is not in PATH; the application menu works anyway." ;;
esac
echo "Open the UI:              tuxdeck ui   (or \"TuxDeck\" in the application menu)"
