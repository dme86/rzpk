.DEFAULT_GOAL := help
CARGO ?= cargo
PREFIX ?= $(HOME)/.local
DESTDIR ?=
TARGET_DIR ?= target
# GNU Make uses /bin/sh for recipes; preserve the user's login shell separately.
USER_SHELL ?= $(shell printenv SHELL)
SETUP_PATH ?= 1

.PHONY: help build run release test check fmt lint clean install install-macos setup-path uninstall test-install

help: ## Show available targets
	@awk 'BEGIN { FS = ":.*## " } /^[a-zA-Z_-]+:.*## / { printf "  %-15s %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

build: ## Build the debug binary
	$(CARGO) build --target-dir "$(TARGET_DIR)"

run: ## Build and run in the current directory
	$(CARGO) run --target-dir "$(TARGET_DIR)"

release: ## Build an optimized binary
	$(CARGO) build --release --target-dir "$(TARGET_DIR)"

test: ## Run unit tests
	$(CARGO) test --target-dir "$(TARGET_DIR)"

check: ## Check Rust source
	$(CARGO) check --target-dir "$(TARGET_DIR)"

fmt: ## Format Rust source
	$(CARGO) fmt --all

lint: ## Run Clippy
	$(CARGO) clippy --all-targets --target-dir "$(TARGET_DIR)" -- -D warnings

clean: ## Remove build artifacts
	$(CARGO) clean --target-dir "$(TARGET_DIR)"

install: release ## Install in ~/.local/bin and set up PATH (macOS / Linux)
	install -d "$(DESTDIR)$(PREFIX)/bin"
	install -m 755 "$(TARGET_DIR)/release/rzpk" "$(DESTDIR)$(PREFIX)/bin/rzpk"
	@if [ -z "$(DESTDIR)" ] && [ "$(SETUP_PATH)" = 1 ]; then \
	  sh scripts/setup-path.sh "$(PREFIX)/bin" "$(USER_SHELL)"; \
	fi

install-macos: install ## Alias for install; rzpk runs inside your terminal

setup-path: ## Add PREFIX/bin to your shell configuration if needed
	sh scripts/setup-path.sh "$(PREFIX)/bin" "$(USER_SHELL)"

uninstall: ## Remove the installed binary (keep config and PATH entries)
	rm -f "$(DESTDIR)$(PREFIX)/bin/rzpk"

test-install: ## Test PATH setup in temporary homes (Python 3)
	python3 scripts/test-install.py
