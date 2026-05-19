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
//! Prototype quality. See `OpenQBW/re/NOTES.md` (entries C.40–C.43) for the
//! reverse-engineered record layout and remaining gaps.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod attribution_content;
mod bv_recovery;
mod date;
mod fkgraph;
mod lineitem;
mod opaque;
mod page_attribution;
mod syscolumn;
mod systable;
mod transaction_header;

pub use attribution_content::{
    AttributionAgreement, ContentAttribution, RowSignature, SIG_LEN,
};
pub use bv_recovery::{
    deobfuscate_with_bv, oracle_bv_e_page, recover_bv_any, recover_bv_apage,
    recover_bv_brute, recover_bv_qb_data, APAGE_MAGIC,
};
pub use date::{
    sa_day_to_unix_day, sa_day_to_unix_seconds, unix_day_to_sa_day, SA_DAY_MAX_PLAUSIBLE,
    SA_DAY_MIN_PLAUSIBLE,
};
pub use fkgraph::{build as build_fk_graph, stats as fk_graph_stats, FkEdge, FkGraphStats};
pub use lineitem::{
    iter_lineitems, iter_lineitems_with_attribution, AmountType, LineItem, LineItemError,
    DATE_EPOCH_DAYS_BEFORE_UNIX,
};
pub use opaque::{is_opaque_high_entropy, OPAQUE_ENTROPY_THRESHOLD};
pub use page_attribution::PageAttribution;
pub use systable::{collect_unique, iter_systable_entries, scan_page as scan_systable_page, SysTableEntry};
pub use syscolumn::{
    collect_unique as collect_unique_syscolumns, iter_syscolumns, schema_for,
    scan_page as scan_syscolumn_page, SysColumn, SYSCOLUMN_TAG,
};
pub use transaction_header::{iter_transaction_headers, TransactionHeader};
