#!/bin/sh
# Pane entrypoint: run the wt-tui dashboard.
#
# The plugin's action passes the repository in as $WT_TUI_REPO. When the pane is
# opened directly, without that variable, fall back to the pane's own directory
# so `herdr plugin pane open` still does the obvious thing.
set -eu

root="${HERDR_PLUGIN_ROOT:-$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)}"
binary="$root/target/release/wt-tui"

if [ ! -x "$binary" ]; then
	echo "wt-tui: $binary is missing." >&2
	echo "  'herdr plugin install' builds it; for a linked checkout run:" >&2
	echo "    cargo build --release" >&2
	exit 1
fi

exec "$binary" -C "${WT_TUI_REPO:-$PWD}"
