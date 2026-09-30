//! Time a cross-worktree search against a real repository.
//!
//! Usage: `cargo run --release --example search_bench -- <repo> <query>`
//!
//! Reports what deduplication saved, because that is the claim the design rests
//! on: worktrees sharing a tree should cost one walk, not one walk each.

use std::path::PathBuf;
use std::time::Instant;
use wt_tui::search::{QueryOptions, Target, search};
use wt_tui::wt::command::ListScope;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let repo = PathBuf::from(args.next().expect("usage: search_bench <repo> [query]"));
    let query = args.next().unwrap_or_else(|| "auth".to_string());

    // Targets come from worktrunk, exactly as the TUI builds them.
    let envelope = wt_tui::wt::command::list(&repo, ListScope::WITH_BRANCHES)
        .await
        .expect("wt list");

    let targets: Vec<Target> = envelope
        .items
        .iter()
        .filter_map(|item| {
            let path = item.worktree.as_ref()?.path.as_ref()?;
            Some(Target {
                branch: item.label(),
                path: PathBuf::from(path),
            })
        })
        .collect();

    println!("repo: {}", repo.display());
    println!("worktrees: {}", targets.len());
    println!("query: {query:?}\n");

    // Warm the page cache so the numbers compare searches, not disk.
    for target in &targets {
        let _ = std::fs::read_dir(&target.path);
    }

    let started = Instant::now();
    let results = search(&query, &targets, QueryOptions::default());
    let elapsed = started.elapsed();

    println!("elapsed:        {:?}", elapsed);
    println!("trees searched: {}", results.scanned);
    println!("deduplicated:   {}", results.deduped);
    println!("files matched:  {}", results.files_matched);
    println!("hits:           {}", results.hits.len());
    println!("distinct files: {}", results.file_count());
    if results.truncated {
        println!("(truncated at the hit cap)");
    }
    if let Some(error) = results.error {
        println!("error: {error}");
    }

    // Show the widest-reaching hits, which is where deduplication shows up.
    // What the same search would cost without deduplication, so the saving is
    // a measured number rather than a claim.
    let naive_started = Instant::now();
    let mut naive_hits = 0usize;
    for target in &targets {
        let mut walker = ignore::WalkBuilder::new(&target.path);
        walker
            .hidden(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .parents(true);
        walker.filter_entry(|entry| entry.file_name() != ".git");
        for entry in walker.build().flatten() {
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(entry.path()) {
                naive_hits += text.lines().filter(|l| l.contains(&query)).count();
            }
        }
    }
    let naive = naive_started.elapsed();

    println!("\nwithout dedup:");
    println!("  elapsed:      {naive:?}");
    println!("  trees walked: {}", targets.len());
    println!("  matching lines: {naive_hits}");
    if naive > elapsed {
        println!(
            "  speedup:       {:.1}x",
            naive.as_secs_f64() / elapsed.as_secs_f64().max(0.000001)
        );
    }

    let mut by_reach: Vec<_> = results.hits.iter().collect();
    by_reach.sort_by_key(|h| std::cmp::Reverse(h.reach()));
    println!("\nwidest reach:");
    for hit in by_reach.iter().take(5) {
        println!(
            "  {:>6}x  {}:{}  {}",
            hit.reach(),
            hit.rel_path,
            hit.line_number,
            hit.line.trim()
        );
        if hit.reach() > 1 {
            println!("          in: {}", hit.worktrees.join(", "));
        }
    }
}
