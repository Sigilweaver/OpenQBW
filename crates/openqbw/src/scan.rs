//! Options and warnings shared by the scanning entry points.
//!
//! Strict is the default everywhere: input the decoder cannot read with
//! confidence is an error. [`ScanOptions::lenient`] turns specific errors
//! into [`ScanWarning`]s that the caller must report.

use crate::error::Error;

/// How a scan treats records it cannot decode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Strictness {
    /// Stop with an error at the first undecodable record (the default).
    #[default]
    Strict,
    /// Skip undecodable records and report each as a [`ScanWarning`];
    /// keep records the decoder is not fully sure of and mark them with
    /// [`RecordFlag`]s.
    Lenient,
}

/// Options for the scanning entry points. Build with `Default` or the
/// helpers; new fields may be added in minor releases.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ScanOptions {
    /// How undecodable records are treated.
    pub strictness: Strictness,
}

impl ScanOptions {
    /// Stop with an error at the first undecodable record (the default).
    pub fn strict() -> Self {
        Self::default()
    }

    /// Skip undecodable records and report them as warnings.
    pub fn lenient() -> Self {
        Self {
            strictness: Strictness::Lenient,
        }
    }

    /// `true` when `strictness` is [`Strictness::Lenient`].
    pub fn is_lenient(&self) -> bool {
        self.strictness == Strictness::Lenient
    }
}

/// A reason a record that was kept deserves less trust. Records carry a
/// `Vec<RecordFlag>`; an empty list means no check flagged the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RecordFlag {
    /// The amount's source table has no established encoding, so the amount
    /// was not decoded.
    AmountEncodingUnconfirmed,
    /// The amount type is one whose meaning is not established, so the
    /// amount was not decoded.
    AmountTypeUnestablished,
}

impl RecordFlag {
    /// Stable snake_case name, used in exports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AmountEncodingUnconfirmed => "amount_encoding_unconfirmed",
            Self::AmountTypeUnestablished => "amount_type_unestablished",
        }
    }
}

/// Something a lenient scan skipped or could not vouch for.
#[derive(Debug)]
#[non_exhaustive]
pub enum ScanWarning {
    /// A line-item record was skipped.
    RecordSkipped(Error),
}

impl ScanWarning {
    /// Page and byte offset the warning refers to, when known.
    pub fn location(&self) -> (Option<u64>, Option<usize>) {
        match self {
            Self::RecordSkipped(e) => (e.page_number(), e.page_offset()),
        }
    }
}

impl ScanWarning {
    /// Stable snake_case class name, used in exports.
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::RecordSkipped(e) => e.kind().as_str(),
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
