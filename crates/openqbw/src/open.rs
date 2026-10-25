//! Opening a `.qbw` file with format checks.
//!
//! `PageStore::open` checks the superblock magic, the version triple and the
//! `flags_06` base bits. [`open_store`] also checks the CRC-32 footer and the
//! reserved trailer bytes of every page, so a file that is not a QuickBooks
//! company file, or a page damaged on disk, is an error before any record
//! is decoded.

use std::path::Path;

use opensqlany::{ApModel, PageStore, ReadOptions};

use crate::error::{Error, Result};

/// Superblock `format_major`, `version_a` and `version_b` of every supported
/// file (SPECIFICATION.md section 2: identical from QuickBooks 2018 to
/// Enterprise 2024).
pub const SUPPORTED_VERSION: (u32, u16, u16) = (3, 201, 12);

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
pub fn open_store(path: impl AsRef<Path>, options: &OpenOptions) -> Result<PageStore> {
    let store = PageStore::open(path).map_err(|e| Error::from_store(e, None))?;
    check_store(&store, options)?;
    Ok(store)
}

/// Calibrate the AP keystream model of `store`.
///
/// A store whose pure-AP pages read as plaintext, or where none calibrates,
/// is an error ([`Error::Store`]): it is not an obfuscated QuickBooks file
/// this crate can decode.
pub fn learn_model(store: &PageStore) -> Result<ApModel> {
    ApModel::learn(store, &ReadOptions::strict()).map_err(|e| Error::from_store(e, None))
}

/// Check the superblock magic and version and, unless disabled, every
/// page's CRC and trailer. Returns the first failure.
///
/// An all-zero page was never written and passes the page checks.
pub fn check_store(store: &PageStore, options: &OpenOptions) -> Result<()> {
    // `superblock` verifies the magic.
    let sb = store
        .superblock()
        .map_err(|e| Error::from_store(e, Some(0)))?;
    let found = (sb.format_major(), sb.version_a(), sb.version_b());
    if found != SUPPORTED_VERSION {
        return Err(Error::UnsupportedVersion {
            format_major: found.0,
            version_a: found.1,
            version_b: found.2,
        });
    }
    if options.verify_crc {
        for pn in 0..store.page_count() {
            store
                .page_verified(pn)
                .map_err(|e| Error::from_store(e, Some(pn)))?;
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
        b[0x06] = opensqlany::FLAGS_06_BASE;
        b[0x10..0x14].copy_from_slice(&ver.0.to_le_bytes());
        b[0x14..0x18].copy_from_slice(&magic.to_le_bytes());
        b[0x18..0x1A].copy_from_slice(&ver.1.to_le_bytes());
        b[0x1A..0x1C].copy_from_slice(&ver.2.to_le_bytes());
        for p in b.chunks_mut(PAGE_SIZE) {
            seal(p);
        }
        PageStore::from_bytes_unchecked(b).unwrap()
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
        assert!(matches!(e, Error::BadMagic { .. }));
    }

    #[test]
    fn wrong_version_is_an_error() {
        let s = store(2, SA_MAGIC, (3, 202, 12));
        let e = check_store(&s, &OpenOptions::default()).unwrap_err();
        assert!(matches!(
            e,
            Error::UnsupportedVersion { version_a: 202, .. }
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
        let s = PageStore::from_bytes_unchecked(b).unwrap();
        let e = check_store(&s, &OpenOptions::default()).unwrap_err();
        assert!(matches!(e, Error::BadCrc { page_number: 2, .. }));
        let off = OpenOptions {
            verify_crc: false,
            ..OpenOptions::default()
        };
        check_store(&s, &off).unwrap();
    }
}
