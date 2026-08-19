//! End-to-end `symbol_history` tests (phase 6, task 6.3).
//!
//! The suite pins the tool's whole value proposition: transitions only
//! (AC-19 reformat invisible, AC-20 move invisible), exact case-sensitive
//! matching (D-0005 rename = removed + introduced), in-memory historical
//! parsing (AC-37), window/boundary honesty, success-shaped unavailability,
//! and runtime isolation (NFR-10). Fixtures are hermetic git repositories
//! (cleared environment, fixed identities and dates, signing disabled).
//!
//! Tests in this file share a serialization mutex: AC-37 watches the
//! process temp directory for new entries, which only means something when
//! no sibling test is creating fixtures concurrently.

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tokio::sync::{Mutex, MutexGuard};

use code_graph_lang::LanguageRegistry;
use code_graph_lang_rust::RustParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::CodeGraphServer;
use code_graph_vcs::VcsRegistry;
use code_graph_vcs_git::GitProvider;
use common::{first_text, ok_json};
use tempfile::TempDir;

static SUITE_SERIALIZATION: Mutex<()> = Mutex::const_new(());

async fn suite_guard() -> MutexGuard<'static, ()> {
    SUITE_SERIALIZATION.lock().await
}

const AUTHOR: (&str, &str) = ("History Fixture", "history@code-graph.invalid");

struct GitFixture {
    dir: TempDir,
}

impl GitFixture {
    fn init() -> Self {
        let fixture = Self {
            dir: TempDir::new().expect("fixture tempdir"),
        };
        fixture.git(&["init", "--initial-branch", "main"], None);
        fixture
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn commit_file(&self, name: &str, contents: &str, message: &str, timestamp: &str) -> String {
        std::fs::write(self.path().join(name), contents).expect("write fixture file");
        self.git(&["add", "--all"], None);
        self.git(
            &["commit", "--no-gpg-sign", "--message", message],
            Some(timestamp),
        );
        self.rev_parse("HEAD")
    }

    fn rev_parse(&self, spec: &str) -> String {
        String::from_utf8(self.git(&["rev-parse", spec], None).stdout)
            .expect("rev-parse output is UTF-8")
            .trim()
            .to_owned()
    }

    fn git(&self, args: &[&str], timestamp: Option<&str>) -> Output {
        let path =
            std::env::var_os("PATH").unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".into());
        let mut command = Command::new("git");
        command
            .env_clear()
            .current_dir(self.path())
            .env("PATH", path)
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.path().join("no-global.gitconfig"))
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", AUTHOR.0)
            .env("GIT_AUTHOR_EMAIL", AUTHOR.1)
            .env("GIT_COMMITTER_NAME", AUTHOR.0)
            .env("GIT_COMMITTER_EMAIL", AUTHOR.1)
            .args(args);
        if let Some(timestamp) = timestamp {
            command
                .env("GIT_AUTHOR_DATE", timestamp)
                .env("GIT_COMMITTER_DATE", timestamp);
        }
        let output = command.output().expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
}

/// The five-commit main fixture: only c1 (introduction) and c3 (logic
/// change) are content transitions for `target_function`; c2 is a
/// reformat, c4 a move within the file, c5 an unrelated change in the
/// same file.
fn transition_fixture() -> (GitFixture, [String; 5]) {
    let fixture = GitFixture::init();
    let c1 = fixture.commit_file(
        "lib.rs",
        "pub fn target_function(left: u32, right: u32) -> u32 {\n    left + right\n}\n\npub fn other_function() -> u32 {\n    7\n}\n",
        "introduce target",
        "2001-01-01T00:00:00+0000",
    );
    let c2 = fixture.commit_file(
        "lib.rs",
        "pub fn target_function(\n    left: u32,\n    right: u32\n) -> u32 {\n        left + right\n}\n\npub fn other_function() -> u32 {\n    7\n}\n",
        "reformat target only",
        "2001-02-01T00:00:00+0000",
    );
    let c3 = fixture.commit_file(
        "lib.rs",
        "pub fn target_function(\n    left: u32,\n    right: u32\n) -> u32 {\n        left + right + 1\n}\n\npub fn other_function() -> u32 {\n    7\n}\n",
        "change target logic",
        "2001-03-01T00:00:00+0000",
    );
    let c4 = fixture.commit_file(
        "lib.rs",
        "pub fn other_function() -> u32 {\n    7\n}\n\npub fn target_function(\n    left: u32,\n    right: u32\n) -> u32 {\n        left + right + 1\n}\n",
        "move target within file",
        "2001-04-01T00:00:00+0000",
    );
    let c5 = fixture.commit_file(
        "lib.rs",
        "pub fn other_function() -> u32 {\n    8\n}\n\npub fn target_function(\n    left: u32,\n    right: u32\n) -> u32 {\n        left + right + 1\n}\n",
        "change the other function",
        "2001-05-01T00:00:00+0000",
    );
    (fixture, [c1, c2, c3, c4, c5])
}

fn rust_server(vcs: VcsRegistry) -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(RustParser::new().expect("RustParser::new")))
        .unwrap();
    CodeGraphServer::with_vcs_registry(registry, vcs)
}

fn git_backed_server(root: &Path) -> CodeGraphServer {
    let mut vcs = VcsRegistry::new();
    vcs.register(Box::new(
        GitProvider::open(root).expect("fixture is a git working tree"),
    ))
    .unwrap();
    rust_server(vcs)
}

async fn analyze(server: &CodeGraphServer, root: &Path) {
    let r = analyze_codebase(
        server.inner.clone(),
        root.to_string_lossy().into_owned(),
        true,
        None,
        None,
    )
    .await;
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "analyze_codebase failed: {}",
        first_text(&r)
    );
}

fn symbol_id(root: &Path, file: &str, name: &str) -> String {
    let root = code_graph_core::paths::canonicalize(root).expect("canonicalize fixture root");
    format!("{}:{name}", root.join(file).to_string_lossy())
}

async fn call_history(
    server: &CodeGraphServer,
    symbol: &str,
    mode: Option<&str>,
    window: Option<u32>,
) -> rmcp::model::CallToolResult {
    code_graph_tools::handlers::history::symbol_history(&server.inner, symbol, mode, window).await
}

fn changes(body: &serde_json::Value) -> Vec<(String, String)> {
    body["entries"]
        .as_array()
        .expect("entries array")
        .iter()
        .map(|entry| {
            (
                entry["change"].as_str().expect("change").to_string(),
                entry["rev"].as_str().expect("rev").to_string(),
            )
        })
        .collect()
}

/// AC-19 + AC-20 + the trap: reformat, move, and unrelated-change commits
/// touch the file but are NOT transitions; only introduction and the logic
/// change are, oldest first.
#[tokio::test]
async fn symbol_history_reports_only_content_transitions() {
    let _guard = suite_guard().await;
    let (fixture, [c1, _c2, c3, _c4, _c5]) = transition_fixture();
    let server = git_backed_server(fixture.path());
    analyze(&server, fixture.path()).await;
    let symbol = symbol_id(fixture.path(), "lib.rs", "target_function");

    let body = ok_json(&call_history(&server, &symbol, None, None).await);
    assert_eq!(body["available"], serde_json::json!(true));
    assert_eq!(
        changes(&body),
        vec![("introduced".to_string(), c1), ("modified".to_string(), c3),],
        "reformat (AC-19), move (AC-20), and unrelated commits are invisible: {body}"
    );
    assert_eq!(body["revisions_examined"], serde_json::json!(5));
    assert_eq!(body["window_filled"], serde_json::json!(false));
    assert_eq!(body["history_truncated"], serde_json::json!(false));
    assert!(body["skipped"].as_array().unwrap().is_empty());
    assert_eq!(
        body["entries"][0]["at_window_boundary"],
        serde_json::json!(false),
        "an unfilled window saw the whole history — the introduction is genuine"
    );
    assert_eq!(body["mode"], serde_json::json!("normalized"));
    assert_eq!(body["window"], serde_json::json!(50));
}

/// AC-37: historical bytes are parsed in memory. No entry may appear in the
/// process temp directory during a cold walk (the suite mutex keeps sibling
/// fixtures from creating tempdirs concurrently).
#[tokio::test]
async fn symbol_history_writes_no_temporary_files() {
    let _guard = suite_guard().await;
    let (fixture, _) = transition_fixture();
    let server = git_backed_server(fixture.path());
    analyze(&server, fixture.path()).await;
    let symbol = symbol_id(fixture.path(), "lib.rs", "target_function");

    let temp_dir = std::env::temp_dir();
    let listing = |exclude: &Path| -> BTreeSet<PathBuf> {
        std::fs::read_dir(&temp_dir)
            .expect("list temp dir")
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| !p.starts_with(exclude))
            .collect()
    };
    let before = listing(fixture.path());
    let body = ok_json(&call_history(&server, &symbol, None, None).await);
    let after = listing(fixture.path());
    assert_eq!(body["available"], serde_json::json!(true));
    assert_eq!(
        after.difference(&before).collect::<Vec<_>>(),
        Vec::<&PathBuf>::new(),
        "the walk parses revision bytes in memory — no temp files"
    );
}

/// A symbol deleted and later reintroduced reports the full
/// introduced/removed/introduced arc.
#[tokio::test]
async fn symbol_history_reports_removed_and_reintroduced() {
    let _guard = suite_guard().await;
    let fixture = GitFixture::init();
    let c1 = fixture.commit_file(
        "lib.rs",
        "pub fn target_function() -> u32 {\n    1\n}\n",
        "introduce",
        "2001-01-01T00:00:00+0000",
    );
    let c2 = fixture.commit_file(
        "lib.rs",
        "// gone for now\n",
        "remove",
        "2001-02-01T00:00:00+0000",
    );
    let c3 = fixture.commit_file(
        "lib.rs",
        "pub fn target_function() -> u32 {\n    2\n}\n",
        "reintroduce",
        "2001-03-01T00:00:00+0000",
    );
    let server = git_backed_server(fixture.path());
    analyze(&server, fixture.path()).await;
    let symbol = symbol_id(fixture.path(), "lib.rs", "target_function");

    let body = ok_json(&call_history(&server, &symbol, None, None).await);
    assert_eq!(
        changes(&body),
        vec![
            ("introduced".to_string(), c1),
            ("removed".to_string(), c2),
            ("introduced".to_string(), c3),
        ]
    );
}

/// D-0005: a case-only rename is a different symbol — the new name's
/// history starts at the rename, and the old name is simply gone from the
/// graph (a did-you-mean tool error, not an interleaved history).
#[tokio::test]
async fn symbol_history_case_only_rename_is_a_new_symbol() {
    let _guard = suite_guard().await;
    let fixture = GitFixture::init();
    fixture.commit_file(
        "lib.rs",
        "pub fn foo_bar() -> u32 {\n    1\n}\n",
        "introduce lowercase",
        "2001-01-01T00:00:00+0000",
    );
    let c2 = fixture.commit_file(
        "lib.rs",
        "pub fn Foo_bar() -> u32 {\n    1\n}\n",
        "case-only rename",
        "2001-02-01T00:00:00+0000",
    );
    let server = git_backed_server(fixture.path());
    analyze(&server, fixture.path()).await;

    let renamed = symbol_id(fixture.path(), "lib.rs", "Foo_bar");
    let body = ok_json(&call_history(&server, &renamed, None, None).await);
    assert_eq!(
        changes(&body),
        vec![("introduced".to_string(), c2)],
        "the renamed symbol's history starts at the rename — never 'modified': {body}"
    );

    let old = symbol_id(fixture.path(), "lib.rs", "foo_bar");
    let r = call_history(&server, &old, None, None).await;
    assert_eq!(
        r.is_error,
        Some(true),
        "the pre-rename name no longer exists in the graph"
    );
}

/// The window bound is honest: a too-small window flags both the fill and
/// the presence-at-boundary, and the requested value is clamped + echoed.
#[tokio::test]
async fn symbol_history_window_boundary_is_labelled() {
    let _guard = suite_guard().await;
    let (fixture, [_c1, _c2, _c3, c4, _c5]) = transition_fixture();
    let server = git_backed_server(fixture.path());
    analyze(&server, fixture.path()).await;
    let symbol = symbol_id(fixture.path(), "lib.rs", "target_function");

    let body = ok_json(&call_history(&server, &symbol, None, Some(2)).await);
    assert_eq!(body["window"], serde_json::json!(2));
    assert_eq!(body["window_filled"], serde_json::json!(true));
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "no transition within the window: {body}");
    assert_eq!(entries[0]["change"], serde_json::json!("introduced"));
    assert_eq!(entries[0]["rev"], serde_json::json!(c4));
    assert_eq!(
        entries[0]["at_window_boundary"],
        serde_json::json!(true),
        "presence at the window's oldest revision is not a genuine introduction"
    );

    let clamped = ok_json(&call_history(&server, &symbol, None, Some(9_999)).await);
    assert_eq!(
        clamped["window"],
        serde_json::json!(500),
        "requests clamp to the ceiling and echo the resolved value"
    );
}

/// FR-36: no VCS and no committed history are SUCCESS shapes; other tools
/// keep working.
#[tokio::test]
async fn symbol_history_unavailability_is_success_shaped() {
    let _guard = suite_guard().await;
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("lib.rs"),
        "pub fn target_function() -> u32 {\n    1\n}\n",
    )
    .unwrap();
    let server = rust_server(VcsRegistry::new());
    analyze(&server, dir.path()).await;
    let symbol = symbol_id(dir.path(), "lib.rs", "target_function");

    let body = ok_json(&call_history(&server, &symbol, None, None).await);
    assert_eq!(body["available"], serde_json::json!(false));
    assert!(
        body["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("no supported version-control system")),
        "reason names the cause: {body}"
    );

    // Untracked file inside a real repository: distinct reason.
    let fixture = GitFixture::init();
    fixture.commit_file(
        "committed.rs",
        "pub fn anchored() -> u32 {\n    1\n}\n",
        "anchor",
        "2001-01-01T00:00:00+0000",
    );
    std::fs::write(
        fixture.path().join("untracked.rs"),
        "pub fn floating() -> u32 {\n    2\n}\n",
    )
    .unwrap();
    let server = git_backed_server(fixture.path());
    analyze(&server, fixture.path()).await;
    let untracked = symbol_id(fixture.path(), "untracked.rs", "floating");
    let body = ok_json(&call_history(&server, &untracked, None, None).await);
    assert_eq!(body["available"], serde_json::json!(false));
    assert!(
        body["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("no committed history")),
        "untracked is distinct from no-VCS: {body}"
    );
}

/// Unknown and unsupported modes are tool errors that name the way out.
#[tokio::test]
async fn symbol_history_mode_errors_name_the_supported_spelling() {
    let _guard = suite_guard().await;
    let (fixture, _) = transition_fixture();
    let server = git_backed_server(fixture.path());
    analyze(&server, fixture.path()).await;
    let symbol = symbol_id(fixture.path(), "lib.rs", "target_function");

    let bogus = call_history(&server, &symbol, Some("bogus"), None).await;
    assert_eq!(bogus.is_error, Some(true));
    assert!(
        first_text(&bogus).contains("\"normalized\""),
        "unknown mode names the supported spellings: {}",
        first_text(&bogus)
    );

    let unsupported = call_history(&server, &symbol, Some("literal_insensitive"), None).await;
    assert_eq!(unsupported.is_error, Some(true));
    let message = first_text(&unsupported);
    assert!(
        message.contains("not") && message.contains("supported") && message.contains("normalized"),
        "the unsupported mode is reported, never silently downgraded: {message}"
    );
}

/// FR-37/AC-46 at the tool level: the first walk populates the sidecar,
/// including tombstones for pre-introduction revisions, and a second walk
/// returns byte-identical results.
#[tokio::test]
async fn symbol_history_cache_populates_and_second_walk_matches() {
    let _guard = suite_guard().await;
    let fixture = GitFixture::init();
    fixture.commit_file(
        "lib.rs",
        "pub fn early_bird() -> u32 {\n    0\n}\n",
        "before the symbol exists",
        "2001-01-01T00:00:00+0000",
    );
    fixture.commit_file(
        "lib.rs",
        "pub fn early_bird() -> u32 {\n    0\n}\n\npub fn target_function() -> u32 {\n    1\n}\n",
        "introduce target later",
        "2001-02-01T00:00:00+0000",
    );
    let server = git_backed_server(fixture.path());
    analyze(&server, fixture.path()).await;
    let symbol = symbol_id(fixture.path(), "lib.rs", "target_function");

    let first = ok_json(&call_history(&server, &symbol, None, None).await);
    let cache_dir = code_graph_core::paths::canonicalize(fixture.path())
        .unwrap()
        .join(".code-graph/fingerprints");
    assert!(
        cache_dir
            .read_dir()
            .map(|mut d| d.next().is_some())
            .unwrap_or(false),
        "the walk populated the fingerprint sidecar (including the tombstone \
         for the pre-introduction revision)"
    );

    let second = ok_json(&call_history(&server, &symbol, None, None).await);
    assert_eq!(first, second, "a cache-served walk is answer-identical");
}

/// Historical extraction runs the indexer's config pipeline: a class that
/// only parses under `[cpp].macro_strip` has a real history instead of a
/// silent all-tombstone "no history" (gate artifact 18, finding M1).
#[tokio::test]
async fn symbol_history_macro_config_symbols_have_real_history() {
    let _guard = suite_guard().await;
    let fixture = GitFixture::init();
    std::fs::write(
        fixture.path().join(".code-graph.toml"),
        "[cpp]\nmacro_strip = [\"CORE_API\"]\n",
    )
    .expect("write fixture config");
    let c1 = fixture.commit_file(
        "widget.h",
        "class CORE_API Widget {\npublic:\n    int size() const { return 1; }\n};\n",
        "introduce macro-prefixed class",
        "2001-01-01T00:00:00+0000",
    );
    let c2 = fixture.commit_file(
        "widget.h",
        "class CORE_API Widget {\npublic:\n    int size() const { return 2; }\n};\n",
        "change the class body",
        "2001-02-01T00:00:00+0000",
    );

    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(
            code_graph_lang_cpp::CppParser::new().expect("CppParser::new"),
        ))
        .unwrap();
    let mut vcs = VcsRegistry::new();
    vcs.register(Box::new(
        GitProvider::open(fixture.path()).expect("fixture is a git working tree"),
    ))
    .unwrap();
    let server = CodeGraphServer::with_vcs_registry(registry, vcs);
    analyze(&server, fixture.path()).await;
    let symbol = symbol_id(fixture.path(), "widget.h", "Widget");

    let body = ok_json(&call_history(&server, &symbol, None, None).await);
    assert_eq!(body["available"], serde_json::json!(true));
    assert!(
        body["skipped"].as_array().unwrap().is_empty(),
        "macro-stripped revisions parse, they are not skips: {body}"
    );
    assert_eq!(
        changes(&body),
        vec![("introduced".to_string(), c1), ("modified".to_string(), c2)],
        "a [cpp].macro_strip-dependent symbol has a real history: {body}"
    );
}

/// A commit that DELETES the file is an examined absence driving `removed`
/// — not a skip (gate artifact 18 follow-through on the deletion arc).
#[tokio::test]
async fn symbol_history_file_deletion_reports_removed() {
    let _guard = suite_guard().await;
    let fixture = GitFixture::init();
    let c1 = fixture.commit_file(
        "lib.rs",
        "pub fn target_function() -> u32 {\n    1\n}\n",
        "introduce",
        "2001-01-01T00:00:00+0000",
    );
    fixture.git(&["rm", "lib.rs"], None);
    fixture.git(
        &["commit", "--no-gpg-sign", "--message", "delete the file"],
        Some("2001-02-01T00:00:00+0000"),
    );
    let c2 = fixture.rev_parse("HEAD");
    let c3 = fixture.commit_file(
        "lib.rs",
        "pub fn target_function() -> u32 {\n    2\n}\n",
        "restore the file",
        "2001-03-01T00:00:00+0000",
    );
    let server = git_backed_server(fixture.path());
    analyze(&server, fixture.path()).await;
    let symbol = symbol_id(fixture.path(), "lib.rs", "target_function");

    let body = ok_json(&call_history(&server, &symbol, None, None).await);
    assert!(
        body["skipped"].as_array().unwrap().is_empty(),
        "a deletion commit is examined absence, never a skip: {body}"
    );
    assert_eq!(
        changes(&body),
        vec![
            ("introduced".to_string(), c1),
            ("removed".to_string(), c2),
            ("introduced".to_string(), c3),
        ],
        "the deletion commit itself carries the removal: {body}"
    );
}

/// When every revision older than the first examined one was skipped, an
/// `introduced` there carries the same boundary uncertainty as a filled
/// window — `at_window_boundary` says so (gate artifact 18 boundary fix).
#[tokio::test]
async fn symbol_history_skipped_oldest_marks_boundary() {
    use code_graph_vcs::{BlameHunk, Commit, RevId, RevisionWindow, VcsError, VcsProvider};

    struct SkipOldestProvider;

    #[async_trait::async_trait]
    impl VcsProvider for SkipOldestProvider {
        fn id(&self) -> &'static str {
            "skip-oldest"
        }
        fn detect(&self, _working_tree: &Path) -> bool {
            true
        }
        async fn blame(
            &self,
            _path: &Path,
            _lines: Option<(u32, u32)>,
            _at: Option<&RevId>,
        ) -> Result<Vec<BlameHunk>, VcsError> {
            Ok(Vec::new())
        }
        async fn revisions_touching(
            &self,
            _path: &Path,
            _limit: u32,
        ) -> Result<RevisionWindow, VcsError> {
            // Newest first, per the trait contract.
            Ok(RevisionWindow {
                commits: vec![
                    Commit {
                        rev: RevId::new("rev-new"),
                        author: "fixture".to_string(),
                        timestamp_utc: 1,
                        summary: "readable".to_string(),
                    },
                    Commit {
                        rev: RevId::new("rev-old"),
                        author: "fixture".to_string(),
                        timestamp_utc: 0,
                        summary: "unreadable".to_string(),
                    },
                ],
                truncated: false,
            })
        }
        async fn read_at(&self, rev: &RevId, _path: &Path) -> Result<Vec<u8>, VcsError> {
            if rev.as_str() == "rev-old" {
                return Err(VcsError::Operation("simulated unreadable blob".to_string()));
            }
            Ok(b"pub fn target_function() -> u32 {\n    1\n}\n".to_vec())
        }
        async fn resolve_rev(&self, spec: &str) -> Result<RevId, VcsError> {
            Ok(RevId::new(spec))
        }
    }

    let _guard = suite_guard().await;
    let mut vcs = VcsRegistry::new();
    vcs.register(Box::new(SkipOldestProvider)).unwrap();
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("lib.rs"),
        "pub fn target_function() -> u32 {\n    1\n}\n",
    )
    .unwrap();
    let server = rust_server(vcs);
    analyze(&server, dir.path()).await;
    let symbol = symbol_id(dir.path(), "lib.rs", "target_function");

    let body = ok_json(&call_history(&server, &symbol, None, None).await);
    assert_eq!(body["window_filled"], serde_json::json!(false));
    assert_eq!(body["history_truncated"], serde_json::json!(false));
    assert_eq!(
        body["skipped"].as_array().unwrap().len(),
        1,
        "the unreadable oldest revision is flagged: {body}"
    );
    assert_eq!(
        changes(&body),
        vec![("introduced".to_string(), "rev-new".to_string())]
    );
    assert_eq!(
        body["entries"][0]["at_window_boundary"],
        serde_json::json!(true),
        "a skipped-away oldest revision leaves the introduction boundary-ambiguous: {body}"
    );
}

/// NFR-10: a hung provider delays only the history tool; an unrelated query
/// completes while the walk is gated.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn symbol_history_slow_provider_delays_only_history_tools() {
    use code_graph_vcs::{BlameHunk, Commit, RevId, RevisionWindow, VcsError, VcsProvider};
    use std::sync::Arc;

    struct GatedProvider {
        started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl VcsProvider for GatedProvider {
        fn id(&self) -> &'static str {
            "gated-history"
        }
        fn detect(&self, _working_tree: &Path) -> bool {
            true
        }
        async fn blame(
            &self,
            _path: &Path,
            _lines: Option<(u32, u32)>,
            _at: Option<&RevId>,
        ) -> Result<Vec<BlameHunk>, VcsError> {
            Ok(Vec::new())
        }
        async fn revisions_touching(
            &self,
            _path: &Path,
            _limit: u32,
        ) -> Result<RevisionWindow, VcsError> {
            self.started.notify_one();
            self.release.notified().await;
            Ok(RevisionWindow {
                commits: vec![Commit {
                    rev: RevId::new("gated-rev"),
                    author: "gated".to_string(),
                    timestamp_utc: 0,
                    summary: "gated".to_string(),
                }],
                truncated: false,
            })
        }
        async fn read_at(&self, _rev: &RevId, _path: &Path) -> Result<Vec<u8>, VcsError> {
            Ok(b"pub fn target_function() -> u32 {\n    1\n}\n".to_vec())
        }
        async fn resolve_rev(&self, spec: &str) -> Result<RevId, VcsError> {
            Ok(RevId::new(spec))
        }
    }

    let _guard = suite_guard().await;
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut vcs = VcsRegistry::new();
    vcs.register(Box::new(GatedProvider {
        started: Arc::clone(&started),
        release: Arc::clone(&release),
    }))
    .unwrap();

    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("lib.rs"),
        "pub fn target_function() -> u32 {\n    1\n}\n",
    )
    .unwrap();
    let server = rust_server(vcs);
    analyze(&server, dir.path()).await;
    let symbol = symbol_id(dir.path(), "lib.rs", "target_function");
    let file = code_graph_core::paths::canonicalize(dir.path())
        .unwrap()
        .join("lib.rs");

    let inner = server.inner.clone();
    let symbol_for_walk = symbol.clone();
    let walk = tokio::spawn(async move {
        code_graph_tools::handlers::history::symbol_history(&inner, &symbol_for_walk, None, None)
            .await
    });
    started.notified().await;
    assert!(!walk.is_finished(), "the walk is held at the provider gate");

    let symbols = code_graph_tools::handlers::symbols::get_file_symbols(
        &server.inner.graph,
        &file.to_string_lossy(),
        false,
        true,
        None,
        None,
        false,
        usize::MAX,
    );
    assert_eq!(ok_json(&symbols)["total"], serde_json::json!(1));
    assert!(
        !walk.is_finished(),
        "the unrelated query finished first; history alone is delayed"
    );

    release.notify_one();
    let body = ok_json(&walk.await.expect("walk task joins"));
    assert_eq!(body["available"], serde_json::json!(true));
    assert_eq!(
        changes(&body),
        vec![("introduced".to_string(), "gated-rev".to_string())]
    );
}
