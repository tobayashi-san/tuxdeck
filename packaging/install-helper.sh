#!/bin/sh
# Shared by install.sh and the Makefile (run it with `sh`, no exec bit needed).
#
#   install-helper.sh desktop TEMPLATE BINARY
#       print the desktop entry with Exec= pointing to BINARY
#   install-helper.sh service TEMPLATE BINARY DOCDIR
#       print the systemd user unit with ExecStart= pointing to BINARY and
#       Documentation= pointing into DOCDIR (the directory that holds tuxdeck/)
#   install-helper.sh unregister-url-handler DATADIR CONFIGDIR
#       remove what `tuxdeck run` registers for streamdeck:// links
#       (register_url_handler in src/urlhandler.rs): DATADIR/applications/
#       tuxdeck-url-handler.desktop and its x-scheme-handler/streamdeck line in
#       CONFIGDIR/mimeapps.list and CONFIGDIR/<desktop>-mimeapps.list
#
# Install paths may contain spaces and other special characters; they are quoted
# per the Desktop Entry spec (Exec key) and systemd.service(5) (command lines,
# specifiers).
set -eu

die() {
    echo "install-helper.sh: $*" >&2
    exit 1
}

check_path() {
    case $1 in
        /*) ;;
        *) die "not an absolute path: $1" ;;
    esac
    case $1 in
        *[[:cntrl:]]*) die "path contains control characters: $1" ;;
    esac
}

# Desktop Entry spec, "The Exec key": an argument containing reserved characters is
# put in double quotes, with " ` $ \ escaped by a backslash; a literal % is written
# %%. The general string escaping (\ as \\) applies on top of that.
desktop_arg() {
    a=$(printf '%s' "$1" | sed 's/%/%%/g')
    case $a in
        *[!A-Za-z0-9/._+,:@=%-]*) a=\"$(printf '%s' "$a" | sed 's/[\\"`$]/\\&/g')\" ;;
    esac
    printf '%s' "$a" | sed 's/\\/\\\\/g'
}

# systemd.service(5), "Command lines": a word may be wrapped in double quotes; % starts
# a specifier and is written %%. The executable path is not subject to $VAR expansion,
# so $ stays as is. systemd refuses executable paths containing quotes or backslashes
# however they are escaped ("Executable path contains special characters").
systemd_exec_path() {
    case $1 in
        *[\"\'\\]*) die "systemd cannot start a program whose path contains quotes or backslashes: $1
Install to another location, or start 'tuxdeck run --no-browser' from your desktop's autostart." ;;
    esac
    a=$(printf '%s' "$1" | sed 's/%/%%/g')
    case $a in
        *[!A-Za-z0-9/._+,:@=%-]*) a=\"$a\" ;;
    esac
    printf '%s' "$a"
}

# file:// URI for Documentation= (a whitespace separated list of URIs, so the path is
# percent-encoded; every % is doubled because unit files expand %-specifiers there).
systemd_file_uri() {
    printf 'file://'
    printf '%s' "$1" | od -An -v -tx1 | tr -cs '0-9a-f' '\n' | while read -r h; do
        case $h in
            '') ;;
            2[def] | 3[0-9] | 4[1-9a-f] | 5[0-9af] | 6[1-9a-f] | 7[0-9ae])
                # unreserved character or /
                # shellcheck disable=SC2059 # the format is a generated octal escape
                printf "\\$(printf '%03o' "0x$h")" ;;
            *) printf '%%%%%s' "$(printf '%s' "$h" | tr a-f A-F)" ;;
        esac
    done
}

render_desktop() {
    template=$1 bin=$2
    check_path "$bin"
    exec_bin=$(desktop_arg "$bin")
    found=no
    while IFS= read -r line || [ -n "$line" ]; do
        case $line in
            Exec=tuxdeck | Exec=tuxdeck\ *)
                printf 'Exec=%s%s\n' "$exec_bin" "${line#Exec=tuxdeck}"
                found=yes ;;
            *) printf '%s\n' "$line" ;;
        esac
    done <"$template"
    [ "$found" = yes ] || die "$template: no 'Exec=tuxdeck' line"
}

render_service() {
    template=$1 bin=$2 docdir=$3
    check_path "$bin"
    check_path "$docdir"
    exec_bin=$(systemd_exec_path "$bin")
    found=no
    while IFS= read -r line || [ -n "$line" ]; do
        case $line in
            ExecStart=%h/.local/bin/tuxdeck | ExecStart=%h/.local/bin/tuxdeck\ *)
                printf 'ExecStart=%s%s\n' "$exec_bin" "${line#ExecStart=%h/.local/bin/tuxdeck}"
                found=yes ;;
            Documentation=file://%h/.local/share/doc/*)
                printf 'Documentation=%s\n' "$(systemd_file_uri "$docdir/${line#Documentation=file://%h/.local/share/doc/}")" ;;
            *) printf '%s\n' "$line" ;;
        esac
    done <"$template"
    [ "$found" = yes ] || die "$template: no 'ExecStart=%h/.local/bin/tuxdeck' line"
}

# Must match HANDLER and SCHEME in src/urlhandler.rs.
handler=tuxdeck-url-handler.desktop
association=x-scheme-handler/streamdeck=$handler

# Print a mimeapps.list without our association in [Default Applications] (the only
# group register_url_handler writes to); everything else is kept byte for byte.
strip_association() {
    awk -v assoc="$association" '
        { t = $0; sub(/^[ \t\r]+/, "", t); sub(/[ \t\r]+$/, "", t) }
        t ~ /^\[/ { group = t }
        group == "[Default Applications]" && t == assoc { next }
        { print }
    ' "$1"
}

unregister_url_handler() {
    data=$1 config=$2
    check_path "$data"
    check_path "$config"
    desktop=$data/applications/$handler
    if [ -e "$desktop" ] || [ -L "$desktop" ]; then
        rm -f "$desktop"
        echo "Removed the streamdeck:// handler $desktop"
        if command -v update-desktop-database >/dev/null 2>&1; then
            update-desktop-database "$data/applications" 2>/dev/null || true
        fi
    fi
    for list in "$config/mimeapps.list" "$config"/*-mimeapps.list; do
        [ -f "$list" ] || continue
        grep -q "^[[:space:]]*x-scheme-handler/streamdeck=tuxdeck-url-handler\.desktop[[:space:]]*\$" "$list" || continue
        if [ -L "$list" ]; then
            # Managed elsewhere (home-manager); the app does not write to it either.
            echo "Note: $list is a symlink, not changed; remove x-scheme-handler/streamdeck from its source." >&2
            continue
        fi
        tmp=$(mktemp "$list.XXXXXX")
        cp -p "$list" "$tmp" # keep the mode
        strip_association "$list" >"$tmp" || { rm -f "$tmp"; die "could not rewrite $list"; }
        if cmp -s "$list" "$tmp"; then
            rm -f "$tmp"
        else
            mv -f "$tmp" "$list"
            echo "Removed $association from $list"
        fi
    done
}

[ $# -ge 1 ] || die "usage: install-helper.sh desktop|service|unregister-url-handler ..."
cmd=$1
shift
case $cmd in
    desktop) [ $# = 2 ] || die "usage: desktop TEMPLATE BINARY"; render_desktop "$@" ;;
    service) [ $# = 3 ] || die "usage: service TEMPLATE BINARY DOCDIR"; render_service "$@" ;;
    unregister-url-handler) [ $# = 2 ] || die "usage: unregister-url-handler DATADIR CONFIGDIR"; unregister_url_handler "$@" ;;
    *) die "unknown command: $cmd" ;;
esac
