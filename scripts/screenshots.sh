#!/bin/sh
# Regenerates docs/screenshots/*.png from the real UI.
# Needs Node with Playwright (and its Chromium) available as `require('playwright')`.
set -eu
cd "$(dirname "$0")/.."
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cargo run --quiet --example screenshots -- "$tmp"
mkdir -p docs/screenshots
# `require` doesn't look in the global node_modules, so add it for a global Playwright.
NODE_PATH="${NODE_PATH:+$NODE_PATH:}$(npm root -g)" node scripts/screenshots.js "$tmp" docs/screenshots
