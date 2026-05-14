//! Path normalization helpers built on top of [`dunce`].
//!
//! These wrappers exist so the rest of the workspace has one canonical name
//! for each of three semantically distinct path operations:
//!
//! - [`canonicalize`]: filesystem-bound canonicalization.
//! - [`simplify`]: infallible lexical strip.
//! - [`normalize_user_path`]: user-supplied input → best-effort canonical form
//!   with a lexical fallback on failure.
//!
//! On non-Windows targets, the [`dunce`] crate delegates to the standard
//! library and is effectively transparent. On Windows, it strips the verbatim
//! extended-path prefix (`\\?\`) whenever the short form is still valid.

use std::io;
use std::path::{Path, PathBuf};

/// Filesystem-bound canonicalization.
///
/// On Windows, returns the short `D:\...` form whenever possible; the
/// extended-path `\\?\` prefix survives only when the short form is invalid
/// (e.g. path > 260 chars, special device names).
pub fn canonicalize(p: &Path) -> io::Result<PathBuf> {
    dunce::canonicalize(p)
}

/// Infallible lexical strip.
///
/// Use on already-canonical paths (e.g. cache deserialization migration).
/// Identity on non-Windows.
pub fn simplify(p: &Path) -> PathBuf {
    dunce::simplified(p).to_path_buf()
}

/// Normalize a user-supplied file-path argument before graph lookup.
///
/// The fallback covers the stale-graph case (file deleted since indexing) and
/// any other canonicalize failure (permission, broken symlink, malformed
/// input). The worst outcome of fallback is a graph miss, never a panic.
pub fn normalize_user_path(p: &str) -> PathBuf {
    let path = Path::new(p);
    match dunce::canonicalize(path) {
        Ok(canonical) => canonical,
        Err(_) => dunce::simplified(path).to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// (a) `canonicalize` on an existing tempdir resolves to an absolute path
    /// without `\\?\` on Linux.
    #[test]
    fn canonicalize_existing_tempdir_returns_absolute_path_without_verbatim_prefix() {
        let tmp = TempDir::new().expect("create tempdir");
        let canonical = canonicalize(tmp.path()).expect("canonicalize tempdir");
        assert!(
            canonical.is_absolute(),
            "canonicalized path should be absolute: {}",
            canonical.display()
        );
        let as_str = canonical.to_string_lossy();
        assert!(
            !as_str.contains(r"\\?\"),
            "Linux canonicalize must not contain the verbatim prefix: {as_str}"
        );
    }

    /// (b) `simplify` on an already-clean Linux path is a no-op.
    #[test]
    fn simplify_on_clean_linux_path_is_noop() {
        let p = PathBuf::from("/tmp/foo/bar.h");
        let simplified = simplify(&p);
        assert_eq!(
            simplified, p,
            "simplify on an already-clean POSIX path must be identity"
        );
    }

    /// (c) `normalize_user_path` on an existing tempdir returns the canonical
    /// form.
    #[test]
    fn normalize_user_path_existing_tempdir_returns_canonical() {
        let tmp = TempDir::new().expect("create tempdir");
        let tmp_str = tmp
            .path()
            .to_str()
            .expect("tempdir path is valid UTF-8 on Linux");
        let normalized = normalize_user_path(tmp_str);
        let expected = canonicalize(tmp.path()).expect("canonicalize tempdir");
        assert_eq!(
            normalized, expected,
            "normalize_user_path on an existing path must equal canonicalize"
        );
        assert!(
            !normalized.to_string_lossy().contains(r"\\?\"),
            "Linux normalize_user_path must not contain the verbatim prefix"
        );
    }

    /// (d) `normalize_user_path` on a non-existent path falls back without
    /// panicking and the returned PathBuf round-trips through
    /// `to_string_lossy`.
    #[test]
    fn normalize_user_path_nonexistent_falls_back_without_panic() {
        let fake = "/nonexistent/path/that/does/not/exist.h";
        let normalized = normalize_user_path(fake);
        // Lexical fallback on Linux is identity, so the string must round-trip.
        assert_eq!(
            normalized.to_string_lossy(),
            fake,
            "non-existent path must fall back to lexical (identity on Linux)"
        );
        // Defensive: round-trip via to_string_lossy → PathBuf yields the same value.
        let round_tripped = PathBuf::from(normalized.to_string_lossy().as_ref());
        assert_eq!(round_tripped, normalized);
    }

    /// (e) `normalize_user_path` on a path containing `.` and `..` segments
    /// resolves them when the underlying path exists.
    #[test]
    fn normalize_user_path_resolves_dot_and_dotdot_when_path_exists() {
        let tmp = TempDir::new().expect("create tempdir");
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).expect("create sub dir");

        // Build a messy path: <tempdir>/./sub/.. which resolves back to <tempdir>.
        let messy = tmp.path().join(".").join("sub").join("..");
        let messy_str = messy.to_str().expect("messy path is valid UTF-8 on Linux");
        let normalized = normalize_user_path(messy_str);

        let expected = canonicalize(tmp.path()).expect("canonicalize tempdir");
        assert_eq!(
            normalized, expected,
            "normalize_user_path must resolve `.` and `..` segments when the path exists"
        );
        assert!(
            !normalized.to_string_lossy().contains(r"\\?\"),
            "Linux normalize_user_path must not contain the verbatim prefix"
        );
    }

    // The two `#[cfg(windows)]` tests below are the only automated checks of
    // `simplify`'s prefix behavior — one asserts the `VerbatimDisk` strip,
    // the other pins `VerbatimUNC` as a load-bearing identity (dunce only
    // strips `VerbatimDisk`). They do not compile on Linux/macOS — the
    // `#[cfg(windows)]` attribute conditionally removes them at compile time,
    // so `cargo test -p code-graph-core paths` on Linux still reports 5 tests.
    // Manual smoke on Windows before each release is the supplementary
    // verification — see Phase 4 task 4.3 of the PathNormalization plan for
    // the CI-coverage disclosure.

    /// (f, Windows-only) `simplify` strips the verbatim disk prefix
    /// (`\\?\D:\...` → `D:\...`).
    #[cfg(windows)]
    #[test]
    fn simplify_strips_extended_disk_prefix() {
        let input = PathBuf::from(r"\\?\D:\proj\file.h");
        let out = simplify(&input);
        assert_eq!(out, PathBuf::from(r"D:\proj\file.h"));
    }

    /// (g, Windows-only) `simplify` leaves verbatim UNC paths unchanged —
    /// `dunce::simplified` only strips `VerbatimDisk` prefixes.
    #[cfg(windows)]
    #[test]
    fn simplify_leaves_verbatim_unc_unchanged() {
        let input = PathBuf::from(r"\\?\UNC\server\share\file.h");
        let out = simplify(&input);
        assert_eq!(out, input);
    }
}
