#![forbid(unsafe_code)]

//! Pure-Rust Git VCS provider implementation.
//!
//! Production operations use `gix` only. The Git CLI is confined to the test
//! fixture harness, where it creates and independently verifies repositories.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use code_graph_vcs::{
    BlameHunk, Commit, ProviderDetection, RevId, RevisionWindow, VcsError, VcsProvider,
};

/// A Git provider bound to one discovered working tree.
///
/// Bind the provider with [`GitProvider::open`] before registering it. The
/// provider reopens that repository inside each blocking task instead of
/// keeping a non-thread-safe repository handle across async calls.
#[derive(Clone, Debug)]
pub struct GitProvider {
    project_root: PathBuf,
    common_git_dir: PathBuf,
}

impl GitProvider {
    /// Discover and bind a Git working tree containing `project_root`.
    pub fn open(project_root: impl AsRef<Path>) -> Result<Self, VcsError> {
        let project_root = project_root.as_ref();
        let repository = gix::discover(project_root).map_err(|error| {
            VcsError::Unavailable(format!("{}: {error}", project_root.display()))
        })?;
        let work_dir = repository.workdir().ok_or_else(|| {
            VcsError::Unavailable(format!(
                "{} is a bare Git repository",
                project_root.display()
            ))
        })?;

        // Bind the canonical long form: callers pass graph-stored paths that
        // are dunce-canonicalized, and `repository_relative_path` strips the
        // bound root lexically — an 8.3 short-form or verbatim-prefixed
        // binding would spuriously report every file as outside the tree.
        let work_dir = dunce::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());

        Ok(Self {
            common_git_dir: common_git_dir(&repository),
            project_root: work_dir,
        })
    }

    async fn blocking<T, F>(&self, operation: F) -> Result<T, VcsError>
    where
        T: Send + 'static,
        F: FnOnce(PathBuf) -> Result<T, VcsError> + Send + 'static,
    {
        let project_root = self.project_root.clone();
        tokio::task::spawn_blocking(move || operation(project_root))
            .await
            .map_err(|error| VcsError::Operation(format!("Git blocking task failed: {error}")))?
    }
}

#[async_trait]
impl VcsProvider for GitProvider {
    fn id(&self) -> &'static str {
        "git"
    }

    fn detect(&self, working_tree: &Path) -> ProviderDetection {
        // A provider is bound to ONE working tree; answering "yes" for any
        // git tree would let the registry select this provider for a root
        // whose operations then run against the wrong bound repository
        // (phase-gate review F1). Detection is an identity check: the probe
        // must discover the same canonical working tree this provider is
        // bound to.
        let Ok(discovered) = Self::open(working_tree) else {
            return ProviderDetection::NoMatch;
        };
        if discovered.project_root == self.project_root {
            ProviderDetection::Selected
        } else if discovered.common_git_dir == self.common_git_dir {
            ProviderDetection::DifferentCheckout
        } else {
            ProviderDetection::BoundElsewhere
        }
    }

    async fn blame(
        &self,
        path: &Path,
        lines: Option<(u32, u32)>,
        at: Option<&RevId>,
    ) -> Result<Vec<BlameHunk>, VcsError> {
        let path = path.to_path_buf();
        let revision = at.map(|revision| revision.as_str().to_owned());
        self.blocking(move |project_root| blame(&project_root, &path, lines, revision.as_deref()))
            .await
    }

    async fn revisions_touching(
        &self,
        path: &Path,
        limit: u32,
    ) -> Result<RevisionWindow, VcsError> {
        let path = path.to_path_buf();
        self.blocking(move |project_root| revisions_touching(&project_root, &path, limit))
            .await
    }

    async fn read_at(&self, rev: &RevId, path: &Path) -> Result<Vec<u8>, VcsError> {
        let revision = rev.as_str().to_owned();
        let path = path.to_path_buf();
        self.blocking(move |project_root| read_at(&project_root, &revision, &path))
            .await
    }

    async fn resolve_rev(&self, spec: Option<&str>) -> Result<RevId, VcsError> {
        let spec = spec.map(ToOwned::to_owned);
        self.blocking(move |project_root| resolve_rev(&project_root, spec.as_deref()))
            .await
    }
}

fn open_repository(project_root: &Path) -> Result<gix::Repository, VcsError> {
    gix::discover(project_root)
        .map_err(|error| VcsError::Unavailable(format!("{}: {error}", project_root.display())))
}

fn resolve_rev(project_root: &Path, spec: Option<&str>) -> Result<RevId, VcsError> {
    let repository = open_repository(project_root)?;
    let spec = spec.unwrap_or("HEAD");
    let object = repository
        .rev_parse_single(spec)
        .map_err(|error| VcsError::Operation(format!("resolve {spec}: {error}")))?;
    let commit = object
        .object()
        .map_err(|error| VcsError::Operation(format!("read {spec}: {error}")))?
        .peel_to_commit()
        .map_err(|error| VcsError::Operation(format!("peel {spec} to commit: {error}")))?;
    Ok(RevId::new(commit.id.to_string()))
}

/// Canonical identity of the repository object store shared by linked
/// worktrees. `gix` owns parsing Git's worktree metadata and returns the main
/// repository directory for linked worktrees.
fn common_git_dir(repository: &gix::Repository) -> PathBuf {
    let common = repository.common_dir();
    dunce::canonicalize(common).unwrap_or_else(|_| common.to_path_buf())
}

fn read_at(project_root: &Path, revision: &str, path: &Path) -> Result<Vec<u8>, VcsError> {
    let repository = open_repository(project_root)?;
    let object = repository
        .rev_parse_single(revision)
        .map_err(|error| VcsError::Operation(format!("resolve {revision}: {error}")))?;
    let commit = object
        .object()
        .map_err(|error| VcsError::Operation(format!("read {revision}: {error}")))?
        .peel_to_commit()
        .map_err(|error| VcsError::Operation(format!("peel {revision} to commit: {error}")))?;
    let tree = commit
        .tree()
        .map_err(|error| VcsError::Operation(format!("read tree for {revision}: {error}")))?;
    require_owned_by_bound_repository(project_root, path)?;
    let relative_path = repository_relative_path(project_root, path)?;
    let entry = tree
        .lookup_entry_by_path(relative_path)
        .map_err(|error| VcsError::Operation(format!("read tree entry for {revision}: {error}")))?
        .ok_or_else(|| VcsError::NotFound(format!("{} at {revision}", path.display())))?;
    // The tree entry EXISTS past this point: a blob lookup failure is an
    // unreadable object (partial clone without the blob, corrupt object
    // store, non-blob entry such as a gitlink), NOT path absence. It must
    // surface as `Operation` — consumers treat `NotFound` as a
    // deterministic, cacheable "path absent at this revision" (a
    // `symbol_history` deletion transition and a permanent tombstone), and
    // a read failure mapped there would manufacture false `removed`
    // entries that never heal.
    let blob = repository.find_blob(entry.oid()).map_err(|error| {
        VcsError::Operation(format!(
            "read blob for {} at {revision}: {error}",
            path.display()
        ))
    })?;
    Ok(blob.data.to_vec())
}

fn blame(
    project_root: &Path,
    path: &Path,
    lines: Option<(u32, u32)>,
    revision: Option<&str>,
) -> Result<Vec<BlameHunk>, VcsError> {
    let repository = open_repository(project_root)?;
    require_owned_by_bound_repository(project_root, path)?;
    let relative_path = repository_relative_path(project_root, path)?;
    let revision = revision
        .map(|spec| {
            repository
                .rev_parse_single(spec)
                .map_err(|error| VcsError::Operation(format!("resolve {spec}: {error}")))
        })
        .transpose()?
        .map(gix::Id::detach);
    let revision = match revision {
        Some(revision) => revision,
        None => repository
            .head_id()
            .map_err(|error| VcsError::Operation(format!("resolve HEAD: {error}")))?
            .detach(),
    };
    // Absence at the blamed revision is `NotFound` (an untracked file, or a
    // path introduced later), not an opaque `Operation` failure — callers
    // route `NotFound` to the success-shaped "no history for this path"
    // outcome (FR-36). Same membership check `read_at` performs.
    let tree = repository
        .find_commit(revision)
        .map_err(|error| VcsError::Operation(format!("read {revision}: {error}")))?
        .tree()
        .map_err(|error| VcsError::Operation(format!("read tree for {revision}: {error}")))?;
    let entry = tree
        .lookup_entry_by_path(&relative_path)
        .map_err(|error| VcsError::Operation(format!("read tree entry for {revision}: {error}")))?
        .ok_or_else(|| VcsError::NotFound(format!("{} at {revision}", path.display())))?;

    // Clamp the requested range to the file as it exists at the blamed
    // revision: the caller's span comes from the (possibly newer) on-disk
    // file, gix rejects out-of-range blame ranges, and a span lying wholly
    // beyond this revision's EOF means "no attributable lines" — the tool
    // layer reports the divergence through its staleness flag — not an
    // error (phase-gate review F4).
    let blob = repository.find_blob(entry.oid()).map_err(|error| {
        VcsError::Operation(format!(
            "read blob for {} at {revision}: {error}",
            path.display()
        ))
    })?;
    let revision_line_count = blob_line_count(&blob.data);
    let requested = lines.unwrap_or((1, u32::MAX));
    let range_start = requested.0.max(1);
    let range_end = requested.1.max(range_start);
    if range_start > revision_line_count {
        return Ok(Vec::new());
    }
    let range_end = range_end.min(revision_line_count);

    let tree_path = gix_tree_path(&relative_path, path)?;
    // Blame only the requested range (phase-gate review F3): whole-file
    // blame pays the full history-diff walk for every hunk in the file,
    // which on engine-scale files re-opens the documented MCP-timeout
    // failure class this codebase works to avoid.
    let ranges = gix::blame::BlameRanges::from_one_based_inclusive_range(range_start..=range_end)
        .map_err(|error| {
        VcsError::Operation(format!(
            "blame range {range_start}..={range_end} for {}: {error}",
            path.display()
        ))
    })?;
    let options = gix::repository::blame_file::Options {
        ranges,
        ..Default::default()
    };
    let outcome = repository
        .blame_file(
            gix::bstr::BStr::new(tree_path.as_bytes()),
            revision,
            options,
        )
        .map_err(|error| VcsError::Operation(format!("blame {}: {error}", path.display())))?;

    let mut hunks = Vec::new();
    for blame_entry in outcome.entries.iter() {
        let start_line = blame_entry.start_in_blamed_file.saturating_add(1);
        let line_count = blame_entry.len.get();
        let end_line = start_line.saturating_add(line_count.saturating_sub(1));
        let overlap_start = start_line.max(range_start);
        let overlap_end = end_line.min(range_end);
        // Clip before decoding: a non-overlapping entry must not cost a
        // commit decode, nor fail the whole call if its commit is
        // unreadable (phase-gate review F3).
        if overlap_start > overlap_end {
            continue;
        }
        let commit = repository
            .find_commit(blame_entry.commit_id)
            .map_err(|error| {
                VcsError::Operation(format!(
                    "read blame commit {}: {error}",
                    blame_entry.commit_id
                ))
            })?;
        let (author, timestamp_utc) = commit_identity(&commit)?;
        hunks.push(BlameHunk {
            rev: RevId::new(blame_entry.commit_id.to_string()),
            author,
            timestamp_utc,
            start_line: overlap_start,
            line_count: overlap_end.saturating_sub(overlap_start).saturating_add(1),
        });
    }
    Ok(hunks)
}

/// One-based line count of a blob: newline count plus a final unterminated
/// line when present. An empty blob has zero lines.
fn blob_line_count(data: &[u8]) -> u32 {
    let newlines = data.iter().filter(|&&byte| byte == b'\n').count();
    let unterminated = usize::from(!data.is_empty() && data.last() != Some(&b'\n'));
    u32::try_from(newlines + unterminated).unwrap_or(u32::MAX)
}

/// Upper bound on commits examined by one `revisions_touching` walk. The
/// manual revwalk visits every reachable commit when a path has fewer than
/// `limit` touches — including a path that no longer exists — so without a
/// cap a single call on an engine-scale repository (10⁵–10⁶ commits) is
/// minutes of CPU inside one blocking task. Hitting the cap returns what was
/// found rather than erroring and sets [`RevisionWindow::truncated`]. An
/// explicit shallow-clone boundary sets the same flag because older reachable
/// history is unavailable there too.
const MAX_REVWALK_COMMITS: usize = 100_000;

fn revisions_touching(
    project_root: &Path,
    path: &Path,
    limit: u32,
) -> Result<RevisionWindow, VcsError> {
    if limit == 0 {
        return Ok(RevisionWindow {
            commits: Vec::new(),
            truncated: false,
        });
    }

    let repository = open_repository(project_root)?;
    require_owned_by_bound_repository(project_root, path)?;
    let relative_path = repository_relative_path(project_root, path)?;
    let head = repository
        .head_id()
        .map_err(|error| VcsError::Operation(format!("resolve HEAD: {error}")))?
        .detach();
    let shallow = shallow_commit_ids(&repository)?;
    let mut revisions = Vec::new();
    let mut pending = BinaryHeap::new();
    let mut sequence = 0;
    enqueue_commit(&repository, &mut pending, head, &mut sequence)?;
    let mut seen = HashSet::new();
    let mut truncated = false;
    while let Some(pending_commit) = pending.pop() {
        if seen.len() >= MAX_REVWALK_COMMITS {
            // A pending entry was popped but not examined: reachable
            // history remains beyond the cap. Filling the caller's `limit`
            // below is NOT truncation — the caller sees that itself.
            truncated = true;
            break;
        }
        if !seen.insert(pending_commit.id) {
            continue;
        }
        let commit = repository.find_commit(pending_commit.id).map_err(|error| {
            VcsError::Operation(format!("read commit {}: {error}", pending_commit.id))
        })?;
        if shallow.contains(&pending_commit.id) {
            truncated = true;
        } else {
            for parent_id in commit.parent_ids() {
                enqueue_commit(&repository, &mut pending, parent_id.detach(), &mut sequence)?;
            }
        }
        if commit_changes_path(&repository, &shallow, &commit, &relative_path)? {
            revisions.push(commit_metadata(&commit)?);
            if revisions.len() == limit as usize {
                break;
            }
        }
    }
    Ok(RevisionWindow {
        commits: revisions,
        truncated,
    })
}

/// One commit queued for the manual newest-first revwalk.
///
/// The sequence makes equal commit timestamps deterministic without assigning
/// any meaning to the opaque revision identifier.
#[derive(Debug, Eq, PartialEq)]
struct PendingCommit {
    timestamp_utc: i64,
    sequence: u64,
    id: gix::ObjectId,
}

impl Ord for PendingCommit {
    fn cmp(&self, other: &Self) -> Ordering {
        self.timestamp_utc
            .cmp(&other.timestamp_utc)
            .then_with(|| self.sequence.cmp(&other.sequence))
    }
}

impl PartialOrd for PendingCommit {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Queue `id` for the walk. A missing commit object is an `Operation`; the
/// caller skips parent traversal only for IDs proven by the explicit shallow
/// boundary set.
fn enqueue_commit(
    repository: &gix::Repository,
    pending: &mut BinaryHeap<PendingCommit>,
    id: gix::ObjectId,
    sequence: &mut u64,
) -> Result<(), VcsError> {
    let commit = repository
        .find_commit(id)
        .map_err(|error| VcsError::Operation(format!("read commit {id}: {error}")))?;
    pending.push(PendingCommit {
        timestamp_utc: commit_timestamp(&commit)?,
        sequence: *sequence,
        id,
    });
    *sequence = sequence.saturating_add(1);
    Ok(())
}

fn commit_changes_path(
    repository: &gix::Repository,
    shallow: &HashSet<gix::ObjectId>,
    commit: &gix::Commit<'_>,
    path: &Path,
) -> Result<bool, VcsError> {
    let tree = commit
        .tree()
        .map_err(|error| VcsError::Operation(format!("read commit tree: {error}")))?;
    let current = tree
        .lookup_entry_by_path(path)
        .map_err(|error| VcsError::Operation(format!("read commit tree entry: {error}")))?
        .map(|entry| (entry.oid().to_owned(), entry.mode()));
    if shallow.contains(&commit.id) {
        return Ok(current.is_some());
    }
    let mut parent_ids = commit.parent_ids();
    let Some(first_parent_id) = parent_ids.next() else {
        return Ok(current.is_some());
    };
    // This commit is not explicitly shallow, so a missing parent is an
    // operational failure rather than an implicit history boundary.
    let first_parent = repository.find_commit(first_parent_id).map_err(|error| {
        VcsError::Operation(format!("read parent commit {first_parent_id}: {error}"))
    })?;

    if first_parent
        .tree()
        .map_err(|error| VcsError::Operation(format!("read parent tree: {error}")))?
        .lookup_entry_by_path(path)
        .map_err(|error| VcsError::Operation(format!("read parent tree entry: {error}")))?
        .map(|entry| (entry.oid().to_owned(), entry.mode()))
        != current
    {
        return Ok(true);
    }

    for parent_id in parent_ids {
        // Every parent of a non-shallow merge must be readable.
        let parent = repository.find_commit(parent_id).map_err(|error| {
            VcsError::Operation(format!("read parent commit {parent_id}: {error}"))
        })?;
        if parent
            .tree()
            .map_err(|error| VcsError::Operation(format!("read parent tree: {error}")))?
            .lookup_entry_by_path(path)
            .map_err(|error| VcsError::Operation(format!("read parent tree entry: {error}")))?
            .map(|entry| (entry.oid().to_owned(), entry.mode()))
            != current
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Read Git's explicit shallow-boundary set from the common Git directory.
/// Missing boundary metadata means complete history; malformed or unreadable
/// metadata is operational corruption, not permission to hide missing objects.
fn shallow_commit_ids(repository: &gix::Repository) -> Result<HashSet<gix::ObjectId>, VcsError> {
    let path = repository.common_dir().join("shallow");
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashSet::new()),
        Err(error) => {
            return Err(VcsError::Operation(format!(
                "read shallow boundary {}: {error}",
                path.display()
            )))
        }
    };
    contents
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.parse().map_err(|error| {
                VcsError::Operation(format!(
                    "parse shallow boundary {line:?} in {}: {error}",
                    path.display()
                ))
            })
        })
        .collect()
}

fn commit_metadata(commit: &gix::Commit<'_>) -> Result<Commit, VcsError> {
    let (author, timestamp_utc) = commit_identity(commit)?;
    let message = commit
        .message_raw()
        .map_err(|error| VcsError::Operation(format!("read commit message: {error}")))?;
    Ok(Commit {
        rev: RevId::new(commit.id.to_string()),
        author,
        timestamp_utc,
        summary: message
            .to_string()
            .lines()
            .next()
            .unwrap_or_default()
            .to_string(),
    })
}

fn commit_identity(commit: &gix::Commit<'_>) -> Result<(String, i64), VcsError> {
    let decoded = commit
        .decode()
        .map_err(|error| VcsError::Operation(format!("decode commit {}: {error}", commit.id)))?;
    let author = gix::actor::SignatureRef::from_bytes(decoded.author).map_err(|error| {
        VcsError::Operation(format!("read commit author for {}: {error}", commit.id))
    })?;
    let timestamp_utc = signature_timestamp(author.time, "author", commit.id)?;
    Ok((author.name.to_string(), timestamp_utc))
}

fn commit_timestamp(commit: &gix::Commit<'_>) -> Result<i64, VcsError> {
    let decoded = commit
        .decode()
        .map_err(|error| VcsError::Operation(format!("decode commit {}: {error}", commit.id)))?;
    let committer = gix::actor::SignatureRef::from_bytes(decoded.committer).map_err(|error| {
        VcsError::Operation(format!("read commit committer for {}: {error}", commit.id))
    })?;
    signature_timestamp(committer.time, "committer", commit.id)
}

fn signature_timestamp(timestamp: &str, role: &str, id: gix::ObjectId) -> Result<i64, VcsError> {
    timestamp
        .split_whitespace()
        .next()
        .ok_or_else(|| VcsError::Operation(format!("missing {role} timestamp for {id}")))?
        .parse()
        .map_err(|error| VcsError::Operation(format!("invalid {role} timestamp for {id}: {error}")))
}

/// gix tree paths are `/`-separated regardless of host. Joining the relative
/// path's components explicitly keeps `blame` correct where the OS separator
/// differs (Windows), and surfaces a non-UTF-8 name as an error instead of
/// silently corrupting it through a lossy conversion.
fn gix_tree_path(relative_path: &Path, original: &Path) -> Result<String, VcsError> {
    let mut tree_path = String::new();
    for component in relative_path.components() {
        let part = component.as_os_str().to_str().ok_or_else(|| {
            VcsError::Operation(format!(
                "{} contains a non-UTF-8 path component",
                original.display()
            ))
        })?;
        if !tree_path.is_empty() {
            tree_path.push('/');
        }
        tree_path.push_str(part);
    }
    Ok(tree_path)
}

/// The queried file must belong to the bound working tree. Without this
/// check, a nested full clone inside the bound tree (a vendored checkout)
/// would satisfy the lexical containment strip and blame the OUTER
/// repository's stale copy of the same relative path — success-shaped
/// attribution from the wrong repository (phase-gate review F1). A
/// submodule path likewise reports as belonging to its own repository
/// instead of masquerading as an untracked outer-tree file. `NotFound`
/// routes to the tool layer's success-shaped unavailability with the
/// honest reason.
fn require_owned_by_bound_repository(project_root: &Path, path: &Path) -> Result<(), VcsError> {
    if !path.is_absolute() {
        return Ok(());
    }
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let missing = missing_path(path)?;
    let repository = match gix::discover(parent) {
        Ok(repository) => repository,
        Err(error) if missing => {
            return Err(VcsError::NotFound(format!(
                "cannot verify repository ownership for missing path {} from {}: {error}",
                path.display(),
                parent.display()
            )))
        }
        // Existing untracked files may have no discoverable parent repository;
        // the immutable tree lookup below gives their normal no-history result.
        Err(_) => return Ok(()),
    };
    let Some(work_dir) = repository.workdir() else {
        return Ok(());
    };
    let work_dir = dunce::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());
    if work_dir == project_root {
        if missing && has_nested_git_marker(project_root, parent)? {
            return Err(VcsError::NotFound(format!(
                "cannot verify repository ownership for missing path {}: a nested Git marker blocks discovery",
                path.display()
            )));
        }
        Ok(())
    } else {
        Err(VcsError::NotFound(format!(
            "{} belongs to a different repository ({}) than the bound working tree ({})",
            path.display(),
            work_dir.display(),
            project_root.display()
        )))
    }
}

/// A malformed nested `.git` file can make discovery climb to the outer
/// checkout even though the path belongs to a missing nested checkout. For a
/// missing path that would permit an outer tree entry at the same lexical
/// location to shadow it, so treat the marker as an ownership boundary.
fn missing_path(path: &Path) -> Result<bool, VcsError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(VcsError::NotFound(format!(
            "cannot verify repository ownership for {}: inspect path metadata: {error}",
            path.display()
        ))),
    }
}

fn has_nested_git_marker(project_root: &Path, parent: &Path) -> Result<bool, VcsError> {
    let root = normalize_lexical_path(project_root);
    let mut current = normalize_lexical_path(parent);
    while current.starts_with(&root) && current != root {
        let marker = current.join(".git");
        match std::fs::symlink_metadata(&marker) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(VcsError::NotFound(format!(
                    "cannot verify repository ownership for missing path below {}: inspect nested Git marker metadata: {error}",
                    marker.display()
                )))
            }
        }
        if !current.pop() {
            break;
        }
    }
    Ok(false)
}

fn repository_relative_path(project_root: &Path, path: &Path) -> Result<PathBuf, VcsError> {
    let relative_path = if path.is_absolute() {
        // The bound root is canonical (see `GitProvider::open`); bring the
        // incoming absolute path to the same form so the lexical strip is
        // not defeated by 8.3 short names or symlinked prefixes. A path
        // that does not exist on disk (e.g. deleted, queried at an old
        // revision) falls back to its lexical form unchanged.
        let canonical = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let normalized_root = normalize_lexical_path(project_root);
        let normalized_path = normalize_lexical_path(&canonical);
        normalized_path
            .strip_prefix(normalized_root)
            .map(Path::to_path_buf)
            .map_err(|_| {
                VcsError::NotFound(format!(
                    "{} is outside {}",
                    path.display(),
                    project_root.display()
                ))
            })?
    } else {
        normalize_lexical_path(path)
    };

    if relative_path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(VcsError::NotFound(format!(
            "{} escapes bound repository root {}",
            path.display(),
            project_root.display()
        )));
    }
    Ok(relative_path)
}

fn normalize_lexical_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::Normal(segment) => normalized.push(segment),
            std::path::Component::ParentDir => match normalized.file_name() {
                Some(name) if name != ".." => {
                    normalized.pop();
                }
                _ if !path.is_absolute() => normalized.push(".."),
                _ => {}
            },
        }
    }
    normalized
}

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
            self.commit_staged(commit.message, commit.timestamp)?;
            Ok(())
        }

        fn commit_staged(&self, message: &str, timestamp: &str) -> Result<(), HarnessError> {
            self.git_with_commit_identity(
                &["commit", "--no-gpg-sign", "--message", message],
                timestamp,
            )
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

        fn path_history(
            &self,
            path: &str,
        ) -> Result<Vec<(String, String, i64, String)>, HarnessError> {
            self.git_stdout(&["log", "--format=%H%x1f%an%x1f%at%x1f%s", "--", path])
                .map(|history| {
                    history
                        .lines()
                        .map(|line| {
                            let mut fields = line.split('\x1f');
                            let revision =
                                fields.next().expect("Git log revision field").to_string();
                            let author = fields.next().expect("Git log author field").to_string();
                            let timestamp_utc = fields
                                .next()
                                .expect("Git log timestamp field")
                                .parse()
                                .expect("Git log timestamp is an integer");
                            let summary = fields.next().expect("Git log summary field").to_string();
                            (revision, author, timestamp_utc, summary)
                        })
                        .collect()
                })
        }

        fn blame_revision(&self, path: &str, line: u32) -> Result<String, HarnessError> {
            self.git_stdout(&[
                "blame",
                "--porcelain",
                "-L",
                &format!("{line},{line}"),
                "--",
                path,
            ])
            .and_then(|output| {
                output
                    .split_whitespace()
                    .next()
                    .map(str::to_string)
                    .ok_or_else(|| HarnessError("git blame produced no revision".to_string()))
            })
        }

        fn blame_revision_at(
            &self,
            revision: &str,
            path: &str,
            line: u32,
        ) -> Result<String, HarnessError> {
            self.git_stdout(&[
                "blame",
                "--porcelain",
                "-L",
                &format!("{line},{line}"),
                revision,
                "--",
                path,
            ])
            .and_then(|output| {
                output
                    .split_whitespace()
                    .next()
                    .map(str::to_string)
                    .ok_or_else(|| HarnessError("git blame produced no revision".to_string()))
            })
        }

        fn blob_id(&self, revision: &str, path: &str) -> Result<String, HarnessError> {
            let object = format!("{revision}:{path}");
            self.git_stdout(&["rev-parse", &object])
                .map(|id| id.trim().to_string())
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

    const UNRELATED_CHANGE: ScriptedCommit = ScriptedCommit {
        message: "add unrelated documentation",
        timestamp: "2001-01-06T00:00:00+0000",
        changes: &[FileChange {
            path: "README.md",
            contents: "This commit intentionally does not touch the calculator.\n",
        }],
    };

    fn provider_script() -> FixtureScript {
        FixtureScript {
            commits: &[
                INITIAL,
                REFORMAT_ONLY,
                LOGIC_CHANGE,
                MOVE_WITHIN_FILE,
                LITERAL_ONLY,
                UNRELATED_CHANGE,
            ],
        }
    }

    fn initial_only_script() -> FixtureScript {
        FixtureScript {
            commits: &[INITIAL],
        }
    }

    const FEATURE_UNRELATED_CHANGE: ScriptedCommit = ScriptedCommit {
        message: "add feature notes",
        timestamp: "2001-01-02T00:00:00+0000",
        changes: &[FileChange {
            path: "feature-notes.md",
            contents: "Feature branch does not change the calculator.\n",
        }],
    };

    fn merge_parent_fixture() -> Result<Fixture, HarnessError> {
        let fixture = Fixture::build(initial_only_script())?;
        fixture.git(&["checkout", "-b", "feature"])?;
        fixture.apply_commit(FEATURE_UNRELATED_CHANGE)?;
        fixture.git(&["checkout", INITIAL_BRANCH])?;
        fixture.apply_commit(LOGIC_CHANGE)?;
        fixture.git(&["merge", "--no-ff", "--no-commit", "feature"])?;
        fixture.commit_staged("merge feature notes", "2001-01-04T00:00:00+0000")?;
        Ok(fixture)
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

    #[tokio::test]
    async fn git_provider_matches_git_cli_for_fixture_operations_and_path_history() {
        use super::GitProvider;
        use code_graph_vcs::{ProviderDetection, VcsError, VcsProvider};

        const PATH: &str = "src/calculator.rs";
        let fixture = Fixture::build(provider_script()).expect("fixture must build");
        let provider = GitProvider::open(fixture.path()).expect("fixture is a Git working tree");
        let source_path = fixture.path().join(PATH);

        assert_eq!(provider.detect(fixture.path()), ProviderDetection::Selected);
        let plain_directory = tempfile::tempdir().expect("plain temporary directory");
        assert_eq!(
            provider.detect(plain_directory.path()),
            ProviderDetection::NoMatch
        );
        assert!(GitProvider::open(plain_directory.path()).is_err());

        let revision = provider
            .resolve_rev(Some("HEAD~2"))
            .await
            .expect("Git revision resolves");
        assert_eq!(
            provider
                .resolve_rev(None)
                .await
                .expect("provider default revision resolves"),
            provider
                .resolve_rev(Some("HEAD"))
                .await
                .expect("explicit Git revision resolves"),
            "None selects the provider default without exposing a Git default to callers"
        );
        assert_eq!(
            provider
                .read_at(&revision, Path::new(PATH))
                .await
                .expect("read historical relative path"),
            fixture
                .file_at_commit(revision.as_str(), PATH)
                .expect("Git CLI reads the same historical file")
                .into_bytes()
        );
        assert_eq!(
            provider
                .read_at(&revision, Path::new("src/../src/calculator.rs"))
                .await
                .expect("component-normalized in-root path reads"),
            fixture
                .file_at_commit(revision.as_str(), PATH)
                .expect("Git CLI reads the normalized historical file")
                .into_bytes()
        );
        assert!(matches!(
            provider
                .read_at(&revision, Path::new("src/../../outside.rs"))
                .await,
            Err(VcsError::NotFound(_))
        ));

        let expected_history = fixture.path_history(PATH).expect("Git CLI path history");
        let actual_history = provider
            .revisions_touching(&source_path, 3)
            .await
            .expect("manual path revwalk succeeds");
        let actual_history = actual_history
            .commits
            .iter()
            .map(|commit| {
                (
                    commit.rev.as_str().to_string(),
                    commit.author.clone(),
                    commit.timestamp_utc,
                    commit.summary.clone(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(actual_history, expected_history[..3]);
        let full_history = provider
            .revisions_touching(&source_path, 50)
            .await
            .expect("uncapped fixture history succeeds");
        assert_eq!(
            full_history
                .commits
                .last()
                .map(|commit| commit.summary.as_str()),
            Some("initial calculator"),
            "the root commit introducing a path must be included"
        );
        assert!(
            !full_history.truncated,
            "an exhausted fixture history is complete, not truncated"
        );
        assert!(
            actual_history
                .iter()
                .all(|(_, _, _, summary)| summary != "add unrelated documentation"),
            "per-parent tree comparison must exclude commits that did not change the requested path"
        );
        assert!(provider
            .revisions_touching(&source_path, 0)
            .await
            .expect("zero-limit history succeeds")
            .commits
            .is_empty());

        let hunks = provider
            .blame(&source_path, Some((1, 1)), None)
            .await
            .expect("blame succeeds");
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].start_line, 1);
        assert_eq!(hunks[0].line_count, 1);
        assert_eq!(
            hunks[0].rev.as_str(),
            fixture
                .blame_revision(PATH, 1)
                .expect("Git CLI blame revision")
        );
        assert_eq!(hunks[0].author, AUTHOR_NAME);
        assert_eq!(hunks[0].timestamp_utc, 978_652_800);

        let historical_hunks = provider
            .blame(&source_path, Some((1, 1)), Some(&revision))
            .await
            .expect("revision-bound blame succeeds");
        assert_eq!(historical_hunks.len(), 1);
        assert_eq!(
            historical_hunks[0].rev.as_str(),
            fixture
                .blame_revision_at(revision.as_str(), PATH, 1)
                .expect("Git CLI revision-bound blame")
        );
    }

    #[tokio::test]
    async fn revisions_touching_compares_every_merge_parent() {
        use super::GitProvider;
        use code_graph_vcs::VcsProvider;

        const PATH: &str = "src/calculator.rs";
        let fixture = merge_parent_fixture().expect("merge fixture builds");
        let provider = GitProvider::open(fixture.path()).expect("fixture is a Git working tree");
        let merge_revision = provider
            .resolve_rev(Some("HEAD"))
            .await
            .expect("resolve merge");
        let history = provider
            .revisions_touching(&fixture.path().join(PATH), 10)
            .await
            .expect("walk merge history");

        assert!(
            history
                .commits
                .iter()
                .any(|commit| commit.rev == merge_revision),
            "the merge changes the path against its second parent even though it matches its first"
        );
    }

    #[tokio::test]
    async fn revisions_touching_detects_mode_only_changes_and_keeps_the_root_commit() {
        use super::GitProvider;
        use code_graph_vcs::VcsProvider;

        const PATH: &str = "src/calculator.rs";
        let fixture = Fixture::build(initial_only_script()).expect("fixture must build");
        fixture
            .git(&["update-index", "--chmod=+x", PATH])
            .expect("stage executable-bit-only change");
        fixture
            .commit_staged("mark calculator executable", "2001-01-02T00:00:00+0000")
            .expect("commit executable-bit-only change");
        assert_eq!(
            fixture.blob_id("HEAD", PATH).expect("current blob id"),
            fixture.blob_id("HEAD~1", PATH).expect("parent blob id"),
            "the mode-only regression fixture must retain the same blob"
        );

        let provider = GitProvider::open(fixture.path()).expect("fixture is a Git working tree");
        let history = provider
            .revisions_touching(&fixture.path().join(PATH), 10)
            .await
            .expect("walk mode-only history");
        assert_eq!(
            history
                .commits
                .iter()
                .map(|commit| commit.summary.as_str())
                .collect::<Vec<_>>(),
            ["mark calculator executable", "initial calculator"]
        );
    }

    /// M5 regression: a `--depth 1` clone advertises parents beyond the
    /// fetch depth without carrying their objects. The revwalk must treat
    /// the unreadable parent as a history boundary — returning the boundary
    /// commit the way `git log` does — instead of failing the whole call.
    #[tokio::test]
    async fn revisions_touching_terminates_at_a_shallow_clone_boundary() {
        use super::GitProvider;
        use code_graph_vcs::VcsProvider;

        const PATH: &str = "src/calculator.rs";
        let fixture = Fixture::build(phase_six_script()).expect("fixture must build");
        let full_history_len = fixture
            .path_history(PATH)
            .expect("full-history oracle")
            .len();
        assert!(
            full_history_len > 1,
            "the fixture must have history beyond the shallow boundary"
        );

        // `git clone --depth 1` requires a transport; `file://` provides one
        // without any network. Forward slashes keep the URL valid on Windows.
        let source = fixture.path().to_string_lossy().replace('\\', "/");
        let url = if source.starts_with('/') {
            format!("file://{source}")
        } else {
            format!("file:///{source}")
        };
        fixture
            .git(&["clone", "--depth", "1", &url, "shallow-clone"])
            .expect("create shallow clone");
        let clone_root = fixture.path().join("shallow-clone");
        assert!(
            clone_root.join(".git/shallow").exists(),
            "the clone must be genuinely shallow"
        );
        let boundary_revision = fixture
            .git_stdout(&["-C", "shallow-clone", "rev-parse", "HEAD"])
            .expect("read shallow HEAD")
            .trim()
            .to_owned();

        let provider = GitProvider::open(&clone_root).expect("shallow clone is a working tree");
        let history = provider
            .revisions_touching(&clone_root.join(PATH), 10)
            .await
            .expect("a shallow boundary terminates the walk instead of erroring");
        assert_eq!(
            history
                .commits
                .iter()
                .map(|commit| commit.rev.as_str())
                .collect::<Vec<_>>(),
            [boundary_revision.as_str()],
            "exactly the boundary commit is reported, as git log does"
        );
        assert!(
            history.truncated,
            "a shallow boundary must expose that older history was not exhausted"
        );
    }

    /// Gate artifact 18 (cycle 2, M1): a tree entry that EXISTS but whose
    /// object cannot be read as a blob (gitlink/submodule commit oid whose
    /// object is absent — same shape as a `--filter=blob:none` partial
    /// clone or a corrupt object store) must surface as `Operation`, never
    /// `NotFound`. Consumers treat `NotFound` as deterministic "path absent
    /// at this revision" and cache it permanently; a read failure mapped
    /// there would manufacture false `removed` transitions that never heal.
    #[tokio::test]
    async fn read_at_reports_unreadable_blob_as_operation_not_absence() {
        use super::GitProvider;
        use code_graph_vcs::{VcsError, VcsProvider};

        let fixture = Fixture::build(initial_only_script()).expect("fixture must build");
        // A gitlink entry (mode 160000) whose commit object does not exist
        // in this repository — exactly how a submodule rides in a tree.
        fixture
            .git(&[
                "update-index",
                "--add",
                "--cacheinfo",
                "160000,0123456789abcdef0123456789abcdef01234567,vendored/module",
            ])
            .expect("stage gitlink entry");
        fixture
            .commit_staged("add gitlink entry", "2001-01-02T00:00:00+0000")
            .expect("commit gitlink entry");

        let provider = GitProvider::open(fixture.path()).expect("fixture is a Git working tree");
        let head = provider
            .resolve_rev(Some("HEAD"))
            .await
            .expect("resolve HEAD");

        let unreadable = provider
            .read_at(&head, Path::new("vendored/module"))
            .await
            .expect_err("a gitlink entry is not a readable blob");
        assert!(
            matches!(unreadable, VcsError::Operation(_)),
            "an existing-but-unreadable entry is an Operation failure, got: {unreadable:?}"
        );

        let absent = provider
            .read_at(&head, Path::new("src/never_existed.rs"))
            .await
            .expect_err("an absent path has no bytes");
        assert!(
            matches!(absent, VcsError::NotFound(_)),
            "genuine path absence keeps the NotFound contract, got: {absent:?}"
        );
    }

    /// Missing revision objects and blobs are operational failures. In
    /// particular, a partial clone can advertise a tree entry while the blob
    /// is still absent locally; neither that nor a pruned loose object may be
    /// described to history consumers as "no history".
    #[tokio::test]
    async fn revision_and_pruned_blob_failures_are_operations_not_absence() {
        use super::GitProvider;
        use code_graph_vcs::{VcsError, VcsProvider};

        let fixture = Fixture::build(initial_only_script()).expect("fixture must build");
        let provider = GitProvider::open(fixture.path()).expect("fixture is a Git working tree");

        let missing_revision = provider
            .resolve_rev(Some("definitely-not-a-revision"))
            .await
            .expect_err("an unresolved revision is not a path absence");
        assert!(
            matches!(missing_revision, VcsError::Operation(_)),
            "revision parse/object/peel failures must be operational: {missing_revision:?}"
        );

        let head = provider
            .resolve_rev(Some("HEAD"))
            .await
            .expect("resolve HEAD");
        let blob_oid = fixture
            .git_stdout(&["rev-parse", "HEAD:src/calculator.rs"])
            .expect("resolve fixture blob")
            .trim()
            .to_owned();
        let object = fixture
            .path()
            .join(".git/objects")
            .join(&blob_oid[..2])
            .join(&blob_oid[2..]);
        assert!(object.exists(), "fixture blob must be loose before pruning");
        std::fs::remove_file(&object).expect("prune fixture blob object");

        let pruned_blob = provider
            .read_at(&head, Path::new("src/calculator.rs"))
            .await
            .expect_err("a tree entry with a missing blob is unreadable");
        assert!(
            matches!(pruned_blob, VcsError::Operation(_)),
            "partial-clone/pruned/missing-blob failures must not become no-history: {pruned_blob:?}"
        );
    }

    /// A pruned parent commit is corruption unless the current commit is
    /// explicitly listed in Git's shallow boundary metadata.
    #[tokio::test]
    async fn revisions_touching_reports_pruned_non_shallow_parent_as_operation() {
        use super::GitProvider;
        use code_graph_vcs::{VcsError, VcsProvider};

        let fixture = Fixture::build(provider_script()).expect("fixture must build");
        let parent = fixture
            .git_stdout(&["rev-parse", "HEAD^"])
            .expect("resolve fixture parent")
            .trim()
            .to_owned();
        let object = fixture
            .path()
            .join(".git/objects")
            .join(&parent[..2])
            .join(&parent[2..]);
        assert!(object.exists(), "fixture parent must be a loose object");
        std::fs::remove_file(&object).expect("prune non-shallow parent commit");

        let provider = GitProvider::open(fixture.path()).expect("fixture is a Git working tree");
        let path = fixture.path().join("src/calculator.rs");
        let error = provider
            .revisions_touching(&path, 10)
            .await
            .expect_err("a pruned non-shallow parent is not a history boundary");
        assert!(
            matches!(error, VcsError::Operation(_)),
            "pruned commit object must be operational, got: {error:?}"
        );
    }

    /// A real `--filter=blob:none` clone retains the commit tree while its
    /// source blob is a promisor object. gix must report that unreadable blob
    /// as an operational failure, never as an absent path/no-history result.
    #[tokio::test]
    async fn partial_clone_missing_blob_is_operation_not_absence() {
        use super::GitProvider;
        use code_graph_vcs::{VcsError, VcsProvider};

        let fixture = Fixture::build(initial_only_script()).expect("fixture must build");
        fixture
            .git(&["config", "uploadpack.allowFilter", "true"])
            .expect("allow local partial-clone filtering");
        let source = fixture.path().to_string_lossy().replace('\\', "/");
        let url = if source.starts_with('/') {
            format!("file://{source}")
        } else {
            format!("file:///{source}")
        };
        fixture
            .git(&[
                "clone",
                "--filter=blob:none",
                "--no-checkout",
                &url,
                "partial-clone",
            ])
            .expect("create blob-filtered clone");
        let clone_root = fixture.path().join("partial-clone");
        let provider = GitProvider::open(&clone_root).expect("partial clone has a work tree");
        let head = provider
            .resolve_rev(Some("HEAD"))
            .await
            .expect("resolve partial-clone HEAD");

        let error = provider
            .read_at(&head, Path::new("src/calculator.rs"))
            .await
            .expect_err("filtered source blob must not be treated as path absence");
        assert!(
            matches!(error, VcsError::Operation(_)),
            "partial clone's missing blob is operational, got: {error:?}"
        );
    }

    /// AC-36 letter: a second provider registers ALONGSIDE the git provider
    /// without editing it, and detection selects each for its own tree.
    #[tokio::test]
    async fn registry_selects_git_alongside_an_independent_second_provider() {
        use code_graph_vcs::{
            BlameHunk, ProviderDetection, RevId, VcsError, VcsProvider, VcsRegistry,
        };

        struct MarkerProvider;

        #[async_trait::async_trait]
        impl VcsProvider for MarkerProvider {
            fn id(&self) -> &'static str {
                "marker-test"
            }
            fn detect(&self, working_tree: &Path) -> ProviderDetection {
                if working_tree.join(".marker-vcs").is_dir() {
                    ProviderDetection::Selected
                } else {
                    ProviderDetection::NoMatch
                }
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
            ) -> Result<code_graph_vcs::RevisionWindow, VcsError> {
                Ok(code_graph_vcs::RevisionWindow {
                    commits: Vec::new(),
                    truncated: false,
                })
            }
            async fn read_at(&self, _rev: &RevId, _path: &Path) -> Result<Vec<u8>, VcsError> {
                Ok(Vec::new())
            }
            async fn resolve_rev(&self, spec: Option<&str>) -> Result<RevId, VcsError> {
                Ok(RevId::new(spec.unwrap_or("default")))
            }
        }

        let fixture = Fixture::build(initial_only_script()).expect("fixture must build");
        let marker_tree = tempfile::tempdir().expect("marker tree");
        fs::create_dir(marker_tree.path().join(".marker-vcs")).expect("marker dir");

        let mut registry = VcsRegistry::new();
        registry
            .register(Box::new(
                super::GitProvider::open(fixture.path()).expect("open fixture"),
            ))
            .unwrap();
        registry.register(Box::new(MarkerProvider)).unwrap();

        assert!(
            matches!(
                registry.detect(fixture.path()).await.unwrap(),
                code_graph_vcs::VcsDetection::Selected(provider) if provider.id() == "git"
            ),
            "the git tree selects the git provider"
        );
        assert!(
            matches!(
                registry.detect(marker_tree.path()).await.unwrap(),
                code_graph_vcs::VcsDetection::Selected(provider) if provider.id() == "marker-test"
            ),
            "the marker tree selects the second provider without touching git"
        );
        assert!(
            matches!(
                registry
                    .detect(
                        std::env::temp_dir()
                            .join("code-graph-nonexistent-vcs-probe")
                            .as_path()
                    )
                    .await
                    .unwrap(),
                code_graph_vcs::VcsDetection::NoProvider
            ),
            "an unrecognized tree selects nothing"
        );
    }

    /// Gate-review F1: a provider bound to repository A must not answer for
    /// files owned by repository B — neither by detection nor by operating
    /// on them through the outer tree's lexical namespace.
    #[tokio::test]
    async fn provider_refuses_files_owned_by_a_different_repository() {
        use code_graph_vcs::{ProviderDetection, VcsDetection, VcsError, VcsProvider, VcsRegistry};

        const PATH: &str = "src/calculator.rs";
        let bound = Fixture::build(initial_only_script()).expect("bound fixture");
        let other = Fixture::build(initial_only_script()).expect("other fixture");
        let provider = super::GitProvider::open(bound.path()).expect("open bound fixture");

        assert!(
            provider.detect(other.path()) == ProviderDetection::BoundElsewhere,
            "detection is an identity check on the bound tree, not 'any git tree'"
        );
        let mut registry = VcsRegistry::new();
        registry.register(Box::new(provider.clone())).unwrap();
        assert!(matches!(
            registry.detect(other.path()).await.unwrap(),
            VcsDetection::ProviderBoundElsewhere { provider: "git" }
        ));

        let foreign = other.path().join(PATH);
        let error = provider
            .blame(&foreign, Some((1, 3)), None)
            .await
            .expect_err("a foreign file must not blame through the bound repository");
        assert!(
            matches!(&error, VcsError::NotFound(reason)
                if reason.contains("belongs to a different repository")),
            "the refusal names the real cause: {error}"
        );
    }

    /// A linked worktree shares the repository object store but is still a
    /// separate checkout: a provider bound to the main worktree must not be
    /// selected for it.
    #[tokio::test]
    async fn provider_classifies_linked_worktree_as_different_checkout() {
        use code_graph_vcs::{ProviderDetection, VcsDetection, VcsProvider, VcsRegistry};

        let fixture = Fixture::build(initial_only_script()).expect("fixture");
        fixture
            .git(&["worktree", "add", "--detach", "linked"])
            .expect("create linked worktree");
        let linked = fixture.path().join("linked");
        let provider = super::GitProvider::open(fixture.path()).expect("open main worktree");
        assert_eq!(
            provider.detect(&linked),
            ProviderDetection::DifferentCheckout,
            "linked worktrees share a repository but not a checkout"
        );

        let mut registry = VcsRegistry::new();
        registry.register(Box::new(provider)).unwrap();
        assert!(matches!(
            registry.detect(&linked).await.unwrap(),
            VcsDetection::DifferentCheckout { provider: "git" }
        ));
    }

    /// A missing file below a deleted nested clone must not fall back to an
    /// outer tree entry with the same lexical path. There is no retained
    /// ownership metadata once the clone is fully deleted, so this
    /// intentionally conservative rule also refuses an ordinary deleted
    /// directory when direct-parent discovery fails.
    #[tokio::test]
    async fn ownership_fails_closed_for_deleted_nested_clone_over_outer_shadow() {
        use code_graph_vcs::{VcsError, VcsProvider};

        let fixture = Fixture::build(initial_only_script()).expect("fixture");
        let shadow = fixture.path().join("nested/src/calculator.rs");
        fs::create_dir_all(shadow.parent().expect("shadow parent")).expect("create shadow");
        fs::write(&shadow, "outer shadow\n").expect("write outer shadow");
        fixture
            .git(&["add", "nested/src/calculator.rs"])
            .expect("stage shadow");
        fixture
            .commit_staged("add outer shadow", "2001-01-02T00:00:00+0000")
            .expect("commit shadow");
        fixture.git(&["init", "nested"]).expect("init nested clone");
        fs::remove_dir_all(fixture.path().join("nested")).expect("delete nested clone");

        let provider = super::GitProvider::open(fixture.path()).expect("open outer");
        let head = provider.resolve_rev(None).await.expect("resolve default");
        let read_error = provider
            .read_at(&head, &shadow)
            .await
            .expect_err("missing nested path must not read outer shadow");
        assert!(
            matches!(read_error, VcsError::NotFound(ref reason) if reason.contains("cannot verify repository ownership"))
        );
        let history_error = provider
            .revisions_touching(&shadow, 10)
            .await
            .expect_err("missing nested path must not walk outer shadow history");
        assert!(
            matches!(history_error, VcsError::NotFound(ref reason) if reason.contains("cannot verify repository ownership"))
        );
        let blame_error = provider
            .blame(&shadow, Some((1, 1)), Some(&head))
            .await
            .expect_err("missing nested path must not blame outer shadow");
        assert!(
            matches!(blame_error, VcsError::NotFound(ref reason) if reason.contains("cannot verify repository ownership"))
        );
    }

    /// A malformed gitlink/worktree marker blocks parent discovery. If its
    /// nested file is missing, do not let the outer repository's historical
    /// entry shadow it through lexical containment.
    #[tokio::test]
    async fn ownership_fails_closed_for_broken_gitlink_over_outer_shadow() {
        use code_graph_vcs::{VcsError, VcsProvider};

        let fixture = Fixture::build(initial_only_script()).expect("fixture");
        let shadow = fixture.path().join("nested/src/calculator.rs");
        fs::create_dir_all(shadow.parent().expect("shadow parent")).expect("create shadow");
        fs::write(&shadow, "outer shadow\n").expect("write outer shadow");
        fixture
            .git(&["add", "nested/src/calculator.rs"])
            .expect("stage shadow");
        fixture
            .commit_staged("add outer shadow", "2001-01-02T00:00:00+0000")
            .expect("commit shadow");
        fs::remove_file(&shadow).expect("remove working file");
        fs::write(
            fixture.path().join("nested/.git"),
            "gitdir: missing-git-dir\n",
        )
        .expect("write broken gitlink marker");

        let provider = super::GitProvider::open(fixture.path()).expect("open outer");
        let head = provider.resolve_rev(None).await.expect("resolve default");
        let error = provider
            .read_at(&head, &shadow)
            .await
            .expect_err("broken nested Git marker must not read outer shadow");
        assert!(
            matches!(error, VcsError::NotFound(ref reason) if reason.contains("cannot verify repository ownership"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn dangling_nested_git_marker_is_an_ownership_boundary() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("root");
        let nested = root.path().join("nested");
        fs::create_dir(&nested).expect("nested directory");
        symlink("missing-git-dir", nested.join(".git")).expect("dangling Git marker");

        assert!(
            super::has_nested_git_marker(root.path(), &nested.join("src"))
                .expect("symlink metadata succeeds"),
            "a dangling marker must not be mistaken for no nested repository"
        );
    }

    /// Gate-review F3/F4: blame requests only the span's range, and a span
    /// beyond the blamed revision's EOF clamps instead of erroring.
    #[tokio::test]
    async fn blame_clamps_spans_to_the_revision_eof() {
        use code_graph_vcs::VcsProvider;

        const PATH: &str = "src/calculator.rs";
        let fixture = Fixture::build(initial_only_script()).expect("fixture must build");
        let provider = super::GitProvider::open(fixture.path()).expect("open fixture");
        let file = fixture.path().join(PATH);
        let line_count = super::blob_line_count(&fs::read(&file).expect("read fixture file"));
        assert!(line_count > 1, "fixture file spans multiple lines");

        let beyond = provider
            .blame(&file, Some((line_count + 100, line_count + 200)), None)
            .await
            .expect("a span wholly beyond EOF is empty, not an error");
        assert!(beyond.is_empty());

        let clamped = provider
            .blame(&file, Some((1, line_count + 500)), None)
            .await
            .expect("a span overhanging EOF clamps");
        let last_attributed = clamped
            .iter()
            .map(|hunk| hunk.start_line + hunk.line_count - 1)
            .max()
            .expect("clamped blame attributes at least one line");
        assert_eq!(
            last_attributed, line_count,
            "attribution stops at the revision's last line"
        );
    }

    #[test]
    fn git_backend_dependency_is_confined_to_this_provider_crate() {
        let crate_manifest = include_str!("../Cargo.toml");
        let workspace_manifest = include_str!("../../../Cargo.toml");

        assert!(crate_manifest.contains("gix = \"0.86.0\""));
        assert!(!crate_manifest.contains("git2"));
        assert!(!crate_manifest.contains("libgit2"));
        assert!(!workspace_manifest.contains("gix ="));
        assert!(!workspace_manifest.contains("git2 ="));
    }
}
