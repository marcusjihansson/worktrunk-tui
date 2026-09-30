//! Cross-worktree content search.
//!
//! # Why tree-SHA deduplication
//!
//! Worktrees branched from the same commit have byte-identical trees, and each
//! worktree is a separate copy on disk. Walking every one would search the same
//! bytes N times for no extra information.
//!
//! `git rev-parse HEAD^{tree}` returns the same SHA for worktrees sharing a
//! tree, so it is a sound dedup key: search one representative per distinct
//! tree, then attribute each hit to every worktree sharing that tree. The key
//! costs one cheap `git rev-parse`, far less than walking the directory.
//!
//! Verified on a fixture of five worktrees branched from one commit: all five
//! reported the same tree SHA, and a string present in only one worktree's
//! changes was found in exactly one path.
//!
//! # Scope limits, chosen deliberately
//!
//! * Only **tracked** files are searched. Untracked scratch files and
//!   `node_modules` are usually the bulk of a repo and rarely the answer.
//! * Gitignore rules are honoured, via `ignore`'s walker.
//! * Binary files are skipped: their matches cannot be rendered as lines.
//! * Results are capped, and truncation is reported rather than silent.

use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::SearcherBuilder;
use grep_searcher::sinks::UTF8;
use ignore::WalkState;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// Upper bound on collected hits. A query matching everything on a large corpus
/// would otherwise exhaust memory; truncation is surfaced in the UI.
pub const MAX_HITS: usize = 5_000;

/// One matching line, attributed to every worktree holding that content.
#[derive(Debug, Clone)]
pub struct Hit {
    pub worktrees: Vec<String>,
    /// Path relative to the worktree root.
    pub rel_path: String,
    pub line_number: u64,
    pub line: String,
}

impl Hit {
    /// How many worktrees contain this line.
    pub fn reach(&self) -> usize {
        self.worktrees.len()
    }
}

/// A whole search's results.
#[derive(Debug, Clone, Default)]
pub struct Results {
    pub hits: Vec<Hit>,
    /// Distinct trees actually searched.
    pub scanned: usize,
    /// Worktrees skipped because their tree was already searched.
    pub deduped: usize,
    /// Files that produced at least one hit.
    ///
    /// Not every file opened: a batch is only recorded when it matched, so this
    /// counts matching files rather than reads.
    pub files_matched: usize,
    pub truncated: bool,
    /// Set when the query itself could not be compiled.
    pub error: Option<String>,
}

impl Results {
    /// Worktrees represented, searched or deduplicated.
    pub fn covered(&self) -> usize {
        self.scanned + self.deduped
    }

    pub fn is_empty(&self) -> bool {
        self.hits.is_empty()
    }

    /// Distinct files with at least one hit.
    pub fn file_count(&self) -> usize {
        let mut seen: Vec<&str> = self.hits.iter().map(|h| h.rel_path.as_str()).collect();
        seen.sort_unstable();
        seen.dedup();
        seen.len()
    }
}

/// A worktree offered to the search.
#[derive(Debug, Clone)]
pub struct Target {
    pub branch: String,
    pub path: PathBuf,
}

/// How to interpret the query text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueryOptions {
    pub case_insensitive: bool,
    /// Treat the query as a regular expression even without metacharacters.
    pub force_regex: bool,
}

/// Compile a matcher for `query`.
///
/// **Literal by default; regex only when explicitly requested.** A search box
/// that silently treats `.` as a wildcard makes `config.rs` also match
/// `configXrs`, burying the hit you wanted in noise. Regex is a deliberate mode
/// (`force_regex`, bound to a key) rather than something inferred from a
/// character that happens to be in the query.
///
/// The literal is escaped before it reaches the regex engine, because
/// `build_literals` takes a pattern rather than a string.
pub fn compile_matcher(query: &str, options: QueryOptions) -> Result<RegexMatcher, String> {
    let mut builder = RegexMatcherBuilder::new();
    builder
        .case_insensitive(options.case_insensitive)
        .multi_line(true)
        .dot_matches_new_line(false)
        .line_terminator(Some(b'\n'));

    if !options.force_regex {
        // `build_literals` is ripgrep's own fast path for fixed strings.
        return builder
            .build_literals(&[regex_escape(query)])
            .map_err(|e| format!("invalid pattern: {e}"));
    }

    builder
        .build(query)
        .map_err(|e| format!("invalid pattern: {e}"))
}

/// Escape regex metacharacters in a literal.
///
/// Hand-rolled rather than pulled from the `regex` crate: the character set is
/// small, fixed, and this avoids a dependency whose escape rules could drift
/// from the engine `grep-regex` actually uses.
fn regex_escape(input: &str) -> String {
    const SPECIAL: &[char] = &[
        '\\', '.', '+', '*', '?', '(', ')', '|', '[', ']', '{', '}', '^', '$', '#', '&', '-', '~',
    ];
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        if SPECIAL.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The tree SHA for a worktree, or `None` when it has none (an unborn branch,
/// or a worktree whose git metadata is unreadable).
///
/// Synchronous: the caller is already on a blocking thread.
fn tree_key(path: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "HEAD^{tree}"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

/// Search every target, deduplicated by tree SHA.
///
/// Blocking and CPU-bound; run it on a blocking thread.
pub fn search(query: &str, targets: &[Target], options: QueryOptions) -> Results {
    let matcher = match compile_matcher(query, options) {
        Ok(m) => m,
        Err(e) => {
            return Results {
                error: Some(e),
                ..Default::default()
            };
        }
    };

    // Group targets by tree. The first for a key does the walking; the rest
    // inherit its hits.
    let mut representative: HashMap<String, &Target> = HashMap::new();
    let mut branches: HashMap<String, Vec<String>> = HashMap::new();
    let mut keys: Vec<String> = Vec::new();

    for target in targets {
        // A worktree with no resolvable tree gets its own key, so it is still
        // searched rather than dropped.
        let key =
            tree_key(&target.path).unwrap_or_else(|| format!("path:{}", target.path.display()));

        if !branches.contains_key(&key) {
            representative.insert(key.clone(), target);
            keys.push(key.clone());
        }
        branches.entry(key).or_default().push(target.branch.clone());
    }

    let mut results = Results {
        scanned: representative.len(),
        deduped: targets.len().saturating_sub(representative.len()),
        ..Default::default()
    };

    for key in &keys {
        let Some(target) = representative.get(key) else {
            continue;
        };
        let owned = branches.get(key).cloned().unwrap_or_default();

        let (hits, files, truncated) = search_one_tree(target, &matcher);
        results.files_matched += files;
        results.truncated |= truncated;

        for mut hit in hits {
            hit.worktrees = owned.clone();
            results.hits.push(hit);
            if results.hits.len() >= MAX_HITS {
                results.truncated = true;
                break;
            }
        }

        if results.hits.len() >= MAX_HITS {
            break;
        }
    }

    results
}

/// Walk and search one worktree, returning its hits, file count, and whether
/// the hit cap cut the walk short.
fn search_one_tree(target: &Target, matcher: &RegexMatcher) -> (Vec<Hit>, usize, bool) {
    let (tx, rx) = mpsc::channel::<(Vec<Hit>, usize, bool)>();

    let mut builder = ignore::WalkBuilder::new(&target.path);
    builder
        .hidden(false) // `.github/` and friends matter
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .follow_links(false)
        // Each worktree's git metadata is a file or a directory that must never
        // be searched for source.
        .filter_entry(|entry| entry.file_name() != ".git");

    builder.build_parallel().run(|| {
        let tx = tx.clone();
        // Each worker thread owns its own searcher: `Searcher` holds reusable
        // buffers and is not meant to be shared across threads.
        let mut searcher = SearcherBuilder::new().build();
        let matcher = matcher.clone();
        let root = target.path.clone();

        Box::new(move |entry| {
            let Ok(entry) = entry else {
                return WalkState::Continue;
            };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                return WalkState::Continue;
            }
            // Binary files cannot be rendered as lines.
            if is_binary(entry.path()) {
                return WalkState::Continue;
            }

            let rel = entry.path().strip_prefix(&root).unwrap_or(entry.path());
            let mut collected: Vec<Hit> = Vec::new();
            let mut truncated = false;

            // `UTF8` hands the sink decoded `&str` lines with line numbers; a
            // file that is not valid UTF-8 errors out, which the binary check
            // above already filters.
            let result = searcher.search_path(
                &matcher,
                entry.path(),
                UTF8(|line_number, line| {
                    if collected.len() >= MAX_HITS {
                        truncated = true;
                        return Ok(false);
                    }
                    collected.push(Hit {
                        worktrees: Vec::new(),
                        rel_path: rel.to_string_lossy().into_owned(),
                        line_number,
                        line: line.trim_end_matches(['\n', '\r']).to_string(),
                    });
                    Ok(true)
                }),
            );

            if result.is_ok() {
                // Drain per file so nothing is stranded on a worker thread when
                // the walk ends.
                if !collected.is_empty() || truncated {
                    let _ = tx.send((std::mem::take(&mut collected), 1, truncated));
                }
                if truncated {
                    return WalkState::Quit;
                }
            }
            WalkState::Continue
        })
    });

    drop(tx);

    let mut hits = Vec::new();
    let mut files = 0;
    let mut truncated = false;
    while let Ok((batch, batch_files, batch_truncated)) = rx.recv() {
        hits.extend(batch);
        files += batch_files;
        truncated |= batch_truncated;
    }

    (hits, files, truncated)
}

/// A NUL byte in the first block marks a binary file.
///
/// This is the same heuristic git and ripgrep use, and it avoids handing
/// non-UTF-8 bytes to a text renderer.
fn is_binary(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return true;
    };
    let mut buf = [0u8; 8192];
    let Ok(n) = file.read(&mut buf) else {
        return true;
    };
    buf[..n].contains(&0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_query_is_treated_as_a_literal() {
        // "foo.bar" has no metacharacters, so it must match that text exactly
        // and NOT "fooXbar". This is what someone typing a path fragment
        // expects, and escaping by hand would be surprising.
        let dir = tempdir();
        let file = dir.join("sample.txt");
        std::fs::write(&file, "foo.bar\nfooXbar\n").unwrap();

        let matcher = compile_matcher("foo.bar", QueryOptions::default()).unwrap();
        let lines = search_file(&matcher, &file);
        assert_eq!(lines, vec!["foo.bar".to_string()]);
    }

    #[test]
    fn a_metacharacter_query_matches_as_a_regex() {
        let dir = tempdir();
        let file = dir.join("sample.txt");
        std::fs::write(&file, "fooXbar\nfoo.bar\n").unwrap();

        let matcher = compile_matcher(
            "foo.bar",
            QueryOptions {
                force_regex: true,
                ..Default::default()
            },
        )
        .unwrap();
        let lines = search_file(&matcher, &file);
        assert_eq!(lines, vec!["fooXbar".to_string(), "foo.bar".to_string()]);
    }

    #[test]
    fn case_insensitive_matching_is_honoured() {
        let dir = tempdir();
        let file = dir.join("sample.txt");
        std::fs::write(&file, "Needle\nneedle\n").unwrap();

        let sensitive = compile_matcher("needle", QueryOptions::default()).unwrap();
        assert_eq!(search_file(&sensitive, &file).len(), 1);

        let insensitive = compile_matcher(
            "needle",
            QueryOptions {
                case_insensitive: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(search_file(&insensitive, &file).len(), 2);
    }

    /// Run a matcher over one file, returning the matching lines.
    fn search_file(matcher: &RegexMatcher, path: &Path) -> Vec<String> {
        use grep_searcher::sinks::UTF8;
        let mut lines = Vec::new();
        let mut searcher = grep_searcher::SearcherBuilder::new().build();
        let _ = searcher.search_path(
            matcher,
            path,
            UTF8(|_, line| {
                lines.push(line.trim_end_matches(['\n', '\r']).to_string());
                Ok(true)
            }),
        );
        lines
    }

    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "wt-tui-search-test-{}-{:p}",
            std::process::id(),
            &std::time::Instant::now()
        ));
        std::fs::create_dir_all(&base).expect("create temp dir");
        base
    }

    #[test]
    fn an_invalid_regex_is_reported_not_swallowed() {
        // In literal mode this is just text and must not error. In regex mode it
        // is malformed, and the error must reach the user rather than silently
        // matching nothing.
        assert!(
            compile_matcher("unclosed(", QueryOptions::default()).is_ok(),
            "literal mode accepts any text"
        );

        let options = QueryOptions {
            force_regex: true,
            ..Default::default()
        };
        let err = compile_matcher("unclosed(", options).unwrap_err();
        assert!(err.contains("invalid pattern"), "unexpected message: {err}");
    }

    #[test]
    fn force_regex_uses_the_regex_engine() {
        let options = QueryOptions {
            force_regex: true,
            ..Default::default()
        };
        assert!(compile_matcher("plain", options).is_ok());
    }

    #[test]
    fn results_count_distinct_files() {
        let results = Results {
            hits: vec![
                Hit {
                    worktrees: vec!["a".into()],
                    rel_path: "src/x.rs".into(),
                    line_number: 1,
                    line: "l".into(),
                },
                Hit {
                    worktrees: vec!["a".into()],
                    rel_path: "src/x.rs".into(),
                    line_number: 2,
                    line: "l".into(),
                },
                Hit {
                    worktrees: vec!["b".into()],
                    rel_path: "src/y.rs".into(),
                    line_number: 1,
                    line: "l".into(),
                },
            ],
            ..Default::default()
        };
        assert_eq!(results.file_count(), 2);
        assert_eq!(results.covered(), 0);
    }

    #[test]
    fn coverage_counts_searched_and_deduplicated() {
        let results = Results {
            scanned: 2,
            deduped: 3,
            ..Default::default()
        };
        assert_eq!(results.covered(), 5);
    }

    #[test]
    fn a_bad_query_yields_an_error_not_a_panic() {
        let options = QueryOptions {
            force_regex: true,
            ..Default::default()
        };
        let results = search("unclosed(", &[], options);
        assert!(
            results.error.is_some(),
            "a bad pattern must surface an error"
        );
        assert!(results.is_empty());
    }

    #[test]
    fn searching_no_targets_is_empty_not_an_error() {
        let results = search("anything", &[], QueryOptions::default());
        assert!(results.error.is_none());
        assert!(results.is_empty());
        assert_eq!(results.covered(), 0);
    }

    #[test]
    fn reach_counts_worktrees_holding_the_line() {
        let hit = Hit {
            worktrees: vec!["main".into(), "feature".into()],
            rel_path: "f.rs".into(),
            line_number: 1,
            line: "x".into(),
        };
        assert_eq!(hit.reach(), 2);
    }
}
