#![forbid(unsafe_code)]

//! Version-control provider abstraction and registry.
//!
//! This crate deliberately contains no VCS backend. Providers own their
//! backend dependencies and register with [`VcsRegistry`]. In particular,
//! [`RevId`] is an opaque token: this crate never assumes a hash shape,
//! length, or alphabet.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// An opaque revision identity supplied by a [`VcsProvider`].
///
/// The inner token is intentionally private. It can represent a git object
/// name, an integer changelist, a revision specifier, or another provider's
/// native identity without callers assigning it any VCS-specific meaning.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct RevId(String);

impl RevId {
    /// Construct an opaque token in a provider implementation.
    ///
    /// Callers should obtain revisions through [`VcsProvider::resolve_rev`]
    /// and pass them unchanged to other provider operations.
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// Return the provider-owned token for passing to that provider's backend.
    ///
    /// This does not validate, parse, or otherwise assign meaning to the
    /// token. Consumers outside a provider should treat it as display-only.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RevId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Metadata for one revision that changed a file.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Commit {
    /// Provider-native revision identity.
    pub rev: RevId,
    /// Provider-reported author display value.
    pub author: String,
    /// Provider-reported UTC Unix timestamp in seconds.
    pub timestamp_utc: i64,
    /// Provider-reported one-line revision summary.
    pub summary: String,
}

/// A bounded window of revisions that changed one file, newest first.
///
/// `truncated` distinguishes "the provider stopped before exhausting older
/// history" (for example, the git revwalk cap or a shallow-clone boundary)
/// from the window merely filling to the requested `limit` — a caller can
/// detect the latter itself via `commits.len() == limit`, but only the
/// provider knows about the former. Consumers building user-facing results
/// must surface both states rather than presenting an incomplete walk as
/// complete history.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RevisionWindow {
    /// Up to `limit` revisions that changed the path, newest first.
    pub commits: Vec<Commit>,
    /// The provider stopped before exhausting older history.
    pub truncated: bool,
}

/// Attribution for a contiguous source-line range.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlameHunk {
    /// Provider-native revision identity responsible for the range.
    pub rev: RevId,
    /// Provider-reported author display value.
    pub author: String,
    /// Provider-reported UTC Unix timestamp in seconds.
    pub timestamp_utc: i64,
    /// One-based first line in the blamed file.
    pub start_line: u32,
    /// Number of source lines in this attribution range.
    pub line_count: u32,
}

/// Errors returned by a [`VcsProvider`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VcsError {
    /// No usable working copy is available for this provider.
    #[error("VCS unavailable: {0}")]
    Unavailable(String),
    /// The provider could not resolve a revision or file.
    #[error("VCS object not found: {0}")]
    NotFound(String),
    /// A provider backend failed while servicing an operation.
    #[error("VCS operation failed: {0}")]
    Operation(String),
    /// A provider encountered filesystem I/O while servicing an operation.
    #[error("VCS I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// How one registered provider classifies a working-tree probe.
///
/// Detection is metadata rather than one of the four required asynchronous
/// VCS operations. [`VcsRegistry::detect`] runs it on Tokio's blocking pool:
/// repository discovery may touch the filesystem (or, for a future provider,
/// invoke its native discovery mechanism).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderDetection {
    /// This provider does not recognize the probe.
    NoMatch,
    /// The probe is the checkout this provider is bound to.
    Selected,
    /// The provider recognizes the VCS but is bound to another repository.
    BoundElsewhere,
    /// The provider recognizes the same repository in another checkout.
    DifferentCheckout,
}

/// Result of detecting a provider for a history request.
#[derive(Clone)]
pub enum VcsDetection {
    /// A provider bound to the requested checkout.
    Selected(Arc<dyn VcsProvider>),
    /// No registered provider recognized the requested root.
    NoProvider,
    /// A registered provider recognized the VCS, but its bound repository is
    /// not the requested repository.
    ProviderBoundElsewhere { provider: &'static str },
    /// A registered provider recognized the same repository in a linked (or
    /// otherwise separate) checkout.
    DifferentCheckout { provider: &'static str },
}

/// A source-control backend for one kind of working tree.
///
/// The four async methods are the complete required VCS operation set:
/// line-range blame, file revision history, reading a file at a revision, and
/// resolving a revision specifier. [`Self::id`] and [`Self::detect`] are
/// provider metadata used only for registry selection.
#[async_trait]
pub trait VcsProvider: Send + Sync {
    /// Stable identifier for this provider implementation (for example,
    /// `"git"` or `"perforce"`).
    fn id(&self) -> &'static str;

    /// Classify `working_tree` relative to this provider's bound checkout.
    ///
    /// This synchronous hook is always called through
    /// [`VcsRegistry::detect`]'s blocking boundary, never directly from a
    /// history tool's async task.
    fn detect(&self, working_tree: &Path) -> ProviderDetection;

    /// Attribute an optional inclusive one-based line range at an optional
    /// revision.
    ///
    /// `lines: None` selects the whole file. `at: None` selects the
    /// provider's **default revision** — for Git that is `HEAD`, so
    /// attribution reflects the committed state, never uncommitted
    /// working-tree edits. A caller whose line numbers come from the
    /// on-disk file must detect divergence itself (for example by
    /// comparing the on-disk contents against [`Self::read_at`] for the
    /// blamed revision) rather than assume the two line spaces agree.
    async fn blame(
        &self,
        path: &Path,
        lines: Option<(u32, u32)>,
        at: Option<&RevId>,
    ) -> Result<Vec<BlameHunk>, VcsError>;

    /// List up to `limit` revisions that changed `path`, newest first, with
    /// an explicit signal when the provider stopped before exhausting older
    /// history (independent of the caller's `limit`).
    async fn revisions_touching(&self, path: &Path, limit: u32)
        -> Result<RevisionWindow, VcsError>;

    /// Read `path` as it existed at `rev`.
    async fn read_at(&self, rev: &RevId, path: &Path) -> Result<Vec<u8>, VcsError>;

    /// Resolve an optional provider-native revision specifier to its opaque
    /// identity. `None` asks the provider for its default revision.
    async fn resolve_rev(&self, spec: Option<&str>) -> Result<RevId, VcsError>;
}

/// Registration failures from [`VcsRegistry`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RegistryError {
    /// A provider with the same stable identifier is already registered.
    #[error("VCS provider {0:?} is already registered")]
    DuplicateProvider(String),
}

/// Registered VCS providers with working-tree detection.
///
/// Detection uses registration order, so callers can make precedence explicit
/// when provider markers overlap. Adding another provider requires only
/// registering it; existing providers are not modified.
pub struct VcsRegistry {
    providers: Vec<Arc<dyn VcsProvider>>,
}

impl VcsRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    /// Register a provider, rejecting duplicate provider identifiers.
    pub fn register(&mut self, provider: Box<dyn VcsProvider>) -> Result<(), RegistryError> {
        if self
            .providers
            .iter()
            .any(|registered| registered.id() == provider.id())
        {
            return Err(RegistryError::DuplicateProvider(provider.id().to_string()));
        }
        self.providers.push(Arc::from(provider));
        Ok(())
    }

    /// Detect a provider for `working_tree` without blocking the async
    /// runtime. Detection intentionally runs on every request: binding can
    /// change as repositories, worktrees, and provider state change.
    pub async fn detect(&self, working_tree: &Path) -> Result<VcsDetection, VcsError> {
        let providers = self.providers.clone();
        let working_tree = PathBuf::from(working_tree);
        match tokio::task::spawn_blocking(move || {
            for provider in providers {
                match provider.detect(&working_tree) {
                    ProviderDetection::Selected => return VcsDetection::Selected(provider),
                    ProviderDetection::BoundElsewhere => {
                        return VcsDetection::ProviderBoundElsewhere {
                            provider: provider.id(),
                        }
                    }
                    ProviderDetection::DifferentCheckout => {
                        return VcsDetection::DifferentCheckout {
                            provider: provider.id(),
                        }
                    }
                    ProviderDetection::NoMatch => {}
                }
            }
            VcsDetection::NoProvider
        })
        .await
        {
            Ok(detection) => Ok(detection),
            Err(error) => Err(VcsError::Operation(format!(
                "VCS detection task failed: {error}"
            ))),
        }
    }

    /// Look up a registered provider by its stable identifier.
    pub fn provider(&self, id: &str) -> Option<&dyn VcsProvider> {
        self.providers
            .iter()
            .map(AsRef::as_ref)
            .find(|provider| provider.id() == id)
    }

    /// Iterate over registered providers in detection precedence order.
    pub fn providers(&self) -> impl Iterator<Item = &dyn VcsProvider> + '_ {
        self.providers.iter().map(AsRef::as_ref)
    }
}

impl Default for VcsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Notify;

    /// A Perforce-shaped test provider: its native revision model is an
    /// integer changelist, converted only to the opaque token at the trait
    /// boundary. No caller needs a different trait method or wire type.
    struct IntegerRevisionProvider {
        changelist: i64,
    }

    impl IntegerRevisionProvider {
        fn revision(&self) -> RevId {
            RevId::new(self.changelist.to_string())
        }
    }

    #[async_trait]
    impl VcsProvider for IntegerRevisionProvider {
        fn id(&self) -> &'static str {
            "integer-test"
        }

        fn detect(&self, working_tree: &Path) -> ProviderDetection {
            if working_tree.join(".integer-vcs").is_dir() {
                ProviderDetection::Selected
            } else {
                ProviderDetection::NoMatch
            }
        }

        async fn blame(
            &self,
            _path: &Path,
            lines: Option<(u32, u32)>,
            _at: Option<&RevId>,
        ) -> Result<Vec<BlameHunk>, VcsError> {
            let (start_line, end_line) = lines.unwrap_or((1, 1));
            Ok(vec![BlameHunk {
                rev: self.revision(),
                author: "integer-provider".to_string(),
                timestamp_utc: 0,
                start_line,
                line_count: end_line.saturating_sub(start_line).saturating_add(1),
            }])
        }

        async fn revisions_touching(
            &self,
            _path: &Path,
            limit: u32,
        ) -> Result<RevisionWindow, VcsError> {
            Ok(RevisionWindow {
                commits: (limit > 0)
                    .then(|| Commit {
                        rev: self.revision(),
                        author: "integer-provider".to_string(),
                        timestamp_utc: 0,
                        summary: "integer changelist".to_string(),
                    })
                    .into_iter()
                    .collect(),
                truncated: false,
            })
        }

        async fn read_at(&self, _rev: &RevId, _path: &Path) -> Result<Vec<u8>, VcsError> {
            Ok(b"integer provider contents".to_vec())
        }

        async fn resolve_rev(&self, _spec: Option<&str>) -> Result<RevId, VcsError> {
            Ok(self.revision())
        }
    }

    /// A test provider that recognizes a distinct working-tree marker.
    struct MarkerProvider {
        id: &'static str,
        marker: &'static str,
    }

    #[async_trait]
    impl VcsProvider for MarkerProvider {
        fn id(&self) -> &'static str {
            self.id
        }

        fn detect(&self, working_tree: &Path) -> ProviderDetection {
            if working_tree.join(self.marker).is_dir() {
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
        ) -> Result<RevisionWindow, VcsError> {
            Ok(RevisionWindow {
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

    /// A provider whose four operations await a gate, modelling a network
    /// request without occupying the runtime thread while it is pending.
    #[derive(Clone)]
    struct SlowProvider {
        started: Arc<Notify>,
        release: Arc<Notify>,
    }

    #[derive(Clone)]
    struct BlockingDetectionProvider {
        started: Arc<AtomicBool>,
        calls: Arc<AtomicUsize>,
        classification: ProviderDetection,
        panic: bool,
        delay: bool,
    }

    #[async_trait]
    impl VcsProvider for BlockingDetectionProvider {
        fn id(&self) -> &'static str {
            "blocking-detection"
        }

        fn detect(&self, _working_tree: &Path) -> ProviderDetection {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.started.store(true, Ordering::SeqCst);
            assert!(!self.panic, "detector panic fixture");
            if self.delay {
                std::thread::sleep(Duration::from_millis(100));
            }
            self.classification
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
            Ok(RevisionWindow {
                commits: Vec::new(),
                truncated: false,
            })
        }

        async fn read_at(&self, _rev: &RevId, _path: &Path) -> Result<Vec<u8>, VcsError> {
            Ok(Vec::new())
        }

        async fn resolve_rev(&self, _spec: Option<&str>) -> Result<RevId, VcsError> {
            Ok(RevId::new("default"))
        }
    }

    impl SlowProvider {
        async fn wait(&self) {
            self.started.notify_one();
            self.release.notified().await;
        }
    }

    #[async_trait]
    impl VcsProvider for SlowProvider {
        fn id(&self) -> &'static str {
            "slow-test"
        }

        fn detect(&self, _working_tree: &Path) -> ProviderDetection {
            ProviderDetection::NoMatch
        }

        async fn blame(
            &self,
            _path: &Path,
            _lines: Option<(u32, u32)>,
            _at: Option<&RevId>,
        ) -> Result<Vec<BlameHunk>, VcsError> {
            self.wait().await;
            Ok(Vec::new())
        }

        async fn revisions_touching(
            &self,
            _path: &Path,
            _limit: u32,
        ) -> Result<RevisionWindow, VcsError> {
            self.wait().await;
            Ok(RevisionWindow {
                commits: Vec::new(),
                truncated: false,
            })
        }

        async fn read_at(&self, _rev: &RevId, _path: &Path) -> Result<Vec<u8>, VcsError> {
            self.wait().await;
            Ok(Vec::new())
        }

        async fn resolve_rev(&self, spec: Option<&str>) -> Result<RevId, VcsError> {
            self.wait().await;
            Ok(RevId::new(spec.unwrap_or("default")))
        }
    }

    #[test]
    fn vcs_provider_is_object_safe() {
        fn assert_object_safe<T: ?Sized>() {}
        assert_object_safe::<dyn VcsProvider>();
    }

    #[tokio::test]
    async fn integer_revision_provider_uses_opaque_revision_tokens() {
        let provider = IntegerRevisionProvider { changelist: 42_424 };
        let path = Path::new("//depot/project/lib.rs");

        // Calling all four required operations on an integer-native provider
        // makes a future fifth *required* operation a compile failure here.
        let revision = provider.resolve_rev(Some("latest")).await.unwrap();
        let blame = provider
            .blame(path, Some((4, 6)), Some(&revision))
            .await
            .unwrap();
        let window = provider.revisions_touching(path, 1).await.unwrap();
        let contents = provider.read_at(&revision, path).await.unwrap();

        assert_eq!(revision.as_str(), "42424");
        assert_eq!(blame[0].rev, revision);
        assert_eq!(blame[0].start_line, 4);
        assert_eq!(blame[0].line_count, 3);
        assert!(!window.truncated);
        assert_eq!(window.commits[0].rev, revision);
        assert_eq!(contents, b"integer provider contents");
    }

    #[tokio::test]
    async fn registry_selects_an_independently_registered_second_provider() {
        let temporary_tree = tempfile::tempdir().unwrap();
        std::fs::create_dir(temporary_tree.path().join(".second-vcs")).unwrap();

        let mut registry = VcsRegistry::new();
        registry
            .register(Box::new(MarkerProvider {
                id: "first",
                marker: ".first-vcs",
            }))
            .unwrap();
        registry
            .register(Box::new(MarkerProvider {
                id: "second",
                marker: ".second-vcs",
            }))
            .unwrap();

        assert!(
            matches!(
                registry.detect(temporary_tree.path()).await.unwrap(),
                VcsDetection::Selected(provider) if provider.id() == "second"
            ),
            "detection must consult the independently registered provider"
        );
        assert_eq!(
            registry.provider("first").map(VcsProvider::id),
            Some("first")
        );
    }

    #[tokio::test]
    async fn duplicate_provider_registration_is_rejected_without_replacement() {
        let temporary_tree = tempfile::tempdir().unwrap();
        std::fs::create_dir(temporary_tree.path().join(".one")).unwrap();

        let mut registry = VcsRegistry::new();
        registry
            .register(Box::new(MarkerProvider {
                id: "same",
                marker: ".one",
            }))
            .unwrap();
        let error = registry
            .register(Box::new(MarkerProvider {
                id: "same",
                marker: ".two",
            }))
            .unwrap_err();

        assert!(matches!(error, RegistryError::DuplicateProvider(id) if id == "same"));
        assert_eq!(registry.providers().count(), 1);
        assert!(
            matches!(
                registry.detect(temporary_tree.path()).await.unwrap(),
                VcsDetection::Selected(provider) if provider.id() == "same"
            ),
            "the rejected duplicate must not replace the original provider"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn slow_provider_await_does_not_block_the_runtime() {
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let provider = SlowProvider {
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        };

        let operation =
            tokio::spawn(async move { provider.resolve_rev(Some("network-rev")).await });
        started.notified().await;
        assert!(
            !operation.is_finished(),
            "the provider must still be waiting when the unrelated task runs"
        );

        let probe = tokio::time::timeout(Duration::from_millis(50), async {
            let (sent, received) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                sent.send(()).expect("probe receiver must remain alive");
            });
            received.await.expect("probe sender must complete")
        })
        .await;
        assert!(probe.is_ok(), "the runtime must schedule unrelated work");

        release.notify_one();
        assert_eq!(operation.await.unwrap().unwrap().as_str(), "network-rev");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn detection_is_blocking_isolated_and_never_cached() {
        let started = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut registry = VcsRegistry::new();
        registry
            .register(Box::new(BlockingDetectionProvider {
                started: Arc::clone(&started),
                calls: Arc::clone(&calls),
                classification: ProviderDetection::Selected,
                panic: false,
                delay: true,
            }))
            .unwrap();
        let registry = Arc::new(registry);
        let root = PathBuf::from("/blocking-detection-root");

        let first_registry = Arc::clone(&registry);
        let first_root = root.clone();
        let detection = tokio::spawn(async move { first_registry.detect(&first_root).await });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !started.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("blocking detector started");
        assert!(
            !detection.is_finished(),
            "detection must wait in spawn_blocking rather than block the runtime"
        );
        let (sent, received) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            sent.send(()).expect("probe receiver remains alive");
        });
        tokio::time::timeout(Duration::from_millis(50), received)
            .await
            .expect("unrelated async work runs while detection blocks")
            .expect("probe completes");
        assert!(matches!(
            detection.await.unwrap().unwrap(),
            VcsDetection::Selected(_)
        ));

        assert!(matches!(
            registry.detect(&root).await.unwrap(),
            VcsDetection::Selected(_)
        ));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "each history request must execute detection; selection is not cached"
        );
    }

    #[tokio::test]
    async fn panicking_detector_is_an_operational_error() {
        let mut registry = VcsRegistry::new();
        registry
            .register(Box::new(BlockingDetectionProvider {
                started: Arc::new(AtomicBool::new(false)),
                calls: Arc::new(AtomicUsize::new(0)),
                classification: ProviderDetection::NoMatch,
                panic: true,
                delay: false,
            }))
            .unwrap();

        let error = match registry.detect(Path::new("/panicking-detector")).await {
            Ok(_) => panic!("a detector panic must not become no-provider"),
            Err(error) => error,
        };
        assert!(
            matches!(error, VcsError::Operation(ref reason) if reason.contains("detection task failed")),
            "panic becomes an operational detection error: {error}"
        );
    }

    #[tokio::test]
    async fn first_recognized_provider_wins_over_later_selection() {
        let mut registry = VcsRegistry::new();
        registry
            .register(Box::new(BlockingDetectionProvider {
                started: Arc::new(AtomicBool::new(false)),
                calls: Arc::new(AtomicUsize::new(0)),
                classification: ProviderDetection::BoundElsewhere,
                panic: false,
                delay: false,
            }))
            .unwrap();
        // Use a distinct marker provider for the later registration; it is
        // selected only if registry precedence incorrectly skips the first
        // recognized classification.
        registry
            .register(Box::new(MarkerProvider {
                id: "later-selected",
                marker: ".always-selected",
            }))
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".always-selected")).unwrap();
        assert!(matches!(
            registry.detect(root.path()).await.unwrap(),
            VcsDetection::ProviderBoundElsewhere {
                provider: "blocking-detection"
            }
        ));
    }
}
