#![forbid(unsafe_code)]

//! Git VCS provider implementation.
//!
//! The provider is introduced separately from its test fixture harness. The
//! harness below is compiled only for this crate's tests so Git CLI use cannot
//! enter production code.

#[cfg(test)]
mod harness {
    use std::ffi::OsString;
    use std::fmt;
    use std::fs;
    use std::path::Path;
    use std::process::{Command, Output};

    use tempfile::TempDir;

    const AUTHOR_NAME: &str = "Code Graph Fixture Author";
    const AUTHOR_EMAIL: &str = "fixture-author@code-graph.invalid";
    const COMMITTER_NAME: &str = "Code Graph Fixture Committer";
    const COMMITTER_EMAIL: &str = "fixture-committer@code-graph.invalid";
    const INITIAL_BRANCH: &str = "main";

    /// A source-file replacement made by one scripted commit.
    #[derive(Clone, Copy, Debug)]
    struct FileChange {
        path: &'static str,
        contents: &'static str,
    }

    /// One deterministic commit in a fixture history.
    #[derive(Clone, Copy, Debug)]
    struct ScriptedCommit {
        message: &'static str,
        timestamp: &'static str,
        changes: &'static [FileChange],
    }

    /// A deterministic source-history recipe for VCS tests.
    #[derive(Clone, Copy, Debug)]
    struct FixtureScript {
        commits: &'static [ScriptedCommit],
    }

    /// A temporary Git working tree built from a [`FixtureScript`].
    ///
    /// `TempDir` owns the directory, including during stack unwinding, so the
    /// working tree is removed on success and on a panic in a test.
    struct Fixture {
        directory: TempDir,
    }

    #[derive(Debug)]
    struct HarnessError(String);

    impl fmt::Display for HarnessError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(&self.0)
        }
    }

    impl std::error::Error for HarnessError {}

    impl Fixture {
        fn build(script: FixtureScript) -> Result<Self, HarnessError> {
            let directory = tempfile::tempdir().map_err(|error| {
                HarnessError(format!("create temporary Git fixture directory: {error}"))
            })?;
            let fixture = Self { directory };

            fs::create_dir(fixture.path().join("fixture-template")).map_err(|error| {
                HarnessError(format!("create empty Git template directory: {error}"))
            })?;
            fs::create_dir(fixture.path().join("fixture-hooks")).map_err(|error| {
                HarnessError(format!("create empty Git hooks directory: {error}"))
            })?;
            fs::write(fixture.path().join("fixture-attributes"), "").map_err(|error| {
                HarnessError(format!("create empty Git attributes file: {error}"))
            })?;

            fixture.git(&["init", "--initial-branch", INITIAL_BRANCH])?;
            fixture.git(&["config", "user.name", AUTHOR_NAME])?;
            fixture.git(&["config", "user.email", AUTHOR_EMAIL])?;
            fixture.git(&["config", "commit.gpgsign", "false"])?;

            for commit in script.commits {
                fixture.apply_commit(*commit)?;
            }

            Ok(fixture)
        }

        fn path(&self) -> &Path {
            self.directory.path()
        }

        fn apply_commit(&self, commit: ScriptedCommit) -> Result<(), HarnessError> {
            for change in commit.changes {
                let path = self.path().join(change.path);
                let parent = path.parent().ok_or_else(|| {
                    HarnessError(format!("fixture path has no parent: {}", change.path))
                })?;
                fs::create_dir_all(parent).map_err(|error| {
                    HarnessError(format!(
                        "create fixture directory {}: {error}",
                        parent.display()
                    ))
                })?;
                fs::write(&path, change.contents).map_err(|error| {
                    HarnessError(format!("write fixture file {}: {error}", path.display()))
                })?;
            }

            self.git(&["add", "--all"])?;
            self.git_with_commit_identity(
                &["commit", "--no-gpg-sign", "--message", commit.message],
                commit.timestamp,
            )?;
            Ok(())
        }

        fn commit_graph(&self) -> Result<String, HarnessError> {
            self.git_stdout(&["rev-list", "--reverse", "--parents", "HEAD"])
        }

        fn branch(&self) -> Result<String, HarnessError> {
            self.git_stdout(&["branch", "--show-current"])
                .map(|branch| branch.trim().to_string())
        }

        fn config(&self, key: &str) -> Result<String, HarnessError> {
            self.git_stdout(&["config", "--get", key])
                .map(|value| value.trim().to_string())
        }

        fn file_at_commit(&self, commit: &str, path: &str) -> Result<String, HarnessError> {
            let object = format!("{commit}:{path}");
            self.git_stdout(&["show", &object])
        }

        fn commit_ids(&self) -> Result<Vec<String>, HarnessError> {
            self.git_stdout(&["rev-list", "--reverse", "HEAD"])
                .map(|ids| ids.lines().map(str::to_owned).collect())
        }

        fn commit_metadata(&self) -> Result<String, HarnessError> {
            self.git_stdout(&[
                "log",
                "--reverse",
                "--format=%an%x1f%ae%x1f%aI%x1f%cn%x1f%ce%x1f%cI",
                "HEAD",
            ])
        }

        fn git(&self, args: &[&str]) -> Result<(), HarnessError> {
            self.git_output(args).map(|_| ())
        }

        fn git_stdout(&self, args: &[&str]) -> Result<String, HarnessError> {
            let output = self.git_output(args)?;
            String::from_utf8(output.stdout)
                .map_err(|error| HarnessError(format!("git output was not UTF-8: {error}")))
        }

        fn git_with_commit_identity(
            &self,
            args: &[&str],
            timestamp: &str,
        ) -> Result<(), HarnessError> {
            self.git_command(args)
                .env("GIT_AUTHOR_NAME", AUTHOR_NAME)
                .env("GIT_AUTHOR_EMAIL", AUTHOR_EMAIL)
                .env("GIT_AUTHOR_DATE", timestamp)
                .env("GIT_COMMITTER_NAME", COMMITTER_NAME)
                .env("GIT_COMMITTER_EMAIL", COMMITTER_EMAIL)
                .env("GIT_COMMITTER_DATE", timestamp)
                .output()
                .map_err(|error| HarnessError(format!("run git: {error}")))
                .and_then(|output| self.require_success(args, output))
                .map(|_| ())
        }

        fn git_output(&self, args: &[&str]) -> Result<Output, HarnessError> {
            self.git_command(args)
                .output()
                .map_err(|error| HarnessError(format!("run git: {error}")))
                .and_then(|output| self.require_success(args, output))
        }

        fn git_command(&self, args: &[&str]) -> Command {
            let empty_global_config = self.path().join("fixture-global.gitconfig");
            let empty_hooks = self.path().join("fixture-hooks");
            let empty_template = self.path().join("fixture-template");
            let empty_attributes = self.path().join("fixture-attributes");
            let hooks_path = format!("core.hooksPath={}", empty_hooks.display());
            let attributes_file = format!("core.attributesFile={}", empty_attributes.display());
            let path = std::env::var_os("PATH")
                .unwrap_or_else(|| OsString::from("/usr/local/bin:/usr/bin:/bin"));
            let mut command = Command::new("git");
            command
                .env_clear()
                .current_dir(self.path())
                .env("PATH", path)
                .env("LC_ALL", "C")
                .env("TZ", "UTC")
                .arg("-c")
                .arg(hooks_path)
                .arg("-c")
                .arg(attributes_file)
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", empty_global_config)
                .env("GIT_ATTR_NOSYSTEM", "1")
                .env("GIT_TEMPLATE_DIR", empty_template);
            command
        }

        fn require_success(&self, args: &[&str], output: Output) -> Result<Output, HarnessError> {
            if output.status.success() {
                return Ok(output);
            }

            Err(HarnessError(format!(
                "git {} failed with {}: {}",
                args.join(" "),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )))
        }
    }

    const INITIAL: ScriptedCommit = ScriptedCommit {
        message: "initial calculator",
        timestamp: "2001-01-01T00:00:00+0000",
        changes: &[FileChange {
            path: "src/calculator.rs",
            contents: "pub const DEFAULT_LIMIT: u32 = 10;\n\npub fn add(left: u32, right: u32) -> u32 {\n    left + right\n}\n\npub fn multiply(left: u32, right: u32) -> u32 {\n    left * right\n}\n",
        }],
    };

    const REFORMAT_ONLY: ScriptedCommit = ScriptedCommit {
        message: "reformat calculator",
        timestamp: "2001-01-02T00:00:00+0000",
        changes: &[FileChange {
            path: "src/calculator.rs",
            contents: "pub const DEFAULT_LIMIT: u32 = 10;\n\npub fn add(left: u32, right: u32) -> u32 {\n    left + right\n}\n\npub fn multiply(\n    left: u32,\n    right: u32\n) -> u32 {\n    left * right\n}\n",
        }],
    };

    const LOGIC_CHANGE: ScriptedCommit = ScriptedCommit {
        message: "adjust addition logic",
        timestamp: "2001-01-03T00:00:00+0000",
        changes: &[FileChange {
            path: "src/calculator.rs",
            contents: "pub const DEFAULT_LIMIT: u32 = 10;\n\npub fn add(left: u32, right: u32) -> u32 {\n    left + right + 1\n}\n\npub fn multiply(\n    left: u32,\n    right: u32\n) -> u32 {\n    left * right\n}\n",
        }],
    };

    const MOVE_WITHIN_FILE: ScriptedCommit = ScriptedCommit {
        message: "move multiply before add",
        timestamp: "2001-01-04T00:00:00+0000",
        changes: &[FileChange {
            path: "src/calculator.rs",
            contents: "pub const DEFAULT_LIMIT: u32 = 10;\n\npub fn multiply(\n    left: u32,\n    right: u32\n) -> u32 {\n    left * right\n}\n\npub fn add(left: u32, right: u32) -> u32 {\n    left + right + 1\n}\n",
        }],
    };

    const LITERAL_ONLY: ScriptedCommit = ScriptedCommit {
        message: "raise default limit",
        timestamp: "2001-01-05T00:00:00+0000",
        changes: &[FileChange {
            path: "src/calculator.rs",
            contents: "pub const DEFAULT_LIMIT: u32 = 20;\n\npub fn multiply(\n    left: u32,\n    right: u32\n) -> u32 {\n    left * right\n}\n\npub fn add(left: u32, right: u32) -> u32 {\n    left + right + 1\n}\n",
        }],
    };

    fn phase_six_script() -> FixtureScript {
        FixtureScript {
            commits: &[
                INITIAL,
                REFORMAT_ONLY,
                LOGIC_CHANGE,
                MOVE_WITHIN_FILE,
                LITERAL_ONLY,
            ],
        }
    }

    fn without_whitespace(source: &str) -> String {
        source
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect()
    }

    #[test]
    fn scripted_history_pins_repository_and_commit_identity() {
        let fixture = Fixture::build(phase_six_script()).expect("fixture must build");

        assert_eq!(fixture.branch().unwrap(), INITIAL_BRANCH);
        assert_eq!(fixture.config("user.name").unwrap(), AUTHOR_NAME);
        assert_eq!(fixture.config("user.email").unwrap(), AUTHOR_EMAIL);
        assert_eq!(fixture.config("commit.gpgsign").unwrap(), "false");
        assert_eq!(fixture.commit_ids().unwrap().len(), 5);

        for (index, metadata) in fixture.commit_metadata().unwrap().lines().enumerate() {
            let timestamp = format!("2001-01-0{}T00:00:00Z", index + 1);
            let fields: Vec<_> = metadata.split('\x1f').collect();
            assert_eq!(
                fields.as_slice(),
                [
                    AUTHOR_NAME,
                    AUTHOR_EMAIL,
                    timestamp.as_str(),
                    COMMITTER_NAME,
                    COMMITTER_EMAIL,
                    timestamp.as_str(),
                ]
            );
        }
    }

    #[test]
    fn scripted_history_contains_phase_six_change_shapes() {
        let fixture = Fixture::build(phase_six_script()).expect("fixture must build");
        let commits = fixture.commit_ids().unwrap();

        let initial = fixture
            .file_at_commit(&commits[0], "src/calculator.rs")
            .unwrap();
        let reformat = fixture
            .file_at_commit(&commits[1], "src/calculator.rs")
            .unwrap();
        let logic = fixture
            .file_at_commit(&commits[2], "src/calculator.rs")
            .unwrap();
        let moved = fixture
            .file_at_commit(&commits[3], "src/calculator.rs")
            .unwrap();
        let literal = fixture
            .file_at_commit(&commits[4], "src/calculator.rs")
            .unwrap();

        assert_eq!(without_whitespace(&initial), without_whitespace(&reformat));
        assert!(reformat.contains("pub fn multiply(\n"));
        assert!(logic.contains("left + right + 1"));
        assert!(
            moved.find("pub fn multiply").unwrap() < moved.find("pub fn add").unwrap(),
            "the move fixture must relocate multiply within the same file"
        );
        assert!(literal.contains("DEFAULT_LIMIT: u32 = 20"));
        assert_eq!(literal.replace("20", "10"), moved);
    }

    #[test]
    fn same_script_creates_identical_opaque_commit_graphs() {
        let first = Fixture::build(phase_six_script()).expect("first fixture must build");
        let second = Fixture::build(phase_six_script()).expect("second fixture must build");

        assert_eq!(
            first.commit_graph().unwrap(),
            second.commit_graph().unwrap()
        );
    }

    #[test]
    fn cleanup_removes_temp_repository_after_success_and_panic() {
        let successful_path = {
            let fixture = Fixture::build(phase_six_script()).expect("fixture must build");
            let path = fixture.path().to_path_buf();
            assert!(path.is_dir());
            path
        };
        assert!(!successful_path.exists());

        let fixture = Fixture::build(phase_six_script()).expect("fixture must build");
        let panic_path = fixture.path().to_path_buf();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _fixture = fixture;
            panic!("exercise panic cleanup");
        }));

        assert!(result.is_err());
        assert!(!panic_path.exists());
    }

    #[test]
    fn inherited_git_environment_cannot_redirect_fixture() {
        const CHILD_MARKER: &str = "CODE_GRAPH_GIT_HARNESS_CHILD";
        if std::env::var_os(CHILD_MARKER).is_some() {
            println!("CODE_GRAPH_GIT_HARNESS_CHILD_EXECUTED");
            let fixture = Fixture::build(phase_six_script())
                .expect("sanitized fixture must ignore inherited Git environment");
            assert_eq!(fixture.config("user.name").unwrap(), AUTHOR_NAME);
            assert_eq!(fixture.branch().unwrap(), INITIAL_BRANCH);
            return;
        }

        let redirect = tempfile::tempdir().expect("create external redirect directory");
        let output = Command::new(std::env::current_exe().expect("locate test executable"))
            .arg("--exact")
            .arg("harness::inherited_git_environment_cannot_redirect_fixture")
            .arg("--nocapture")
            .env(CHILD_MARKER, "1")
            .env("GIT_DIR", redirect.path().join("redirected-git"))
            .env("GIT_WORK_TREE", redirect.path())
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "user.name")
            .env("GIT_CONFIG_VALUE_0", "Injected Host Configuration")
            .output()
            .expect("spawn isolated test child");

        assert!(
            output.status.success(),
            "sanitized fixture child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains("CODE_GRAPH_GIT_HARNESS_CHILD_EXECUTED"),
            "sanitized fixture child did not execute the intended test: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            !redirect.path().join("redirected-git").exists(),
            "fixture must not create Git state outside its TempDir"
        );
    }
}
