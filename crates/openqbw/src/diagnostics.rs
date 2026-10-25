//! Reverse-engineering diagnostics.
//!
//! **Outside semantic versioning.** Everything in this module may change or
//! disappear in any release. It exists for the `openqbw debug` commands and
//! for people studying the file format: heuristic cross-checks whose output
//! is evidence about the format rather than data read from the file, the
//! raw keystream oracles, and the per-page row scanners with their anchor
//! constants.

pub use crate::attribution_content::{
    AttributionAgreement, ContentAttribution, RowSignature, SIG_LEN,
};
pub use crate::attribution_schema::{
    MIN_ROW_BODY_BYTES, SchemaAttribution, VARIABLE_COLUMN_UPPER_ALLOWANCE, ValidationStats,
    WidthBand,
};
pub use crate::bv_recovery::{
    APAGE_MAGIC, deobfuscate_with_bv, oracle_bv_e_page, recover_bv_any, recover_bv_apage,
    recover_bv_brute, recover_bv_qb_data,
};
pub use crate::fkgraph::{FkEdge, FkGraph, FkGraphStats, stats as fk_graph_stats};
pub use crate::nullability::NullsFlagBucket;
pub use crate::opaque::{OPAQUE_ENTROPY_THRESHOLD, OPAQUE_FILE_FRACTION, is_opaque_high_entropy};
pub use crate::open::SUPPORTED_VERSION;
pub use crate::owner_bridge::{
    BACKREF_MAX_DISTANCE, BACKREF_MIN_DISTANCE, BackrefVotes, DEFAULT_BACKREF_DISTANCE, OwnerBridge,
};
pub use crate::syscolumn::{SYSCOLUMN_TAG, scan_page as scan_syscolumn_page};
pub use crate::sysindex::{
    AuditOutcome, CrossValidation, SYSINDEX_CREATOR, scan_page as scan_sysindex_page,
};
pub use crate::sysobject::SYSOBJECT_NAME_OFFSET;
pub use crate::systable::scan_page as scan_systable_page;

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
