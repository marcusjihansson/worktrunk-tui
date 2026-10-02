#!/bin/sh
# Action: open the wt-tui dashboard in a pane over the current workspace.
#
# The repository is resolved here and handed to the pane through the
# environment, so the dashboard is correct regardless of how herdr resolves a
# plugin pane's own working directory.
set -eu

dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
# shellcheck source=repo.sh
. "$dir/repo.sh"

repo=$(resolve_repo) || exit 1

herdr_bin="${HERDR_BIN_PATH:-herdr}"

exec "$herdr_bin" plugin pane open \
	--plugin marcusjihansson.wt-tui \
	--entrypoint dashboard \
	--placement overlay \
	--env "WT_TUI_REPO=$repo" \
	--focus
