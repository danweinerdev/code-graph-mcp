//! End-to-end `blame_symbol` tests (phase 5, task 5.4).
//!
//! AC-21 is oracle-based: per-line attribution is diffed against
//! `git blame --porcelain -L <start>,<end>` output rather than
//! hand-asserted SHAs, so the fixture can change without rotting the test.
//! The fixture repository is hermetic (cleared environment, explicit
//! identity, fixed timestamps, signing disabled), mirroring the
//! `code-graph-vcs-git` harness conventions.

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

use code_graph_lang::LanguageRegistry;
use code_graph_lang_rust::RustParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::CodeGraphServer;
use code_graph_vcs::{BlameHunk, Commit, RevId, VcsError, VcsProvider, VcsRegistry};
use code_graph_vcs_git::GitProvider;
use common::{first_text, ok_json};
use tempfile::TempDir;

const ALICE: (&str, &str) = ("Alice Fixture", "alice@code-graph.invalid");
const BOB: (&str, &str) = ("Bob Fixture", "bob@code-graph.invalid");

/// First revision: four lines, all Alice's.
const INITIAL: &str = "pub fn target_function() -> u32 {\n    let base = 1;\n    base + 2\n}\n";
/// Second revision: Bob rewrites the two body lines; the signature and the
/// closing brace stay Alice's, so the span blames to two authors.
const EDITED: &str = "pub fn target_function() -> u32 {\n    let base = 40;\n    base + 2 + 0\n}\n";

struct GitFixture {
    dir: TempDir,
}

impl GitFixture {
    /// Two-commit repository: `INITIAL` by Alice (2001), `EDITED` by Bob
    /// (2002).
    fn build() -> Self {
        let fixture = Self {
            dir: TempDir::new().expect("fixture tempdir"),
        };
        fixture.git(&["init", "--initial-branch", "main"], None);
        std::fs::write(fixture.path().join("lib.rs"), INITIAL).expect("write initial lib.rs");
        fixture.commit("initial function", "2001-01-01T00:00:00+0000", ALICE);
        std::fs::write(fixture.path().join("lib.rs"), EDITED).expect("write edited lib.rs");
        fixture.commit("edit function body", "2002-01-01T00:00:00+0000", BOB);
        fixture
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn commit(&self, message: &str, timestamp: &str, author: (&str, &str)) {
        self.git(&["add", "--all"], None);
        self.git(
            &["commit", "--no-gpg-sign", "--message", message],
            Some((author, timestamp)),
        );
    }

    fn rev_parse(&self, spec: &str) -> String {
        String::from_utf8(self.git(&["rev-parse", spec], None).stdout)
            .expect("rev-parse output is UTF-8")
            .trim()
            .to_owned()
    }

    /// Hermetic git invocation: cleared environment (PATH retained),
    /// no system/global config, fixed locale and timezone.
    fn git(&self, args: &[&str], identity: Option<((&str, &str), &str)>) -> Output {
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
            .args(args);
        if let Some(((name, email), timestamp)) = identity {
            command
                .env("GIT_AUTHOR_NAME", name)
                .env("GIT_AUTHOR_EMAIL", email)
                .env("GIT_AUTHOR_DATE", timestamp)
                .env("GIT_COMMITTER_NAME", name)
                .env("GIT_COMMITTER_EMAIL", email)
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

    /// `(final_line -> (sha, author))` from `git blame --porcelain`, the
    /// AC-21 oracle.
    fn porcelain_oracle(
        &self,
        revision: &str,
        start: u32,
        end: u32,
    ) -> HashMap<u32, (String, String)> {
        let output = self.git(
            &[
                "blame",
                "--porcelain",
                "-L",
                &format!("{start},{end}"),
                revision,
                "--",
                "lib.rs",
            ],
            None,
        );
        let text = String::from_utf8(output.stdout).expect("porcelain output is UTF-8");
        let mut authors: HashMap<String, String> = HashMap::new();
        let mut lines: Vec<(String, u32)> = Vec::new();
        let mut current = String::new();
        for line in text.lines() {
            if let Some(author) = line.strip_prefix("author ") {
                authors.insert(current.clone(), author.to_string());
                continue;
            }
            let mut fields = line.split(' ');
            let (Some(sha), Some(_orig), Some(final_line)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            if sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                if let Ok(final_line) = final_line.parse::<u32>() {
                    current = sha.to_string();
                    lines.push((current.clone(), final_line));
                }
            }
        }
        lines
            .into_iter()
            .map(|(sha, line)| {
                let author = authors.get(&sha).cloned().unwrap_or_default();
                (line, (sha, author))
            })
            .collect()
    }
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

/// The canonical fixture facts every test needs: the canonicalized root,
/// the absolute source path, and the symbol ID the graph assigned.
fn fixture_symbol(root: &Path) -> (PathBuf, PathBuf, String) {
    let root = code_graph_core::paths::canonicalize(root).expect("canonicalize fixture root");
    let file = root.join("lib.rs");
    let symbol = format!("{}:target_function", file.to_string_lossy());
    (root, file, symbol)
}

async fn call_blame(
    server: &CodeGraphServer,
    symbol: &str,
    at: Option<&str>,
) -> rmcp::model::CallToolResult {
    let root = server.inner.root_path.read().clone();
    code_graph_tools::handlers::history::blame_symbol(
        &server.inner.graph,
        &server.inner.vcs,
        root,
        symbol,
        at,
    )
    .await
}

/// Expand the response's hunks into a `final_line -> (rev, author)` map for
/// oracle comparison.
fn hunk_lines(body: &serde_json::Value) -> HashMap<u32, (String, String)> {
    let mut lines = HashMap::new();
    for hunk in body["hunks"].as_array().expect("hunks array") {
        let rev = hunk["rev"].as_str().expect("hunk rev").to_string();
        let author = hunk["author"].as_str().expect("hunk author").to_string();
        let start = hunk["start_line"].as_u64().expect("hunk start") as u32;
        let count = hunk["line_count"].as_u64().expect("hunk count") as u32;
        for line in start..start + count {
            let clobbered = lines.insert(line, (rev.clone(), author.clone()));
            assert!(clobbered.is_none(), "hunks must not overlap on line {line}");
        }
    }
    lines
}

#[tokio::test]
async fn blame_symbol_matches_git_blame_porcelain_oracle() {
    let fixture = GitFixture::build();
    let server = git_backed_server(fixture.path());
    analyze(&server, fixture.path()).await;
    let (_root, _file, symbol) = fixture_symbol(fixture.path());

    let body = ok_json(&call_blame(&server, &symbol, None).await);
    assert_eq!(body["available"], serde_json::json!(true));
    assert_eq!(
        body["stale"],
        serde_json::json!(false),
        "clean tree: {body}"
    );
    assert_eq!(body["rev"], serde_json::json!(fixture.rev_parse("HEAD")));
    let start = body["start_line"].as_u64().expect("start_line") as u32;
    let end = body["end_line"].as_u64().expect("end_line") as u32;
    assert!(end > start, "fixture symbol spans multiple lines");

    let ours = hunk_lines(&body);
    let oracle = fixture.porcelain_oracle("HEAD", start, end);
    assert_eq!(
        ours.len(),
        oracle.len(),
        "every span line is attributed exactly once; ours={ours:?} oracle={oracle:?}"
    );
    for (line, expected) in &oracle {
        assert_eq!(
            ours.get(line),
            Some(expected),
            "line {line} attribution must match git blame --porcelain"
        );
    }
    // The fixture is constructed so the span genuinely spans two commits —
    // otherwise the oracle comparison would pass vacuously on one hunk.
    let distinct: std::collections::HashSet<_> = oracle.values().map(|(sha, _)| sha).collect();
    assert_eq!(distinct.len(), 2, "span blames to both fixture commits");
}

#[tokio::test]
async fn blame_symbol_at_prior_revision_pins_attribution_and_flags_divergence() {
    let fixture = GitFixture::build();
    let server = git_backed_server(fixture.path());
    analyze(&server, fixture.path()).await;
    let (_root, _file, symbol) = fixture_symbol(fixture.path());
    let first = fixture.rev_parse("HEAD~1");

    let body = ok_json(&call_blame(&server, &symbol, Some(&first)).await);
    assert_eq!(body["available"], serde_json::json!(true));
    assert_eq!(body["rev"], serde_json::json!(first.clone()));
    for (line, (sha, _)) in hunk_lines(&body) {
        assert_eq!(sha, first, "line {line} attributed to the first commit");
    }
    // The working tree holds the EDITED contents, which differ from the
    // blamed (first) revision — the divergence must be reported.
    assert_eq!(body["stale"], serde_json::json!(true));
    assert!(
        body["stale_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("on-disk contents differ")),
        "stale_reason names the divergence: {body}"
    );
}

#[tokio::test]
async fn blame_symbol_flags_uncommitted_edits_as_stale() {
    let fixture = GitFixture::build();
    let server = git_backed_server(fixture.path());
    analyze(&server, fixture.path()).await;
    let (_root, file, symbol) = fixture_symbol(fixture.path());

    // Uncommitted working-tree edit after indexing: attribution still
    // reflects HEAD, so the response must say the two file states diverge.
    let mut contents = std::fs::read_to_string(&file).expect("read fixture source");
    contents.push_str("\n// trailing uncommitted note\n");
    std::fs::write(&file, contents).expect("apply uncommitted edit");

    let body = ok_json(&call_blame(&server, &symbol, None).await);
    assert_eq!(body["available"], serde_json::json!(true));
    assert_eq!(body["stale"], serde_json::json!(true));
    assert!(
        body["stale_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("on-disk contents differ")),
        "stale_reason explains the hazard: {body}"
    );
    assert!(
        !body["hunks"].as_array().unwrap().is_empty(),
        "stale results are returned flagged, not suppressed"
    );
}

/// Gate-review F2: an autocrlf-style checkout (CRLF on disk, LF in the
/// blob) is NOT stale — the staleness compare is line-ending-insensitive,
/// so the flag cannot degenerate into permanent noise on Windows-normalized
/// repositories.
#[tokio::test]
async fn blame_symbol_is_not_stale_on_a_crlf_normalized_checkout() {
    let fixture = GitFixture::build();
    let server = git_backed_server(fixture.path());
    analyze(&server, fixture.path()).await;
    let (_root, file, symbol) = fixture_symbol(fixture.path());

    // Materialize the exact committed contents with CRLF line endings —
    // what an autocrlf checkout produces for an LF blob.
    let committed = std::fs::read_to_string(&file).expect("read fixture source");
    assert!(!committed.contains('\r'), "fixture blob is LF");
    std::fs::write(&file, committed.replace('\n', "\r\n")).expect("write CRLF working copy");

    let body = ok_json(&call_blame(&server, &symbol, None).await);
    assert_eq!(body["available"], serde_json::json!(true));
    assert_eq!(
        body["stale"],
        serde_json::json!(false),
        "CRLF-only divergence is not staleness: {body}"
    );
    assert!(
        body.get("stale_reason").is_none(),
        "verified clean carries no stale_reason: {body}"
    );
}

#[tokio::test]
async fn blame_symbol_reports_unavailability_without_vcs_and_degrades_nothing() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("lib.rs"), INITIAL).unwrap();
    // Production shape for "cwd is not a repository": an empty registry.
    let server = rust_server(VcsRegistry::new());
    analyze(&server, dir.path()).await;
    let (_root, file, symbol) = fixture_symbol(dir.path());

    let body = ok_json(&call_blame(&server, &symbol, None).await);
    assert_eq!(body["available"], serde_json::json!(false));
    assert!(
        body["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("no supported version-control system")),
        "reason names the cause: {body}"
    );
    assert!(body["hunks"].as_array().unwrap().is_empty());

    // AC-22: every other tool behaves normally.
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
    let page = ok_json(&symbols);
    assert_eq!(page["total"], serde_json::json!(1));
}

#[tokio::test]
async fn blame_symbol_reports_untracked_file_as_unavailable() {
    let fixture = GitFixture::build();
    // Written after the last commit and never staged: indexed by the graph,
    // invisible to git history.
    std::fs::write(
        fixture.path().join("untracked.rs"),
        "pub fn untracked_function() -> u32 {\n    7\n}\n",
    )
    .unwrap();
    let server = git_backed_server(fixture.path());
    analyze(&server, fixture.path()).await;
    let root = code_graph_core::paths::canonicalize(fixture.path()).unwrap();
    let symbol = format!(
        "{}:untracked_function",
        root.join("untracked.rs").to_string_lossy()
    );

    let body = ok_json(&call_blame(&server, &symbol, None).await);
    assert_eq!(body["available"], serde_json::json!(false));
    assert!(
        body["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("no history for this path")),
        "untracked is distinct from no-VCS: {body}"
    );
}

#[tokio::test]
async fn blame_symbol_unknown_symbol_is_a_tool_error_with_suggestions() {
    let fixture = GitFixture::build();
    let server = git_backed_server(fixture.path());
    analyze(&server, fixture.path()).await;

    // A bare-name near-miss is the shape the suggestion search supports
    // (it matches against symbol names, not full IDs) — same affordance
    // `get_symbol_detail` provides.
    let r = call_blame(&server, "target_functio", None).await;
    assert_eq!(r.is_error, Some(true));
    let message = first_text(&r);
    assert!(
        message.contains("symbol not found") && message.contains("Did you mean"),
        "unknown symbol keeps the did-you-mean affordance: {message}"
    );
}

#[tokio::test]
async fn blame_symbol_unresolvable_at_is_a_tool_error() {
    let fixture = GitFixture::build();
    let server = git_backed_server(fixture.path());
    analyze(&server, fixture.path()).await;
    let (_root, _file, symbol) = fixture_symbol(fixture.path());

    let r = call_blame(&server, &symbol, Some("no-such-revision-spec")).await;
    assert_eq!(r.is_error, Some(true));
    assert!(
        first_text(&r).contains("cannot resolve revision"),
        "bad `at` names the specifier: {}",
        first_text(&r)
    );
}

/// AC-44 / NFR-10: a hung provider delays only the history tool. The gate
/// blocks `blame` while an unrelated query runs to completion. (The graph
/// read-lock discipline itself is enforced at compile time — a guard held
/// across the await would make the spawned future non-`Send` — so this
/// test pins the observable half: an unrelated query completes while the
/// provider hangs, and the blame future is still pending afterwards.)
struct GatedProvider {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl VcsProvider for GatedProvider {
    fn id(&self) -> &'static str {
        "gated-test"
    }

    fn detect(&self, _working_tree: &Path) -> bool {
        true
    }

    async fn blame(
        &self,
        _path: &Path,
        lines: Option<(u32, u32)>,
        _at: Option<&RevId>,
    ) -> Result<Vec<BlameHunk>, VcsError> {
        self.started.notify_one();
        self.release.notified().await;
        let (start_line, end_line) = lines.unwrap_or((1, 1));
        Ok(vec![BlameHunk {
            rev: RevId::new("gated-rev"),
            author: "gated".to_string(),
            timestamp_utc: 0,
            start_line,
            line_count: end_line.saturating_sub(start_line).saturating_add(1),
        }])
    }

    async fn revisions_touching(&self, _path: &Path, _limit: u32) -> Result<Vec<Commit>, VcsError> {
        Ok(Vec::new())
    }

    async fn read_at(&self, _rev: &RevId, _path: &Path) -> Result<Vec<u8>, VcsError> {
        Err(VcsError::NotFound("gated provider has no blobs".into()))
    }

    async fn resolve_rev(&self, spec: &str) -> Result<RevId, VcsError> {
        Ok(RevId::new(spec))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blame_symbol_slow_provider_delays_only_history_tools() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut vcs = VcsRegistry::new();
    vcs.register(Box::new(GatedProvider {
        started: Arc::clone(&started),
        release: Arc::clone(&release),
    }))
    .unwrap();

    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("lib.rs"), INITIAL).unwrap();
    let server = rust_server(vcs);
    analyze(&server, dir.path()).await;
    let (_root, file, symbol) = fixture_symbol(dir.path());

    let inner = server.inner.clone();
    let root = inner.root_path.read().clone();
    let blame_symbol_id = symbol.clone();
    let blame = tokio::spawn(async move {
        code_graph_tools::handlers::history::blame_symbol(
            &inner.graph,
            &inner.vcs,
            root,
            &blame_symbol_id,
            None,
        )
        .await
    });
    started.notified().await;
    assert!(!blame.is_finished(), "blame is held at the provider gate");

    // A non-history query completes while the provider hangs.
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
        !blame.is_finished(),
        "the unrelated query finished first; history alone is delayed"
    );

    release.notify_one();
    let body = ok_json(&blame.await.expect("blame task joins"));
    assert_eq!(body["available"], serde_json::json!(true));
    assert_eq!(body["hunks"][0]["rev"], serde_json::json!("gated-rev"));
    // GatedProvider's read_at errors, so the staleness comparison cannot
    // run — that must surface as an explicit unverified note, not read as
    // "verified clean" (gate-review F6).
    assert_eq!(body["stale"], serde_json::json!(false));
    assert!(
        body["stale_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("staleness not verified")),
        "unverifiable staleness is explicit: {body}"
    );
}
