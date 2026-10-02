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
  filesystem watch is enough to notice an agent's work appear — measured at
  ~260ms. Your cursor stays on the same branch when the list changes underneath
  you.
- **Search across every worktree at once.** One query, all worktrees, results
  grouped by file with the matching file's content alongside. A string present
  in five worktrees is found once and reported as reaching five.
- **A table driven by worktrunk's own facts.** Status symbols, default-branch
  relations, integration verdicts, and safe-to-delete dimming all come from
  `wt list`'s output rather than being recomputed, so the two tools never
  disagree about what is safe.
- **Filtering** across branch, path, marker, commit subject, and PR number.
- **A preview pane** showing the selected worktree's diff (default) or log.
- **Create**, **merge**, and **remove** — each behind a dialog that explains what
  is about to happen, using worktrunk's own reasoning to explain *why*.
- **Prune** in bulk, from a `--dry-run` preview you read before anything is
  removed.
- **PR and CI status** on demand, without putting the forge on the refresh path.

## Keys

| Key | Action |
| --- | --- |
| `↑` `↓` `j` `k` | Move selection |
| `g` / `G` | First / last row |
| `Tab` | Switch focus between table and preview |
| `t` | Cycle preview tab (diff / log) |
| `/` | Filter (live; `Enter` commits, `Esc` clears) |
| `n` | Create a worktree for a new branch |
| `d` | Remove the selected worktree |
| `m` | Merge the selected branch into the default branch |
| `P` | Prune branches that are already integrated |
| `s` | Toggle the cross-worktree search view |
| `S` | Fetch PR and CI status (needs a remote forge and `gh`/`glab`) |
| `y` / `c` | Copy branch name / worktree path |
| `e` | Open the worktree in `$EDITOR` |
| `o` | Open the pull request in a browser |
| `r` | Refresh now |
| `?` | Keybinding help |
| `q` | Quit |

### Search

| Key | Action |
| --- | --- |
| `s` | Toggle search / back to the table |
| `/` | Start a new query (`Enter` runs it) |
| `j` / `k` | Move between results |
| `i` / `I` | Case-insensitive on / off |
| `r` / `R` | Regex mode on / off |
| `f` | Search only the rows passing the table's filter |
| `Enter` | Re-run the current query |

Search is **literal by default**; regex is opt-in. A search box that silently
treats `.` as a wildcard makes `config.rs` also match `configXrs` and buries
the hit you wanted.

## Search and deduplication

Worktrees branched from the same commit have byte-identical trees, and each is a
separate copy on disk. Walking every one searches the same bytes N times for no
extra information, so `git rev-parse HEAD^{tree}` is used as a dedup key: one
walk per distinct tree, then each hit is attributed to every worktree sharing
that tree.

Measured on 6 worktrees over 30MB / 1,200 files per worktree (5 sharing one
tree):

| | deduplicated | naive |
| --- | --- | --- |
| trees walked | 2 | 6 |
| time | 76ms | 296ms |

That is the difference between a feature that feels instant and one that feels
broken as worktree count grows. `cargo run --release --example search_bench -- <repo> <query>`
reproduces the measurement.

Search covers tracked, gitignore-respecting, non-binary files.

## Things worth knowing

**`wt merge` merges the *current* branch into the target.** Running
`wt merge <branch>` from the repository root merges the root's branch *into*
`<branch>` — the opposite of what pressing "merge" on a row means. wt-tui
therefore invokes it as `wt -C <the row's worktree> merge <target>`, and
`tests/lifecycle.rs` asserts both directions.

**A conflicted merge can report success.** Worktrunk may exit cleanly with no
JSON while leaving a rebase in progress, after which every later `wt` command
fails with "a git operation is already in progress". wt-tui does not trust the
payload: it checks the worktree's git directory and says so plainly.

**`wt step prune` skips worktrees younger than `--min-age` (default 1 day).**
A branch created minutes ago points at the same commit as the default branch and
so looks merged. The prune dialog shows the guard and lets you cycle it
(`0` / `1d` / `7d`) so "nothing happened" is never a mystery.

**`--full` reaches the forge over the network** (~1.3s against a real GitHub
repo with 12 worktrees, versus ~0.55s plain), so PR/CI data is fetched only when
you press `S`. The CI column distinguishes *not requested*, *no forge*, and
*collected with nothing to report*, rather than implying a branch has no checks.

### Environment variables

`$EDITOR` opens the selected worktree. It is split on whitespace, so
`EDITOR="code --wait"` works. The split happens in wt-tui and never in a shell:
the worktree is passed as its own argument, and no shell ever sees `$EDITOR`.

`GIT_DIR`, `GIT_WORK_TREE`, `GIT_COMMON_DIR`, `GIT_INDEX_FILE` and
`GIT_OBJECT_DIRECTORY` are **cleared** on every `git` and `wt` child process.
`git -C <path>` sets the working directory but does not override `GIT_DIR`, so an
inherited one would silently retarget the whole tool at a different repository —
`direnv`'s `layout_git` is the realistic source, and `.envrc` is repository
content. With the override in place, `-C <repo>` is the only thing that decides
which repository wt-tui acts on.

`WT_TUI_WT_BIN` overrides the `wt` binary. It exists for the test suite and for
setups where `wt` is not on `PATH`; there is no reason to set it by hand. Like
`$EDITOR` it is used as a single path, so a value containing spaces will not
resolve.

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
uncommitted changes without `--force`. `tests/lifecycle.rs` covers merge
direction and the conflicted-merge case; `tests/search.rs` covers search against
real worktrees, including that deduplication neither loses a result nor
misattributes one.

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

Phases 1 and 2. Not yet implemented, and each is additive — none needs a contract
change:

- `wt step relocate` for worktrees whose path has drifted
- Dev-server URLs (`dev_server.url` / `listening`) per worktree
- Bulk operations across a selection, rather than one row at a time
- Jump-to-file from a search result, opening the worktree in `$EDITOR`

## Licence

MIT OR Apache-2.0, matching worktrunk.
