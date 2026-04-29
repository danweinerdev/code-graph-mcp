//! Indexer + progress reporting plumbing.
//!
//! Phase 3.3 fills in the orchestration (per-job rayon pool, language-aware
//! edge resolution, tokio progress bridge). Phase 3.1 ships only the
//! [`ProgressSink`] trait so Phase 3.2 (discovery) and Phase 3.3 (indexer)
//! can both depend on it without circularity — the Phase 3.2 walker reports
//! "Discovered N files across M languages" through this trait, and the
//! Phase 3.3 parser pool reports per-file parse progress through the same
//! trait.

/// Reports incremental progress from long-running operations (discovery,
/// parsing). Implementations live behind a trait object so the producer
/// (the rayon worker thread) doesn't need to know whether the consumer is
/// a tokio mpsc bridge, a no-op sink for tests, or a stdout printer for
/// the parse-test binary.
///
/// `report` is called by best-effort senders — implementations should not
/// block. The Phase 3.3 `ChannelProgressSink` will forward to a tokio
/// channel via `try_send`, dropping events on a full channel rather than
/// blocking the parsing pool.
pub trait ProgressSink: Send + Sync {
    /// Report progress as `current` of `total` units complete with a
    /// human-readable status message. `total = 0` means "indeterminate
    /// progress"; callers should display only the message in that case.
    fn report(&self, current: u32, total: u32, message: &str);
}
