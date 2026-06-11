//! Embedded full-text content index (Tantivy) and its feeding pipeline.
//!
//! Three pieces, mirroring the thumbnail/tree-etag architecture:
//!
//! * [`tantivy_content_index`] — the embedded BM25 index over file names and
//!   extracted content. Lives on local disk (`{storage}/.search-index`),
//!   single-writer, microsecond queries. A DERIVED artifact: PostgreSQL is
//!   the source of truth and the index is rebuilt (reseeded) whenever its
//!   on-disk schema version differs from the binary's.
//! * [`text_extractor`] — pure-Rust text extraction (plain text/code, PDF,
//!   Office OOXML/ODF). CPU-bound, runs only on the background worker.
//! * [`content_index_worker`] — drains `storage.search_index_dirty` (fed by
//!   statement triggers on `storage.files`), extracts text once per unique
//!   blob (BLAKE3-keyed cache in `storage.blob_extracted_text`), and applies
//!   batched Tantivy mutations. Never touches a request path.

pub mod content_index_worker;
pub mod tantivy_content_index;
pub mod text_extractor;
