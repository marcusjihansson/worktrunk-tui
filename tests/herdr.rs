//! Herdr's pane entrypoint must bridge Worktrunk's shell directive into a
//! Herdr worktree focus operation.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

struct Harness {
    root: TempDir,
    repo: PathBuf,
    herdr_log: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let repo = root.path().join("repo");
        let plugin = root.path().join("plugin");
        let binary = plugin.join("target/release/wt-tui");
        let fake_herdr = root.path().join("fake-herdr");
        let herdr_log = root.path().join("herdr.log");

        fs::create_dir_all(binary.parent().expect("binary parent")).expect("plugin dirs");
        fs::create_dir_all(&repo).expect("repo");

        write_executable(
            &binary,
            "#!/bin/sh\nif [ -n \"${WRITE_TARGET:-}\" ]; then printf '%s\\n' \"$WRITE_TARGET\" > \"$WORKTRUNK_DIRECTIVE_CD_FILE\"; fi\nexit \"${WT_TUI_STATUS:-0}\"\n",
        );
        write_executable(
            &fake_herdr,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HERDR_TEST_LOG\"\nexit 0\n",
        );

        // run.sh locates the entrypoint relative to HERDR_PLUGIN_ROOT.
        let herdr_dir = plugin.join("herdr");
        fs::create_dir_all(&herdr_dir).expect("herdr dir");
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("herdr/run.sh"),
            herdr_dir.join("run.sh"),
        )
        .expect("copy run.sh");

        Self {
            root,
            repo,
            herdr_log,
        }
    }

    fn run(&self, target: Option<&str>, wt_status: &str) -> Output {
        let plugin = self.root.path().join("plugin");
        let fake_herdr = self.root.path().join("fake-herdr");
        let mut command = Command::new("sh");
        command
            .arg(plugin.join("herdr/run.sh"))
            .env("HERDR_PLUGIN_ROOT", &plugin)
            .env("HERDR_BIN_PATH", fake_herdr)
            .env("HERDR_TEST_LOG", &self.herdr_log)
            .env("TMPDIR", self.root.path())
            .env("WT_TUI_REPO", &self.repo)
            .env("WT_TUI_STATUS", wt_status);
        if let Some(target) = target {
            command.env("WRITE_TARGET", target);
        } else {
            command.env_remove("WRITE_TARGET");
        }
        command.output().expect("run run.sh")
    }

    fn directive_files(&self) -> Vec<PathBuf> {
        fs::read_dir(self.root.path())
            .expect("read harness root")
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("wt-tui-cd."))
            })
            .collect()
    }
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).expect("write executable");
    let mut permissions = fs::metadata(path)
        .expect("executable metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("set executable permissions");
}

#[test]
fn a_switch_directive_focuses_the_selected_worktree_in_herdr() {
    let harness = Harness::new();
    let output = harness.run(Some("/tmp/repo.feature"), "0");

    assert!(output.status.success(), "run.sh failed: {output:?}");
    let args = fs::read_to_string(&harness.herdr_log).expect("Herdr handoff log");
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        vec![
            "worktree",
            "open",
            "--cwd",
            harness.repo.to_str().expect("repo path"),
            "--path",
            "/tmp/repo.feature",
            "--focus",
            "--trust-repository",
        ]
    );
    assert!(
        harness.directive_files().is_empty(),
        "the handoff directive must be cleaned up"
    );
}

#[test]
fn quitting_without_a_switch_does_not_open_a_worktree() {
    let harness = Harness::new();
    let output = harness.run(None, "0");

    assert!(output.status.success(), "run.sh failed: {output:?}");
    assert!(
        !harness.herdr_log.exists(),
        "quitting the dashboard must not trigger a Herdr handoff"
    );
}

#[test]
fn a_failed_dashboard_does_not_focus_a_worktree() {
    let harness = Harness::new();
    let output = harness.run(Some("/tmp/repo.feature"), "17");

    assert_eq!(output.status.code(), Some(17));
    assert!(
        !harness.herdr_log.exists(),
        "a failed dashboard must not trigger a Herdr handoff"
    );
}
