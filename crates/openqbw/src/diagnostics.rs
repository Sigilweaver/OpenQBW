//! Reverse-engineering diagnostics.
//!
//! **Outside semantic versioning.** Everything in this module may change or
//! disappear in any release. It exists for the `openqbw debug` commands and
//! for people studying the file format: heuristic cross-checks whose output
//! is evidence about the format, not data read from the file.

pub use crate::attribution_content::{AttributionAgreement, ContentAttribution, RowSignature};
pub use crate::attribution_schema::{SchemaAttribution, ValidationStats, WidthBand};
pub use crate::fkgraph::{FkEdge, FkGraph, FkGraphStats, stats as fk_graph_stats};
pub use crate::nullability::NullsFlagBucket;
pub use crate::sysindex::{AuditOutcome, CrossValidation};

use crate::error::Result;
use crate::reader::Reader;
use crate::scan::Scan;

/// Heuristic foreign-key edges inferred from `*_id` column names.
pub fn fk_graph(reader: &Reader) -> Result<FkGraph> {
    crate::fkgraph::build(reader.store(), reader.model(), reader.options())
}

/// Histogram of the `SYSCOLUMN` `nulls_flag` byte.
pub fn nulls_flag_histogram(reader: &Reader) -> Result<Scan<NullsFlagBucket>> {
    crate::nullability::histogram(reader.store(), reader.model(), reader.options())
}

/// Row-0 content signatures of every table root page.
pub fn content_attribution(reader: &Reader) -> Result<ContentAttribution> {
    ContentAttribution::build(reader.store(), reader.model(), reader.options())
}

/// Per-table row-width bands derived from `SYSCOLUMN`.
pub fn schema_attribution(reader: &Reader) -> Result<SchemaAttribution> {
    SchemaAttribution::build(reader.store(), reader.model(), reader.options())
}

/// Check the position attribution against `SYSINDEX` root pages.
pub fn sysindex_cross_validation(reader: &Reader) -> Result<CrossValidation> {
    let indexes = reader.indexes()?;
    Ok(CrossValidation::run(
        &indexes.items,
        reader.attribution(),
        &reader.catalog().items,
    ))
}

/// Number of 16-page blocks the keystream model calibrated.
pub fn learned_block_count(reader: &Reader) -> usize {
    reader.model().learned_block_count()
}
