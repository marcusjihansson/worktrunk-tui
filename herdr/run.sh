#!/bin/sh
# Pane entrypoint: run the wt-tui dashboard and hand switches back to Herdr.
#
# The plugin's action passes the repository in as $WT_TUI_REPO. When the pane is
# opened directly, without that variable, fall back to the pane's own directory
# so `herdr plugin pane open` still does the obvious thing.
#
# A terminal `wt tui` inherits WORKTRUNK_DIRECTIVE_CD_FILE from Worktrunk's shell
# wrapper. Herdr starts this pane directly, so there is no parent shell for that
# directive to change. Capture it here, then ask Herdr to open/focus the selected
# worktree after the overlay closes.
set -eu

root="${HERDR_PLUGIN_ROOT:-$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)}"
binary="$root/target/release/wt-tui"
repo="${WT_TUI_REPO:-$PWD}"

if [ ! -x "$binary" ]; then
	echo "wt-tui: $binary is missing." >&2
	echo "  'herdr plugin install' builds it; for a linked checkout run:" >&2
	echo "    cargo build --release" >&2
	exit 1
fi

directive=$(mktemp "${TMPDIR:-/tmp}/wt-tui-cd.XXXXXX")
cleanup() {
	rm -f "$directive"
}
trap cleanup EXIT

status=0
WORKTRUNK_DIRECTIVE_CD_FILE="$directive" "$binary" -C "$repo" || status=$?
if [ "$status" -ne 0 ]; then
	exit "$status"
fi

# Quitting, cancelling, or switching to the current worktree does not write a
# directive. Only a successful switch hands control to another Herdr workspace.
if [ ! -s "$directive" ]; then
	exit 0
fi

target=$(cat "$directive")
if [ -z "$target" ]; then
	exit 0
fi

herdr_bin="${HERDR_BIN_PATH:-herdr}"
"$herdr_bin" worktree open \
	--cwd "$repo" \
	--path "$target" \
	--focus \
	--trust-repository
