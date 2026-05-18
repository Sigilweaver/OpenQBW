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

pub use bv_recovery::{deobfuscate_with_bv, recover_bv_qb_data};
pub use lineitem::{
    iter_lineitems, AmountType, LineItem, LineItemError, DATE_EPOCH_DAYS_BEFORE_UNIX,
};
