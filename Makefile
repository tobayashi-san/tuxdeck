# tuxdeck – build and install on any Linux distribution.
#
#   make                       build (cargo build --release)
#   make install-user          install for the current user (~/.local, no root needed)
#   sudo make install-udev     device access for logged-in users (needed once)
#   sudo make install-uinput   optional: /dev/uinput for logged-in users (virtual keyboard for
#                              hotkeys/text on GNOME/KDE Wayland); lets every program of the
#                              logged-in user inject keystrokes
#   sudo make install          system-wide install (PREFIX=/usr/local)
#   make check                 unit tests, clippy and end-to-end tests (Node.js >= 22)
#
# Packagers: make install PREFIX=/usr DESTDIR=… UDEVRULESDIR=/usr/lib/udev/rules.d
# (installs the device rule only; ship packaging/71-tuxdeck-uinput.rules as an
# optional file, not in the active rules directory)

CARGO ?= cargo
PREFIX ?= /usr/local
DESTDIR ?=
BINDIR ?= $(PREFIX)/bin
DATADIR ?= $(PREFIX)/share
SYSTEMDUSERDIR ?= $(PREFIX)/lib/systemd/user
UDEVRULESDIR ?= /etc/udev/rules.d

# XDG base directories; empty or relative values are invalid per the spec and ignored.
xdg_dir = $(if $(filter /%,$(firstword $(1))),$(1),$(2))
USER_BINDIR ?= $(HOME)/.local/bin
USER_DATADIR ?= $(call xdg_dir,$(XDG_DATA_HOME),$(HOME)/.local/share)
USER_CONFIGDIR ?= $(call xdg_dir,$(XDG_CONFIG_HOME),$(HOME)/.config)
USER_SYSTEMDDIR ?= $(USER_CONFIGDIR)/systemd/user

BIN := target/release/tuxdeck
HELPER := sh packaging/install-helper.sh

# Shell-quote $(1) for use in recipes (install paths may contain spaces).
q = '$(subst ','\'',$(1))'

.PHONY: all build check install uninstall install-user uninstall-user install-udev uninstall-udev \
        install-uinput uninstall-uinput clean

all: build

build:
	$(CARGO) build --release --locked

$(BIN):
	$(CARGO) build --release --locked

check: build
	$(CARGO) test --release --locked
	$(CARGO) clippy --release --locked -- -D warnings
	node tests/i18n-check.mjs
	node tests/e2e.mjs --bin $(BIN)
	node tests/ui-smoke.mjs --bin $(BIN)

# $(1): binary path written into the desktop entry and the service, $(2): bin dir,
# $(3): systemd user unit dir, $(4): data dir to install into, $(5): data dir at run time
# (without DESTDIR, for the Documentation= link).
define install_files
	$(HELPER) desktop packaging/tuxdeck.desktop $(call q,$(1)) > tuxdeck.desktop.tmp
	$(HELPER) service packaging/tuxdeck.service $(call q,$(1)) $(call q,$(5)/doc) > tuxdeck.service.tmp
	install -Dm755 $(BIN) $(call q,$(2)/tuxdeck)
	install -Dm644 README.md $(call q,$(4)/doc/tuxdeck/README.md)
	install -Dm644 THIRD-PARTY-LICENSES.md $(call q,$(4)/doc/tuxdeck/THIRD-PARTY-LICENSES.md)
	install -Dm644 tuxdeck.desktop.tmp $(call q,$(4)/applications/tuxdeck.desktop)
	install -Dm644 tuxdeck.service.tmp $(call q,$(3)/tuxdeck.service)
	install -Dm644 assets/icon/tuxdeck.svg $(call q,$(4)/icons/hicolor/scalable/apps/tuxdeck.svg)
	base=$(call q,$(4)/icons/hicolor); for s in 16 24 32 48 64 128 256; do install -Dm644 "assets/icon/tuxdeck-$$s.png" "$$base/$${s}x$${s}/apps/tuxdeck.png"; done
	rm -f tuxdeck.desktop.tmp tuxdeck.service.tmp
endef

define udev_reload
	-udevadm control --reload
	-udevadm trigger --subsystem-match=hidraw
	-udevadm trigger --subsystem-match=usb --attr-match=idVendor=0fd9
endef

install: $(BIN)
	$(call install_files,$(BINDIR)/tuxdeck,$(DESTDIR)$(BINDIR),$(DESTDIR)$(SYSTEMDUSERDIR),$(DESTDIR)$(DATADIR),$(DATADIR))
	install -Dm644 packaging/70-tuxdeck.rules $(call q,$(DESTDIR)$(UDEVRULESDIR)/70-tuxdeck.rules)
	@echo
	@echo "Installed. Autostart per user: systemctl --user daemon-reload && systemctl --user enable --now tuxdeck"
	@echo "Reload udev (once):            sudo udevadm control --reload && sudo udevadm trigger"
	@echo "Optional, hotkeys/text on GNOME/KDE Wayland: sudo make install-uinput"

uninstall:
	rm -f $(call q,$(DESTDIR)$(BINDIR)/tuxdeck) $(call q,$(DESTDIR)$(DATADIR)/applications/tuxdeck.desktop) \
	      $(call q,$(DESTDIR)$(SYSTEMDUSERDIR)/tuxdeck.service) \
	      $(call q,$(DESTDIR)$(UDEVRULESDIR)/70-tuxdeck.rules) $(call q,$(DESTDIR)$(UDEVRULESDIR)/71-tuxdeck-uinput.rules)
	rm -f $(call q,$(DESTDIR)$(DATADIR)/icons/hicolor/scalable/apps/tuxdeck.svg)
	base=$(call q,$(DESTDIR)$(DATADIR)/icons/hicolor); for s in 16 24 32 48 64 128 256; do rm -f "$$base/$${s}x$${s}/apps/tuxdeck.png"; done
	rm -rf $(call q,$(DESTDIR)$(DATADIR)/doc/tuxdeck)
	@echo "Users who used it: make uninstall-user (removes their service link and streamdeck:// handler)"

install-user: $(BIN)
	$(call install_files,$(USER_BINDIR)/tuxdeck,$(USER_BINDIR),$(USER_SYSTEMDDIR),$(USER_DATADIR),$(USER_DATADIR))
	-if command -v update-desktop-database >/dev/null; then update-desktop-database $(call q,$(USER_DATADIR)/applications); fi
	-if command -v systemctl >/dev/null; then systemctl --user daemon-reload; fi
	@echo
	@echo "Installed to $(USER_BINDIR)/tuxdeck."
	@echo "Device access (once, as root): sudo make install-udev"
	@echo "Autostart:                     systemctl --user enable --now tuxdeck"
	@echo "Running already? Restart:      systemctl --user restart tuxdeck"

uninstall-user:
	-if command -v systemctl >/dev/null; then systemctl --user disable --now tuxdeck.service; fi
	rm -f $(call q,$(USER_BINDIR)/tuxdeck) $(call q,$(USER_DATADIR)/applications/tuxdeck.desktop) \
	      $(call q,$(USER_SYSTEMDDIR)/tuxdeck.service) \
	      $(call q,$(USER_DATADIR)/icons/hicolor/scalable/apps/tuxdeck.svg)
	base=$(call q,$(USER_DATADIR)/icons/hicolor); for s in 16 24 32 48 64 128 256; do rm -f "$$base/$${s}x$${s}/apps/tuxdeck.png"; done
	rm -rf $(call q,$(USER_DATADIR)/doc/tuxdeck)
	$(HELPER) unregister-url-handler $(call q,$(USER_DATADIR)) $(call q,$(USER_CONFIGDIR))
	-if command -v systemctl >/dev/null; then systemctl --user daemon-reload; fi
	@echo "Data and settings are kept: $(USER_DATADIR)/tuxdeck, $(USER_CONFIGDIR)/tuxdeck"

install-udev:
	install -Dm644 packaging/70-tuxdeck.rules $(call q,$(DESTDIR)$(UDEVRULESDIR)/70-tuxdeck.rules)
	$(udev_reload)
	@echo "Unplug the Stream Deck and plug it back in."
	@echo "Optional, hotkeys/text on GNOME/KDE Wayland: sudo make install-uinput"

uninstall-udev:
	rm -f $(call q,$(DESTDIR)$(UDEVRULESDIR)/70-tuxdeck.rules) $(call q,$(DESTDIR)$(UDEVRULESDIR)/71-tuxdeck-uinput.rules)
	-udevadm control --reload

install-uinput:
	install -Dm644 packaging/71-tuxdeck-uinput.rules $(call q,$(DESTDIR)$(UDEVRULESDIR)/71-tuxdeck-uinput.rules)
	-udevadm control --reload
	-udevadm trigger --subsystem-match=misc --sysname-match=uinput
	@echo "Restart the app to use it: systemctl --user restart tuxdeck"

uninstall-uinput:
	rm -f $(call q,$(DESTDIR)$(UDEVRULESDIR)/71-tuxdeck-uinput.rules)
	-udevadm control --reload
	@echo "Access already granted to /dev/uinput ends with the next reboot."

clean:
	$(CARGO) clean
