//! StoragePath - Domain Value Object for representing storage paths
//!
//! This module contains only the StoragePath Value Object which is part of the pure domain.
//! PathService (which implements StoragePort and StorageMediator) was moved to
//! infrastructure/services/path_service.rs because it has file system dependencies.

use std::path::PathBuf;
use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfc_quick};

/// NFC-normalize a single file or folder name component.
///
/// The storage layer (PostgreSQL `storage.files.name` and
/// `storage.folders.name`) compares bytes literally — there is no
/// Unicode-aware collation in either UNIQUE index. macOS APFS stores
/// filenames in NFD (decomposed: `é` = `e` + U+0301), while browsers
/// and most other clients post NFC (`é` = U+00E9). Without
/// normalization, the same logical filename can land as two distinct
/// rows: one from a web upload, one from a NextCloud desktop client
/// re-upload of the round-tripped name. The UNIQUE index does not
/// catch it because the bytes differ.
///
/// This function is called at every name-receiving boundary (entity
/// constructors, repository path lookups) so the database invariant
/// becomes "every stored name is NFC". A one-shot migration
/// (`migrate-nfc-filenames`) cleans up rows that pre-date this rule.
///
/// Pure function — no I/O, allocates one `String`.
///
/// Fast path: `is_nfc_quick` is a per-char table lookup that answers
/// `Yes` for virtually every name already in NFC — which is every name
/// loaded back from PostgreSQL (the DB invariant above) and every
/// ASCII name. That skips the full decompose/recompose state machine
/// this function otherwise runs once per row on every listing
/// (PROPFIND, folder listing, photos timeline). `Maybe`/`No` fall
/// through to the full pipeline.
pub fn normalize_storage_name(name: &str) -> String {
    if is_nfc_quick(name.chars()) == IsNormalized::Yes {
        return name.to_string();
    }
    name.nfc().collect()
}

/// Owned-input sibling of [`normalize_storage_name`].
///
/// The borrowing variant must always allocate a fresh `String` even when
/// the input is already NFC — which is every name loaded back from
/// PostgreSQL (DB invariant) and every ASCII name. Callers that own the
/// `String` (entity constructors receive `name: String` by value) were
/// paying that copy only to drop the original immediately. This variant
/// returns the input unchanged on the fast path: zero allocations per
/// row on every listing (PROPFIND, photos timeline, search).
pub fn normalize_storage_name_owned(name: String) -> String {
    if is_nfc_quick(name.chars()) == IsNormalized::Yes {
        return name;
    }
    name.nfc().collect()
}

/// Validates a single file or folder name component.
///
/// Returns `Err` with a human-readable reason if the name is rejected.
/// Callers should wrap the reason into their own error type.
pub fn validate_storage_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("name cannot be empty");
    }
    if name.contains('/') || name.contains('\\') {
        return Err("name must not contain '/' or '\\'");
    }
    if name.contains('\0') {
        return Err("name must not contain null bytes");
    }
    if name == "." || name == ".." {
        return Err("'.' and '..' are not valid names");
    }
    Ok(())
}

/// Represents a storage path in the domain (Value Object)
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StoragePath {
    segments: Vec<String>,
}

impl StoragePath {
    /// Checks whether a single segment is safe (no traversal, no slashes)
    fn is_safe_segment(s: &str) -> bool {
        !s.is_empty() && s != "." && s != ".." && !s.contains('/')
    }

    /// Creates a new storage path, silently dropping any traversal segments
    pub fn new(segments: Vec<String>) -> Self {
        Self {
            segments: segments
                .into_iter()
                .filter(|s| Self::is_safe_segment(s))
                .collect(),
        }
    }

    /// Creates an empty path (root)
    pub fn root() -> Self {
        Self {
            segments: Vec::new(),
        }
    }

    /// Creates a path from a string with segments separated by /
    ///
    /// Traversal segments (`.`, `..`) are silently stripped to prevent
    /// path-traversal attacks.
    pub fn from_string(path: &str) -> Self {
        let segments = path
            .split('/')
            .filter(|s| Self::is_safe_segment(s))
            .map(|s| s.to_string())
            .collect();
        Self { segments }
    }

    /// One-pass builder for PG listing rows: materialized folder path +
    /// file name → `(StoragePath, path_string)`.
    ///
    /// Replaces the old per-row chain
    /// `StoragePath::from_string(&format!("{fp}/{name}"))` +
    /// `storage_path.to_string()`, which allocated a joined temporary,
    /// split it back into per-segment `String`s, and then re-joined those
    /// segments (via `join` + `write!`) into the `path_string` the DTOs
    /// actually serve. Here both representations are built in a single
    /// pass with exactly one `String` for the joined form and no
    /// intermediate temporaries.
    ///
    /// Byte-equivalence with the old chain holds because concatenating
    /// with a `/` separator distributes over `split('/')`:
    /// `(fp + "/" + name).split('/') == fp.split('/') ⧺ name.split('/')`,
    /// and the joined form is exactly `Display`'s `/`-prefixed rendering
    /// of the surviving segments (root renders as `"/"`).
    pub fn from_folder_and_name(folder_path: Option<&str>, file_name: &str) -> (Self, String) {
        let fp = folder_path.unwrap_or("");
        // Upper bounds: every byte of both inputs survives at most once,
        // plus one leading '/' per segment (≤ segment count) — sizing to
        // input length + 2 covers the worst case without a second scan.
        let mut joined = String::with_capacity(fp.len() + file_name.len() + 2);
        let mut segments: Vec<String> =
            Vec::with_capacity(fp.bytes().filter(|&b| b == b'/').count() + 2);
        for seg in fp
            .split('/')
            .chain(file_name.split('/'))
            .filter(|s| Self::is_safe_segment(s))
        {
            joined.push('/');
            joined.push_str(seg);
            segments.push(seg.to_string());
        }
        if segments.is_empty() {
            joined.push('/');
        }
        (Self { segments }, joined)
    }

    /// One-pass splitter for a pre-joined materialized path (the
    /// `storage.folders.path` column) → `(StoragePath, path_string)`.
    ///
    /// When the input is already in canonical joined form (leading `/`,
    /// no empty/`.`/`..` segments, no trailing `/`) — which is every row
    /// the repository writes — the input `String` is reused as the
    /// `path_string` with zero copies. Non-canonical inputs fall back to
    /// the filtering rebuild and produce exactly what
    /// `from_string(&path).to_string()` used to.
    pub fn from_joined(path: String) -> (Self, String) {
        if Self::is_canonical_joined(&path) {
            let segments: Vec<String> = if path.len() == 1 {
                Vec::new()
            } else {
                path[1..].split('/').map(str::to_string).collect()
            };
            return (Self { segments }, path);
        }
        // Fallback: identical to the old from_string + to_string pair.
        let segments: Vec<String> = path
            .split('/')
            .filter(|s| Self::is_safe_segment(s))
            .map(str::to_string)
            .collect();
        let sp = Self { segments };
        let joined = sp.to_path_string();
        (sp, joined)
    }

    /// `true` when `path` is exactly `Display`'s canonical rendering of
    /// its own segments: `"/"` alone, or `/seg(/seg)*` where every
    /// segment is safe. One scan, no allocations.
    fn is_canonical_joined(path: &str) -> bool {
        if path == "/" {
            return true;
        }
        if !path.starts_with('/') || path.ends_with('/') {
            return false;
        }
        path[1..].split('/').all(Self::is_safe_segment)
    }

    /// Creates a path from a PathBuf
    pub fn from(path_buf: PathBuf) -> Self {
        let segments = path_buf
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(os_str) => Some(os_str.to_string_lossy().to_string()),
                _ => None,
            })
            .collect();
        Self { segments }
    }

    /// Appends a segment to the path, consuming `self` so the existing
    /// segment buffer is reused instead of deep-cloned.
    ///
    /// Traversal segments (`.`, `..`) and segments containing `/` are
    /// silently ignored to prevent path-traversal attacks.
    pub fn join(mut self, segment: &str) -> Self {
        if Self::is_safe_segment(segment) {
            self.segments.push(segment.to_string());
        }
        self
    }

    /// Gets the file name (last segment)
    pub fn file_name(&self) -> Option<String> {
        self.segments.last().cloned()
    }

    /// Gets the parent directory path
    pub fn parent(&self) -> Option<Self> {
        if self.segments.is_empty() {
            None
        } else {
            let parent_segments = self.segments[..self.segments.len() - 1].to_vec();
            Some(Self {
                segments: parent_segments,
            })
        }
    }

    /// Checks if the path is empty (is the root)
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }
}

impl std::fmt::Display for StoragePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.segments.is_empty() {
            return f.write_str("/");
        }
        // Write segments directly — the old `self.segments.join("/")`
        // allocated a full joined temporary inside every `format!`/
        // `to_string` of a path.
        for seg in &self.segments {
            f.write_str("/")?;
            f.write_str(seg)?;
        }
        Ok(())
    }
}

impl StoragePath {
    /// The canonical joined form (`Display`'s output) in exactly one
    /// pre-sized allocation.
    ///
    /// `to_string()` routes through `Display` into an unsized `String`
    /// that grows geometrically (multiple reallocs + copies for typical
    /// path lengths). Entity constructors call this once per row on
    /// every listing, so the sized single-alloc variant is the default
    /// there.
    pub fn to_path_string(&self) -> String {
        if self.segments.is_empty() {
            return "/".to_string();
        }
        let mut s = String::with_capacity(self.segments.iter().map(|seg| seg.len() + 1).sum());
        for seg in &self.segments {
            s.push('/');
            s.push_str(seg);
        }
        s
    }

    /// Returns the path representation as a string
    pub fn as_str(&self) -> &str {
        // Note: The implementation should really store the string,
        // but here we do a temporary implementation that always returns "/"
        // This is only used for the get_folder_path_str implementation
        "/"
    }

    /// Gets the path segments
    pub fn segments(&self) -> &[String] {
        &self.segments
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_path_from_string() {
        let path = StoragePath::from_string("folder/subfolder/file.txt");
        assert_eq!(path.segments(), &["folder", "subfolder", "file.txt"]);
        assert_eq!(path.to_string(), "/folder/subfolder/file.txt");
    }

    #[test]
    fn test_storage_path_join() {
        let path = StoragePath::from_string("folder");
        let joined = path.join("file.txt");
        assert_eq!(joined.to_string(), "/folder/file.txt");
    }

    #[test]
    fn test_storage_path_parent() {
        let path = StoragePath::from_string("folder/file.txt");
        let parent = path.parent().unwrap();
        assert_eq!(parent.to_string(), "/folder");
    }

    #[test]
    fn test_storage_path_root() {
        let root = StoragePath::root();
        assert!(root.is_empty());
        assert_eq!(root.to_string(), "/");
    }

    #[test]
    fn test_storage_path_file_name() {
        let path = StoragePath::from_string("folder/file.txt");
        assert_eq!(path.file_name(), Some("file.txt".to_string()));
    }

    // ── Path-traversal hardening tests (VULN-02) ──────────────

    #[test]
    fn test_from_string_strips_dot_dot() {
        let path = StoragePath::from_string("../../etc/passwd");
        assert_eq!(path.segments(), &["etc", "passwd"]);
    }

    #[test]
    fn test_from_string_strips_single_dot() {
        let path = StoragePath::from_string("folder/./file.txt");
        assert_eq!(path.segments(), &["folder", "file.txt"]);
    }

    #[test]
    fn test_from_string_strips_mixed_traversal() {
        let path = StoragePath::from_string("a/../b/./c/../../d");
        assert_eq!(path.segments(), &["a", "b", "c", "d"]);
    }

    #[test]
    fn test_from_string_all_traversal_yields_root() {
        let path = StoragePath::from_string("../../..");
        assert!(path.is_empty());
        assert_eq!(path.to_string(), "/");
    }

    #[test]
    fn test_new_strips_traversal_segments() {
        let path = StoragePath::new(vec!["..".into(), "etc".into(), ".".into(), "passwd".into()]);
        assert_eq!(path.segments(), &["etc", "passwd"]);
    }

    #[test]
    fn test_new_strips_empty_segments() {
        let path = StoragePath::new(vec!["a".into(), "".into(), "b".into()]);
        assert_eq!(path.segments(), &["a", "b"]);
    }

    #[test]
    fn test_join_rejects_dot_dot() {
        let base = StoragePath::from_string("folder");
        let joined = base.join("..");
        // ".." is silently ignored — path stays unchanged
        assert_eq!(joined.segments(), &["folder"]);
    }

    #[test]
    fn test_join_rejects_single_dot() {
        let base = StoragePath::from_string("folder");
        let joined = base.join(".");
        assert_eq!(joined.segments(), &["folder"]);
    }

    #[test]
    fn test_join_rejects_slash_in_segment() {
        let base = StoragePath::from_string("folder");
        let joined = base.join("sub/../../etc/passwd");
        // Segment contains '/' → silently ignored
        assert_eq!(joined.segments(), &["folder"]);
    }

    #[test]
    fn test_from_pathbuf_strips_traversal() {
        let path = StoragePath::from(PathBuf::from("a/../b/./c"));
        // PathBuf Component::Normal only yields the normal parts
        // On most platforms this strips . and ..
        // but regardless, our from() only accepts Component::Normal
        assert!(!path.segments().contains(&"..".to_string()));
        assert!(!path.segments().contains(&".".to_string()));
    }

    // ── NFC normalization tests ─────────────────────────────────

    /// Plain ASCII names must round-trip identical bytes.
    #[test]
    fn test_normalize_ascii_unchanged() {
        assert_eq!(normalize_storage_name("file.txt"), "file.txt");
        assert_eq!(normalize_storage_name("My Documents"), "My Documents");
    }

    /// The macOS APFS / NextCloud-desktop pathological case: `é`
    /// decomposed as `e` + combining acute (U+0301). Stored bytes
    /// `65 cc 81` collapse to NFC `c3 a9`.
    #[test]
    fn test_normalize_nfd_to_nfc() {
        let nfd = "caf\u{0065}\u{0301}";
        let nfc = "caf\u{00E9}";
        assert_ne!(nfd.as_bytes(), nfc.as_bytes());
        assert_eq!(normalize_storage_name(nfd), nfc);
    }

    /// Already-NFC input must round-trip unchanged. This is the
    /// idempotence property the boundary normalization relies on.
    #[test]
    fn test_normalize_nfc_idempotent() {
        let nfc = "Capture d\u{2019}\u{00E9}cran.png";
        assert_eq!(normalize_storage_name(nfc), nfc);
        // And applying twice is the same as once.
        assert_eq!(normalize_storage_name(&normalize_storage_name(nfc)), nfc);
    }

    /// Multi-codepoint NFD sequences (combining acute + grave +
    /// typographic apostrophe) all converge to a single NFC form.
    #[test]
    fn test_normalize_mixed_accents() {
        let nfd = "Capture d\u{2019}\u{0065}\u{0301}cran a\u{0300}.png";
        let nfc = "Capture d\u{2019}\u{00E9}cran \u{00E0}.png";
        assert_eq!(normalize_storage_name(nfd), nfc);
    }
}
