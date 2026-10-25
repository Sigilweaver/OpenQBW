//! Options and warnings shared by the scanning entry points.
//!
//! Strict is the default everywhere: input the decoder cannot read with
//! confidence is an error. [`ScanOptions::lenient`] turns specific errors
//! into [`ScanWarning`]s that the caller must report.

use crate::lineitem::LineItemError;

/// How a scan treats records it cannot decode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ScanOptions {
    /// Skip undecodable records and report each as a [`ScanWarning`]
    /// instead of returning the first error.
    pub lenient: bool,
}

impl ScanOptions {
    /// Stop with an error at the first undecodable record (the default).
    pub fn strict() -> Self {
        Self::default()
    }

    /// Skip undecodable records and report them as warnings.
    pub fn lenient() -> Self {
        Self { lenient: true }
    }
}

/// Something a lenient scan skipped or could not vouch for.
#[derive(Debug)]
#[non_exhaustive]
pub enum ScanWarning {
    /// A line-item record was skipped.
    RecordSkipped(LineItemError),
}

impl ScanWarning {
    /// Page and byte offset the warning refers to, when known.
    pub fn location(&self) -> (Option<u64>, Option<usize>) {
        match self {
            Self::RecordSkipped(e) => e.location(),
        }
    }
}

impl std::fmt::Display for ScanWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RecordSkipped(e) => write!(f, "record skipped: {e}"),
        }
    }
}

/// Result of a scan: the records found and the warnings a lenient scan
/// produced. `warnings` is always empty in strict mode.
#[derive(Debug)]
#[non_exhaustive]
pub struct ScanResult<T> {
    /// Records found, in page order.
    pub items: Vec<T>,
    /// Records skipped or flagged. Callers must surface these.
    pub warnings: Vec<ScanWarning>,
}

impl<T> Default for ScanResult<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            warnings: Vec::new(),
        }
    }
}
