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

mod bv_recovery;
mod lineitem;
mod opaque;
mod page_attribution;
mod systable;
mod transaction_header;

pub use bv_recovery::{
    deobfuscate_with_bv, oracle_bv_e_page, recover_bv_any, recover_bv_brute,
    recover_bv_qb_data,
};
pub use lineitem::{
    iter_lineitems, iter_lineitems_with_attribution, AmountType, LineItem, LineItemError,
    DATE_EPOCH_DAYS_BEFORE_UNIX,
};
pub use opaque::{is_opaque_high_entropy, OPAQUE_ENTROPY_THRESHOLD};
pub use page_attribution::PageAttribution;
pub use systable::{collect_unique, iter_systable_entries, scan_page as scan_systable_page, SysTableEntry};
pub use transaction_header::{iter_transaction_headers, TransactionHeader};
