//! Memory-mapped read helper. The ONE unsafe boundary in this crate.
//!
//! [`mmap_read_only`] is the only place this crate calls `unsafe`. It
//! exists because `memmap2::Mmap::map` is `unsafe fn` on every
//! platform — the OS can invalidate the mapping under concurrent file
//! modification, and there is no safe wrapper in the Rust ecosystem
//! (this is fundamental, not a library gap). The site is isolated to
//! this single function with a documented `// SAFETY:` block. See
//! `lib.rs` crate-level doc-comment for the `unsafe_code = allow`
//! rationale and `.plans/Designs/PackedCache/README.md` Decision 5.

use memmap2::Mmap;
use std::fs::File;
use std::io;
use std::path::Path;

/// Open `path` read-only and return a memory-mapped view + the owning
/// file handle. The caller MUST hold the returned `MmapHolder` for the
/// entire duration of any references into the mapped bytes — dropping
/// it unmaps the region and invalidates all derived references.
///
/// Returns `None` if the file does not exist (matches the convention
/// the load path uses to surface "no cache" as `Ok(false)`).
pub(crate) fn mmap_read_only(path: &Path) -> io::Result<Option<MmapHolder>> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };

    // Zero-byte files cannot be mmap'd. Treat as cache-absent.
    let metadata = file.metadata()?;
    if metadata.len() == 0 {
        return Ok(None);
    }

    // SAFETY: We just opened the file read-only and own the `File`
    // handle for the lifetime of the returned `MmapHolder` (which
    // bundles both). The atomic-rename write contract used by
    // `Graph::save` (write to `.tmp`, fsync, rename over the final
    // path) means concurrent writers always create a new inode rather
    // than mutating the inode we hold open — so the mapped pages stay
    // stable for the duration of our read. The only remaining UB
    // window is a process EXTERNAL to this crate truncating the open
    // file while we're reading; that scenario is out of scope for the
    // cache (no other process touches `.code-graph-cache.db`).
    //
    // On Windows, the file is opened via `File::open` which uses
    // `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE` by
    // default. A concurrent writer using `OpenOptions::write(true)
    // .share_mode(FILE_SHARE_READ)` would fail to open while we hold
    // the mmap. Since `Graph::save` does NOT use restrictive share
    // modes (it uses the default `File::create`), the rename-based
    // atomic write contract still holds: the rename succeeds even
    // while our mmap is open, but it points at a NEW inode; our mmap
    // continues to read the OLD inode until we drop it. Standard
    // POSIX-style behavior.
    let mmap = unsafe { Mmap::map(&file)? };

    Ok(Some(MmapHolder { _file: file, mmap }))
}

/// Owned mmap + file handle bundle. The mmap reference is only valid
/// while this struct is alive.
pub(crate) struct MmapHolder {
    /// Held open for the mmap's lifetime. Prefixed `_` because the
    /// field exists for its `Drop` side effect only — closing the
    /// file would invalidate `mmap` on some platforms.
    _file: File,
    mmap: Mmap,
}

impl MmapHolder {
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.mmap
    }
}
