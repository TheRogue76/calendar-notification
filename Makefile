# Makefile for calendar-notification.
#
# Everyday use:
#   make install          # build (release) + install binary, icon, and desktop entry
#   make install-service  # also install & enable the systemd user service
#   make uninstall        # remove everything install/install-service placed
#   make check            # fmt --check, clippy (deny warnings), tests
#
# Install locations follow the XDG user layout and can be overridden, e.g.
#   make install PREFIX=/usr/local        (system-wide; may need sudo)

PREFIX      ?= $(HOME)/.local
BINDIR      := $(PREFIX)/bin
ICONDIR     := $(PREFIX)/share/icons/hicolor/scalable/apps
APPDIR      := $(PREFIX)/share/applications
SYSTEMDDIR  := $(HOME)/.config/systemd/user

BIN         := calendar-notification
TARGET      := target/release/$(BIN)

.DEFAULT_GOAL := help

.PHONY: help build run install install-desktop install-service \
        uninstall uninstall-service check fmt clippy test coverage clean

help: ## Show this help
	@echo "calendar-notification — make targets:"
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| sort \
		| awk 'BEGIN {FS = ":.*?## "} {printf "  \033[36m%-18s\033[0m %s\n", $$1, $$2}'

build: ## Build the release binary
	cargo build --release

run: build ## Build then run the app
	$(TARGET)

install: build install-desktop ## Build + install binary, icon, and desktop entry
	install -Dm755 $(TARGET) $(BINDIR)/$(BIN)
	@echo "Installed $(BIN) to $(BINDIR) (ensure it is on your PATH)."
	$(MAKE) install-service
install-desktop: ## Install just the icon + desktop entry (no build)
	install -Dm644 assets/$(BIN).svg $(ICONDIR)/$(BIN).svg
	install -Dm644 assets/$(BIN).desktop $(APPDIR)/$(BIN).desktop
	update-icon-caches $(PREFIX)/share/icons/hicolor 2>/dev/null || true
	@echo "Installed icon + desktop entry. Log out/in if the dock icon doesn't refresh."

install-service: ## Install + enable the systemd user service
	install -Dm644 systemd/$(BIN).service $(SYSTEMDDIR)/$(BIN).service
	systemctl --user daemon-reload
	systemctl --user enable --now $(BIN)

uninstall: uninstall-service ## Remove installed binary, icon, and desktop entry
	rm -f $(BINDIR)/$(BIN)
	rm -f $(ICONDIR)/$(BIN).svg
	rm -f $(APPDIR)/$(BIN).desktop
	update-icon-caches $(PREFIX)/share/icons/hicolor 2>/dev/null || true

uninstall-service: ## Stop, disable, and remove the systemd user service
	-systemctl --user disable --now $(BIN) 2>/dev/null
	rm -f $(SYSTEMDDIR)/$(BIN).service
	-systemctl --user daemon-reload 2>/dev/null

check: fmt clippy test ## Run fmt --check, clippy, and tests

fmt: ## Check formatting
	cargo fmt --check

clippy: ## Lint with warnings denied (tests included)
	cargo clippy --tests -- -D warnings

test: ## Run the test suite
	cargo test

coverage: ## Print line-coverage summary
	cargo llvm-cov --summary-only

clean: ## Remove build artifacts
	cargo clean
