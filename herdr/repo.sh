#!/bin/sh
# Print the repository wt-tui should open.
#
# Herdr runs plugin commands with the plugin directory as their working
# directory, so wt-tui's default of "the current directory" would be the plugin
# checkout rather than the project the user is looking at. Verified with herdr
# 0.9.1: an action's `pwd` is the plugin root.
#
# Resolution order:
#   1. $WT_TUI_REPO     explicit override
#   2. focused_pane_cwd the pane the command was invoked from
#   3. workspace_cwd    the workspace containing that pane
#   4. $PWD             the pane's own directory, if invoked without a context
#
# Prints the path on stdout, or exits non-zero with a reason on stderr.

# Extract one string field from a compact JSON object on stdin.
#
# herdr emits compact JSON with no whitespace, and each of these keys occurs
# exactly once at the top level, so a greedy match is unambiguous here.
json_field() {
	sed -n "s/.*\"$1\":\"\([^\"]*\)\".*/\1/p"
}

resolve_repo() {
	if [ -n "${WT_TUI_REPO:-}" ]; then
		printf '%s\n' "$WT_TUI_REPO"
		return 0
	fi

	candidate=""
	if [ -n "${HERDR_PLUGIN_CONTEXT_JSON:-}" ]; then
		candidate=$(printf '%s' "$HERDR_PLUGIN_CONTEXT_JSON" | json_field focused_pane_cwd)
		if [ -z "$candidate" ]; then
			candidate=$(printf '%s' "$HERDR_PLUGIN_CONTEXT_JSON" | json_field workspace_cwd)
		fi
	fi

	[ -n "$candidate" ] || candidate=$(pwd)

	# wt-tui can remove worktrees and branches. Refuse to open it on a path that
	# is not a repository rather than rendering an empty table: a wrong or
	# half-parsed path should say so, not look like "no worktrees exist".
	if ! git -C "$candidate" rev-parse --git-dir >/dev/null 2>&1; then
		echo "wt-tui: $candidate is not a git repository." >&2
		echo "  Open this from a project pane, or set WT_TUI_REPO to a repository path." >&2
		return 1
	fi

	printf '%s\n' "$candidate"
}
