//! The crate error type.
//!
//! No `opensqlany` type appears in the public API of this type: page-store
//! failures are mapped to the variants below or boxed.

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors from opening a file and decoding records.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The file could not be read.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// The superblock magic is not the SQL Anywhere 17 magic. The file is not
    /// a QuickBooks SQL Anywhere company file.
    #[error(
        "not a QuickBooks SQL Anywhere company file: bad superblock magic \
         0x{found:08X} (want 0x{expected:08X}); older QuickBooks versions, such as \
         2001-era files, use a different format and are not supported"
    )]
    #[non_exhaustive]
    BadMagic {
        /// Magic found at superblock offset 0x14.
        found: u32,
        /// Expected magic.
        expected: u32,
    },
    /// The superblock version triple is not the supported one.
    #[error(
        "unsupported file version (format {format_major}, version {version_a}.{version_b}); \
         supported is 3, 201.12"
    )]
    #[non_exhaustive]
    UnsupportedVersion {
        /// `format_major` found at superblock offset 0x10.
        format_major: u32,
        /// `version_a` found at offset 0x18.
        version_a: u16,
        /// `version_b` found at offset 0x1A.
        version_b: u16,
    },
    /// The superblock `flags_06` byte lacks the base bits every supported
    /// file carries.
    #[error("unsupported superblock flags 0x{flags_06:02X}")]
    #[non_exhaustive]
    UnsupportedFlags {
        /// The byte at superblock offset 0x06.
        flags_06: u8,
    },
    /// A page's reserved trailer bytes are not zero.
    #[error("bad page trailer on page {page_number}: reserved bytes non-zero")]
    #[non_exhaustive]
    BadTrailer {
        /// Zero-based page number.
        page_number: u64,
    },
    /// A page failed its CRC-32 check.
    #[error("crc mismatch on page {page_number}: stored 0x{stored:08X}, computed 0x{computed:08X}")]
    #[non_exhaustive]
    BadCrc {
        /// Zero-based page number.
        page_number: u64,
        /// CRC stored in the page footer.
        stored: u32,
        /// CRC computed over the page body.
        computed: u32,
    },
    /// A QBB backup could not be unpacked into its company file. The
    /// backup is damaged, uses an unsupported compression, or holds a company
    /// file larger than the allowed size. Reading the file itself failed with
    /// [`Error::Io`] instead.
    #[error("cannot read QBB backup: {reason}")]
    #[non_exhaustive]
    Backup {
        /// What is wrong with the backup.
        reason: BackupFailure,
    },
    /// Any other page-store failure.
    #[error("page store: {source}")]
    #[non_exhaustive]
    Store {
        /// Page involved, when known.
        page_number: Option<u64>,
        /// Underlying failure.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// A line-item anchor was followed by a value-length byte above 10.
    /// The anchor may be a false positive, or the file may use a layout this
    /// crate does not know.
    #[error(
        "page {page_number} offset {page_offset:#x}: value length byte {length_byte:#04x} after line-item anchor is above 10"
    )]
    #[non_exhaustive]
    InvalidValueLength {
        /// Page holding the anchor.
        page_number: u64,
        /// Offset of the anchor within the decoded page.
        page_offset: usize,
        /// The length byte.
        length_byte: u8,
    },
    /// A line-item anchor ends too close to the end of the page data area
    /// for its value field to be read.
    #[error(
        "page {page_number} offset {page_offset:#x}: line-item anchor leaves too few bytes for the value field"
    )]
    #[non_exhaustive]
    TruncatedValue {
        /// Page holding the anchor.
        page_number: u64,
        /// Offset of the anchor within the decoded page.
        page_offset: usize,
    },
    /// Most data pages are indistinguishable from random bytes, as in a
    /// password-protected or otherwise encrypted file. No records were read.
    #[error("{opaque} of {extent} data pages are opaque; the file content is not readable")]
    #[non_exhaustive]
    ContentOpaque {
        /// Opaque `E` pages.
        opaque: u64,
        /// All `E` pages.
        extent: u64,
    },
}

/// Why a QBB backup could not be unpacked. Carried by [`Error::Backup`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum BackupFailure {
    /// The file starts like a compound file but its structure is damaged.
    BadContainer(String),
    /// A stream the reader needs is not in the backup.
    MissingStream(&'static str),
    /// `Compression_Header` is shorter than the `Compression_Format=...;`
    /// prefix the reader checks.
    ShortHeader,
    /// `Compression_Header` names a compression other than `Deflate_ZLIB`.
    UnsupportedCompression,
    /// The company-file stream is not valid zlib data (bad header, bad
    /// checksum or corrupt deflate data).
    BadCompressedData(String),
    /// The company-file stream ends before the zlib stream does.
    Truncated,
    /// Bytes follow the end of the zlib stream.
    TrailingData,
    /// The company file inflates to more than the allowed size.
    TooLarge {
        /// The limit in bytes.
        limit: usize,
    },
}

impl std::fmt::Display for BackupFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadContainer(why) => write!(f, "damaged compound-file container: {why}"),
            Self::MissingStream(name) => write!(f, "stream {name} is missing"),
            Self::ShortHeader => f.write_str("Compression_Header is too short"),
            Self::UnsupportedCompression => {
                f.write_str("unsupported compression; only Deflate_ZLIB is read")
            }
            Self::BadCompressedData(why) => write!(f, "invalid zlib data: {why}"),
            Self::Truncated => f.write_str("the company-file stream is cut short"),
            Self::TrailingData => f.write_str("data follows the end of the company-file stream"),
            Self::TooLarge { limit } => {
                write!(f, "the company file inflates to more than {limit} bytes")
            }
        }
    }
}

/// Machine-readable class of an [`Error`], stable across message changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// [`Error::Io`]
    Io,
    /// [`Error::BadMagic`]
    BadMagic,
    /// [`Error::UnsupportedVersion`]
    UnsupportedVersion,
    /// [`Error::UnsupportedFlags`]
    UnsupportedFlags,
    /// [`Error::BadTrailer`]
    BadTrailer,
    /// [`Error::BadCrc`]
    BadCrc,
    /// [`Error::Backup`]
    Backup,
    /// [`Error::Store`]
    Store,
    /// [`Error::InvalidValueLength`]
    InvalidValueLength,
    /// [`Error::TruncatedValue`]
    TruncatedValue,
    /// [`Error::ContentOpaque`]
    ContentOpaque,
}

impl ErrorKind {
    /// Stable snake_case name, used in exports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Io => "io",
            Self::BadMagic => "bad_magic",
            Self::UnsupportedVersion => "unsupported_version",
            Self::UnsupportedFlags => "unsupported_flags",
            Self::BadTrailer => "bad_trailer",
            Self::BadCrc => "bad_crc",
            Self::Backup => "backup",
            Self::Store => "store",
            Self::InvalidValueLength => "invalid_value_length",
            Self::TruncatedValue => "truncated_value",
            Self::ContentOpaque => "content_opaque",
        }
    }
}

impl Error {
    /// The class of this error.
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::Io(_) => ErrorKind::Io,
            Self::BadMagic { .. } => ErrorKind::BadMagic,
            Self::UnsupportedVersion { .. } => ErrorKind::UnsupportedVersion,
            Self::UnsupportedFlags { .. } => ErrorKind::UnsupportedFlags,
            Self::BadTrailer { .. } => ErrorKind::BadTrailer,
            Self::BadCrc { .. } => ErrorKind::BadCrc,
            Self::Backup { .. } => ErrorKind::Backup,
            Self::Store { .. } => ErrorKind::Store,
            Self::InvalidValueLength { .. } => ErrorKind::InvalidValueLength,
            Self::TruncatedValue { .. } => ErrorKind::TruncatedValue,
            Self::ContentOpaque { .. } => ErrorKind::ContentOpaque,
        }
    }

    /// Page the error refers to, when known.
    pub fn page_number(&self) -> Option<u64> {
        match self {
            Self::BadCrc { page_number, .. }
            | Self::BadTrailer { page_number }
            | Self::InvalidValueLength { page_number, .. }
            | Self::TruncatedValue { page_number, .. } => Some(*page_number),
            Self::Store { page_number, .. } => *page_number,
            _ => None,
        }
    }

    /// Byte offset within the decoded page, when known.
    pub fn page_offset(&self) -> Option<usize> {
        match self {
            Self::InvalidValueLength { page_offset, .. }
            | Self::TruncatedValue { page_offset, .. } => Some(*page_offset),
            _ => None,
        }
    }

    /// Map a page-store error, noting the page it came from when known.
    pub(crate) fn from_store(e: opensqlany::Error, page_number: Option<u64>) -> Self {
        match e {
            opensqlany::Error::Io(io) => Self::Io(io),
            opensqlany::Error::BadMagic { got, want, .. } => Self::BadMagic {
                found: got,
                expected: want,
            },
            opensqlany::Error::UnsupportedVersion { major, a, b, .. } => Self::UnsupportedVersion {
                format_major: major,
                version_a: a,
                version_b: b,
            },
            opensqlany::Error::UnsupportedFlags { flags_06, .. } => {
                Self::UnsupportedFlags { flags_06 }
            }
            opensqlany::Error::BadCrc {
                page,
                stored,
                computed,
                ..
            } => Self::BadCrc {
                page_number: page,
                stored,
                computed,
            },
            opensqlany::Error::BadTrailer { page, .. } => Self::BadTrailer { page_number: page },
            other => Self::Store {
                page_number: other.page().or(page_number),
                source: Box::new(other),
            },
        }
    }
}
