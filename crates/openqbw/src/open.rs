//! Opening a `.qbw` file with format checks.
//!
//! `PageStore::open` only checks that the file size is a multiple of 4096.
//! [`open_store`] also checks the superblock magic and version triple and
//! the CRC-32 footer of every page, so a file that is not a QuickBooks
//! company file, or a page damaged on disk, is an error before any record
//! is decoded.

use std::path::Path;

use opensqlany::PageStore;

/// Superblock `format_major`, `version_a` and `version_b` of every supported
/// file (SPECIFICATION.md section 2: identical from QuickBooks 2018 to
/// Enterprise 2024).
pub const SUPPORTED_VERSION: (u32, u16, u16) = (3, 201, 12);

/// Errors from [`open_store`] and [`check_store`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OpenError {
    /// The page store could not be opened, the magic is wrong, or a page
    /// failed its CRC check.
    #[error(transparent)]
    Store(#[from] opensqlany::Error),
    /// The superblock version triple is not the supported one.
    #[error(
        "unsupported file version (format {format_major}, version {version_a}.{version_b}); \
         supported is {}, {}.{}",
        SUPPORTED_VERSION.0, SUPPORTED_VERSION.1, SUPPORTED_VERSION.2
    )]
    UnsupportedVersion {
        /// `format_major` found at superblock offset 0x10.
        format_major: u32,
        /// `version_a` found at offset 0x18.
        version_a: u16,
        /// `version_b` found at offset 0x1A.
        version_b: u16,
    },
}

/// What [`open_store`] verifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct OpenOptions {
    /// Verify the CRC-32 footer of every page (default `true`).
    pub verify_crc: bool,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self { verify_crc: true }
    }
}

/// Open `path` as a page store and run [`check_store`].
pub fn open_store(path: impl AsRef<Path>, options: &OpenOptions) -> Result<PageStore, OpenError> {
    let store = PageStore::open(path)?;
    check_store(&store, options)?;
    Ok(store)
}

/// Check the superblock magic and version and, unless disabled, every
/// page's CRC. Returns the first failure.
pub fn check_store(store: &PageStore, options: &OpenOptions) -> Result<(), OpenError> {
    let sb = store.superblock()?; // verifies the magic
    if (sb.format_major, sb.version_a, sb.version_b) != SUPPORTED_VERSION {
        return Err(OpenError::UnsupportedVersion {
            format_major: sb.format_major,
            version_a: sb.version_a,
            version_b: sb.version_b,
        });
    }
    if options.verify_crc {
        for pn in 0..store.page_count() {
            store.page(pn)?.verify_crc()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use opensqlany::{PAGE_SIZE, SA_MAGIC};

    fn seal(page: &mut [u8]) {
        let crc = crc32fast::hash(&page[..0xFFC]);
        page[0xFFC..0x1000].copy_from_slice(&crc.to_le_bytes());
    }

    fn store(pages: usize, magic: u32, ver: (u32, u16, u16)) -> PageStore {
        let mut b = vec![0u8; PAGE_SIZE * pages];
        b[0x10..0x14].copy_from_slice(&ver.0.to_le_bytes());
        b[0x14..0x18].copy_from_slice(&magic.to_le_bytes());
        b[0x18..0x1A].copy_from_slice(&ver.1.to_le_bytes());
        b[0x1A..0x1C].copy_from_slice(&ver.2.to_le_bytes());
        for p in b.chunks_mut(PAGE_SIZE) {
            seal(p);
        }
        PageStore::from_bytes(b).unwrap()
    }

    #[test]
    fn valid_store_passes() {
        let s = store(3, SA_MAGIC, SUPPORTED_VERSION);
        check_store(&s, &OpenOptions::default()).unwrap();
    }

    #[test]
    fn wrong_magic_is_an_error() {
        let s = store(2, 0x1234_5678, SUPPORTED_VERSION);
        let e = check_store(&s, &OpenOptions::default()).unwrap_err();
        assert!(matches!(
            e,
            OpenError::Store(opensqlany::Error::BadMagic { .. })
        ));
    }

    #[test]
    fn wrong_version_is_an_error() {
        let s = store(2, SA_MAGIC, (3, 202, 12));
        let e = check_store(&s, &OpenOptions::default()).unwrap_err();
        assert!(matches!(
            e,
            OpenError::UnsupportedVersion { version_a: 202, .. }
        ));
    }

    #[test]
    fn corrupt_page_is_an_error_unless_crc_check_is_off() {
        let mut b = vec![0u8; PAGE_SIZE * 3];
        b[0x14..0x18].copy_from_slice(&SA_MAGIC.to_le_bytes());
        b[0x10..0x14].copy_from_slice(&3u32.to_le_bytes());
        b[0x18..0x1A].copy_from_slice(&201u16.to_le_bytes());
        b[0x1A..0x1C].copy_from_slice(&12u16.to_le_bytes());
        for p in b.chunks_mut(PAGE_SIZE) {
            seal(p);
        }
        b[PAGE_SIZE * 2 + 100] ^= 0xFF; // bit rot in page 2
        let s = PageStore::from_bytes(b).unwrap();
        let e = check_store(&s, &OpenOptions::default()).unwrap_err();
        assert!(matches!(
            e,
            OpenError::Store(opensqlany::Error::BadCrc { page: 2, .. })
        ));
        let off = OpenOptions {
            verify_crc: false,
            ..OpenOptions::default()
        };
        check_store(&s, &off).unwrap();
    }
}
