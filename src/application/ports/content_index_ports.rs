//! Content Index Port - Application layer abstraction for full-text search
//! over file names and extracted file content.
//!
//! The implementation (an embedded Tantivy BM25 index) lives in the
//! infrastructure layer; `SearchService` only sees this port. The index is a
//! DERIVED artifact fed asynchronously by a background worker — it never sits
//! on a request path, and PostgreSQL remains the source of truth (hits are
//! re-validated and hydrated through SQL before they reach the caller, so a
//! stale index can only ever produce a dropped candidate, never a leak).

use async_trait::async_trait;
use uuid::Uuid;

use crate::common::errors::DomainError;

/// One content-index hit: a candidate file id with its BM25 score and an
/// optional plain-text snippet around the first match.
///
/// `file_id` is a CANDIDATE — callers must hydrate it through the metadata
/// repository (which re-applies user scoping, trash state and the active
/// search filters) before exposing it.
#[derive(Debug, Clone)]
pub struct ContentHitDto {
    /// File UUID as string (matches `storage.files.id`).
    pub file_id: String,
    /// BM25 relevance score (positive, unbounded — normalize per result set).
    pub score: f32,
    /// Plain-text fragment around the first matched term, when available.
    pub snippet: Option<String>,
}

/// Port for querying the full-text content index.
///
/// `#[async_trait]` is used so the trait is dyn-compatible — `SearchService`
/// holds an `Option<Arc<dyn ContentIndexPort>>` (the feature is toggleable).
#[async_trait]
pub trait ContentIndexPort: Send + Sync + 'static {
    /// Search indexed file names + content for `query`, scoped to the drives
    /// the caller can read.
    ///
    /// The filter is applied as an `Occur::Must` set-membership clause on
    /// the `drive_id` field — Tantivy's collector only ever sees documents
    /// in one of the accessible drives, so counts, snippets, and
    /// pagination cursors all reflect the filtered set (no anti-
    /// enumeration leak — see `docs/plan/drive.md` §11). Pass the
    /// caller's full accessible-drive set; the engine already expands
    /// group-mediated drive grants before this is called.
    ///
    /// An empty `accessible_drive_ids` returns no hits — same semantics
    /// as "no drives, no search" (e.g. external users with grants only).
    /// Returns up to `limit` hits sorted by BM25 score descending.
    /// Matching is tokenized (not substring): exact terms, typo-tolerant
    /// fuzzy terms (edit distance 1) and prefix expansion on the last
    /// query token.
    async fn search_content(
        &self,
        accessible_drive_ids: &[Uuid],
        query: &str,
        limit: usize,
    ) -> Result<Vec<ContentHitDto>, DomainError>;
}
