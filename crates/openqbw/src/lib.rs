//! QuickBooks `.qbw` file reader.
//!
//! Reads transaction headers, invoice and journal line items and the system
//! catalog out of a QuickBooks company file, an SQL Anywhere 17 page store
//! read with the [`opensqlany`](https://crates.io/crates/opensqlany) crate.
//! No `opensqlany` type appears in this crate's API.
//!
//! ```no_run
//! use openqbw::{ReadOptions, Reader};
//!
//! let reader = Reader::open("mybooks.qbw", &ReadOptions::default())?;
//! let lines = reader.line_items()?;
//! for w in &lines.warnings {
//!     eprintln!("warning: {w}");
//! }
//! let decoded: i64 = lines.items.iter().filter_map(|li| li.amount_cents).sum();
//! println!("{} lines, {decoded} cents decoded", lines.items.len());
//! # Ok::<(), openqbw::Error>(())
//! ```
//!
//! # Errors, options and flags
//!
//! Reads are strict by default: input the decoder cannot read with
//! confidence is an [`Error`]. [`ReadOptions`] carries a [`Leniency`] set;
//! [`Leniency::RECORDS`] skips undecodable records and lists them in
//! [`Scan::rejected`]. Records that were kept but deserve less trust carry
//! [`RecordFlag`]s, and conditions that apply to a whole read are
//! [`Warning`]s. [`Error`], [`WarningKind`], [`RecordFlag`] and the result
//! types are `#[non_exhaustive]`.
//!
//! [`diagnostics`] holds reverse-engineering cross-checks and is outside
//! semantic versioning.
//!
//! See `SPECIFICATION.md` and `re/NOTES.md` for the reverse-engineered
//! record layouts and remaining gaps.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod attribution_content;
mod attribution_schema;
mod bv_recovery;
mod date;
mod decode;
pub mod diagnostics;
mod error;
mod fkgraph;
mod lineitem;
mod nullability;
mod opaque;
mod open;
mod owner_bridge;
mod page_attribution;
mod reader;
mod scan;
mod syscolumn;
mod sysindex;
mod sysobject;
mod systable;
mod transaction_header;

#[cfg(test)]
mod testutil;

pub use date::DateSource;
pub use error::{Error, ErrorKind, Result};
pub use lineitem::{AmountEncoding, AmountType, LineItem};
pub use owner_bridge::BridgeSource;
pub use page_attribution::{AttributionGap, PageAttribution};
pub use reader::Reader;
pub use scan::{Leniency, ReadOptions, RecordFlag, Scan, Warning, WarningKind};
pub use syscolumn::{SchemaRecovery, SysColumn, SyscolumnStats};
pub use sysindex::SysIndexEntry;
pub use systable::SysTableEntry;
pub use transaction_header::TransactionHeader;
