# wt-tui

A TUI dashboard for [worktrunk](https://worktrunk.dev) worktrees — built for
watching agents work rather than for driving them.

Worktrunk is excellent for agents and for scripted use. `wt list` is a great
table, and `wt switch` with no arguments opens a real interactive picker. What
it is not is a place to *sit* and watch: worktrunk's picker is action-oriented,
so you enter it to leave somewhere. wt-tui is observational — a persistent view
you keep open while work happens in it.

```
cargo install --path .
wt tui          # because the binary is named wt-tui, worktrunk exposes it here
```

## What it does

- **Live updates without a server.** Creating or removing a worktree adds or
  removes a directory under the repository's shared `.git/worktrees/`, so a
  filesystem watch is enough to notice an agent's work appear. Your cursor stays
  on the same branch when the list changes underneath you.
- **A table driven by worktrunk's own facts.** Status symbols, default-branch
  relations, integration verdicts, and safe-to-delete dimming all come from
  `wt list`'s output rather than being recomputed, so the two tools never
  disagree about what is safe.
- **Filtering** across branch, path, marker, commit subject, and PR number.
- **A preview pane** showing the selected worktree's diff (default) or log.
- **Create** a worktree for a new branch.
- **Remove**, behind a dialog that explains *why* a branch is or is not safe to
  delete — uncommitted work, integration status, merge conflicts, in-progress
  git operations — and reports worktrunk's own outcome vocabulary afterwards.

## Keys

| Key | Action |
| --- | --- |
| `↑` `↓` `j` `k` | Move selection |
| `g` / `G` | First / last row |
| `Tab` | Switch focus between table and preview |
| `↑` `↓` (preview) | Scroll the preview pane |
| `t` | Cycle preview tab (diff / log) |
| `/` | Filter (live; `Enter` commits, `Esc` clears) |
| `n` | Create a worktree for a new branch |
| `d` | Remove the selected worktree |
| `y` / `c` | Copy branch name / worktree path |
| `e` | Open the worktree in `$EDITOR` |
| `o` | Open the pull request in a browser |
| `r` | Refresh now |
| `?` | Keybinding help |
| `q` | Quit |

## How it talks to worktrunk

Through `wt ... --format=json`, deliberately, and not by depending on the
`worktrunk` crate:

- The crate's `lib.rs` states the library API is not stable and asks
  integrators to open an issue first.
- Its changelog records breaking library changes in 8 of the last 10 releases,
  and it is still `0.x`, so Cargo will not flag them.
- `list`, `picker`, and `remove` are declared in `main.rs`, not the library, so
  the code most worth reusing is not exposed anyway.

The JSON schema is the documented, versioned contract. Depending on it means
wt-tui keeps working across worktrunk updates and inherits new fields for free.

Three defences keep that honest:

1. **No `deny_unknown_fields`, `#[serde(default)]` everywhere.** New fields are
   ignored, missing ones are never errors.
2. **A runtime schema check.** A payload that is not schema 2 — including the
   legacy bare array from `[list] json-schema = 1` — produces an explanation
   with the setting to change, not a blank table.
3. **A contract test** (`tests/contract.rs`) validating recorded output against
   the JSON Schema published at `worktrunk.dev/schema/list-v2.json`. If
   worktrunk drifts, CI fails with the specific mismatch.

`tests/live.rs` additionally exercises the real binary: create/remove round
trips, a worktree created by another process, and worktrunk's refusal to drop
uncommitted changes without `--force`.

## Discovering it as `wt tui`

Worktrunk resolves any executable named `wt-<name>` on `PATH` as `wt <name>`, so
installing the binary is enough. Custom subcommands do not appear in `wt --help`;
if you want it listed in config instead:

```toml
# ~/.config/worktrunk/config.toml
[aliases]
tui = "wt-tui"
```

## Development

```
cargo test          # unit, contract, and live tests (live ones skip without `wt`)
cargo clippy --all-targets
cargo run -- --help
```

CI runs the full suite on macOS and Linux with worktrunk installed, since the
integration *is* the contract.

## Status

Phase 1. Not yet implemented, and each is additive — none needs a contract
change:

- Cross-worktree content search (ripgrep's `ignore` + `grep-searcher`, in-process)
- `wt merge` in the lifecycle, plus `wt step prune` and `wt step relocate`
- A PR/CI view via `wt list --full`, and dev-server URLs
- Bulk operations across a selection

## Licence

MIT OR Apache-2.0, matching worktrunk.
