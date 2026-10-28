#!/bin/sh
# Apply the udev rule (device access) without replugging where possible.
# The optional /dev/uinput rule is not active; see the message below.
if command -v udevadm >/dev/null 2>&1; then
    udevadm control --reload-rules 2>/dev/null || true
    udevadm trigger --subsystem-match=hidraw 2>/dev/null || true
    udevadm trigger --subsystem-match=usb --attr-match=idVendor=0fd9 2>/dev/null || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications 2>/dev/null || true
fi
echo "tuxdeck installed. Start it with your session: systemctl --user enable --now tuxdeck"
echo "Optional, for hotkeys/text on GNOME/KDE Wayland (lets the user's programs inject keystrokes):"
echo "  sudo cp /usr/share/tuxdeck/udev/71-tuxdeck-uinput.rules /etc/udev/rules.d/"
echo "  (then reload udev as described in that file)"
exit 0
