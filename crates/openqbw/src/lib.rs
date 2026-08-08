//! QuickBooks `.qbw` file parser.
//!
//! Provides parsing of invoice line-item records out of an SA17 page-store
//! that was produced by QuickBooks. Built on the [`opensqlany`] crate which
//! handles the lower-level page-store layer (CRC validation, AP-fill
//! deobfuscation, slotted-page directories).
//!
//! This release (v0.1) covers the invoice line-item record format only.
//! Invoice headers, bills, checks, journal entries, and the system catalog
//! are deferred to later releases.
//!
//! # Status
//!
//! Prototype quality. See `OpenQBW/re/NOTES.md` (entries C.40-C.43) for the
//! reverse-engineered record layout and remaining gaps.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod attribution_content;
mod attribution_schema;
mod bv_recovery;
mod date;
mod fkgraph;
mod lineitem;
mod nullability;
mod opaque;
mod page_attribution;
mod syscolumn;
mod sysindex;
mod sysobject;
mod systable;
mod transaction_header;

pub use attribution_content::{AttributionAgreement, ContentAttribution, RowSignature, SIG_LEN};
pub use attribution_schema::{
    MIN_ROW_BODY_BYTES, SchemaAttribution, VARIABLE_COLUMN_UPPER_ALLOWANCE, ValidationStats,
    WidthBand,
};
pub use bv_recovery::{
    APAGE_MAGIC, deobfuscate_with_bv, oracle_bv_e_page, recover_bv_any, recover_bv_apage,
    recover_bv_brute, recover_bv_qb_data,
};
pub use date::{
    SA_DAY_MAX_PLAUSIBLE, SA_DAY_MIN_PLAUSIBLE, sa_day_to_unix_day, sa_day_to_unix_seconds,
    unix_day_to_sa_day,
};
pub use fkgraph::{FkEdge, FkGraphStats, build as build_fk_graph, stats as fk_graph_stats};
pub use lineitem::{
    AmountType, DATE_EPOCH_DAYS_BEFORE_UNIX, LineItem, LineItemError, iter_lineitems,
    iter_lineitems_with_attribution,
};
pub use nullability::{NullsFlagBucket, histogram as nulls_flag_histogram};
pub use opaque::{OPAQUE_ENTROPY_THRESHOLD, is_opaque_high_entropy};
pub use page_attribution::{AttributionGap, PageAttribution};
pub use syscolumn::{
    SYSCOLUMN_TAG, SysColumn, collect_unique as collect_unique_syscolumns, iter_syscolumns,
    scan_page as scan_syscolumn_page, schema_for,
};
pub use sysindex::{
    AuditOutcome, CrossValidation, DISAGREE_SAMPLE_LIMIT, SYSINDEX_CREATOR, SysIndexEntry,
    collect_unique as collect_unique_sysindex, iter_sysindex, scan_page as scan_sysindex_page,
};
pub use sysobject::{SYSOBJECT_NAME_OFFSET, bridge_owners_to_tables};
pub use systable::{
    SysTableEntry, collect_unique, iter_systable_entries, scan_page as scan_systable_page,
};
pub use transaction_header::{TransactionHeader, iter_transaction_headers};
