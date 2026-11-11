# Notch Assistant: build, install and package the macOS app.
#
#   make app          build "Notch Assistant.app" into target/bundle.noindex
#   make install      install it into ~/Applications, start it, and open it at login
#   make uninstall    stop it, remove the login item and the app (settings and Keychain stay)
#   make dist         zip the app for a GitHub release (dist/), with its SHA-256
#
# INSTALL_DIR=/Applications make install    installs for every user instead.
# LOGIN=0 make install                      skips open at login.

SHELL := /bin/bash
NAME := Notch Assistant
APP := target/bundle.noindex/$(NAME).app
INSTALL_DIR ?= $(HOME)/Applications
LOGIN ?= 1
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ZIP := dist/NotchAssistant-$(VERSION)-arm64.zip

.PHONY: app install uninstall dist icon clean-dist

app:
	@scripts/bundle.sh target/bundle.noindex

install: app
	@pkill -x notch-assistant && sleep 0.5 || true
	@mkdir -p "$(INSTALL_DIR)"
	@rm -rf "$(INSTALL_DIR)/$(NAME).app"
	@ditto "$(APP)" "$(INSTALL_DIR)/$(NAME).app"
	@if [ "$(LOGIN)" = 1 ]; then printf 'Open at login: '; "$(INSTALL_DIR)/$(NAME).app/Contents/MacOS/notch-assistant" --login on; fi
	@open "$(INSTALL_DIR)/$(NAME).app"
	@echo "Installed $(INSTALL_DIR)/$(NAME).app and started it."

uninstall:
	@if [ -x "$(INSTALL_DIR)/$(NAME).app/Contents/MacOS/notch-assistant" ]; then \
	  "$(INSTALL_DIR)/$(NAME).app/Contents/MacOS/notch-assistant" --login off >/dev/null || true; fi
	@pkill -x notch-assistant || true
	@rm -rf "$(INSTALL_DIR)/$(NAME).app"
	@echo "Removed $(INSTALL_DIR)/$(NAME).app. Your settings and Keychain item are kept."

dist: app
	@mkdir -p dist
	@rm -f "$(ZIP)"
	@ditto -c -k --sequesterRsrc --keepParent "$(APP)" "$(ZIP)"
	@shasum -a 256 "$(ZIP)"

# Redraws assets/AppIcon.icns (needs Xcode command-line tools).
icon:
	@swift scripts/icon.swift assets/AppIcon.icns

clean-dist:
	@rm -rf dist target/bundle.noindex
