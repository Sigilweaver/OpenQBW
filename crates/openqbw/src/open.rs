//! Opening a `.qbw` file with format checks.
//!
//! `PageStore::open` checks the superblock magic, the version triple and the
//! `flags_06` base bits. [`open_store`] also checks the CRC-32 footer and the
//! reserved trailer bytes of every data page (page 0, the superblock, has no
//! trailer and gets the CRC check only), so a file that is not a QuickBooks
//! company file, or a page damaged on disk, is an error before any record
//! is decoded.

use std::path::Path;

use opensqlany::{ApModel, PageStore, ReadOptions};

use crate::error::{Error, Result};

/// Superblock `format_major`, `version_a` and `version_b` of every supported
/// file (SPECIFICATION.md section 2: identical from QuickBooks 2018 to
/// Enterprise 2024).
pub const SUPPORTED_VERSION: (u32, u16, u16) = (3, 201, 12);

/// Index of the superblock page.
const SUPERBLOCK_PAGE: u64 = 0;

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
/// page's CRC and, for every page except the superblock (page 0), its
/// trailer. Returns the first failure.
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
            if pn == SUPERBLOCK_PAGE {
                // Page 0 is the superblock, not a data page: from 0x400 it
                // holds the rolled engine fingerprint text, so 0xFF0..0xFFB
                // is ASCII (SPECIFICATION.md sections 2.1 and 2.3). It has
                // the CRC footer but not the page trailer, so only the CRC
                // is checked.
                store
                    .page(pn)
                    .and_then(|page| page.verify_crc())
                    .map_err(|e| Error::from_store(e, Some(pn)))?;
                continue;
            }
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

    /// Page-0 shaped like a real superblock: offsets 0x400..0x1000 hold the
    /// rolled engine fingerprint text (SPECIFICATION.md section 2.3), so the
    /// bytes at 0xFF0..0xFFB are ASCII, not a page trailer.
    fn superblock_with_text_tail(pages: usize) -> Vec<u8> {
        let mut b = vec![0u8; PAGE_SIZE * pages];
        b[0x06] = opensqlany::FLAGS_06_BASE;
        b[0x10..0x14].copy_from_slice(&SUPPORTED_VERSION.0.to_le_bytes());
        b[0x14..0x18].copy_from_slice(&SA_MAGIC.to_le_bytes());
        b[0x18..0x1A].copy_from_slice(&SUPPORTED_VERSION.1.to_le_bytes());
        b[0x1A..0x1C].copy_from_slice(&SUPPORTED_VERSION.2.to_le_bytes());
        let text = b"2182 SAP SE, Copyright (c)2015 17.0.4.";
        for (i, byte) in b[0x400..0xFFC].iter_mut().enumerate() {
            *byte = text[i % text.len()];
        }
        for p in b.chunks_mut(PAGE_SIZE) {
            seal(p);
        }
        b
    }

    #[test]
    fn superblock_text_tail_is_not_held_to_the_data_page_trailer() {
        let b = superblock_with_text_tail(3);
        assert_ne!(b[0xFF3], 0, "fixture must have a non-zero 0xFF3");
        let s = PageStore::from_bytes_unchecked(b).unwrap();
        check_store(&s, &OpenOptions::default()).unwrap();
    }

    #[test]
    fn superblock_crc_is_still_checked() {
        let mut b = superblock_with_text_tail(3);
        b[0x500] ^= 0xFF;
        let s = PageStore::from_bytes_unchecked(b).unwrap();
        let e = check_store(&s, &OpenOptions::default()).unwrap_err();
        assert!(matches!(e, Error::BadCrc { page_number: 0, .. }));
    }

    #[test]
    fn data_page_with_non_zero_reserved_trailer_bytes_is_still_an_error() {
        let mut b = superblock_with_text_tail(3);
        b[PAGE_SIZE * 2 + 0xFF3] = 1;
        seal(&mut b[PAGE_SIZE * 2..PAGE_SIZE * 3]);
        let s = PageStore::from_bytes_unchecked(b).unwrap();
        let e = check_store(&s, &OpenOptions::default()).unwrap_err();
        assert!(
            matches!(e, Error::BadTrailer { page_number: 2 }),
            "expected a trailer error on page 2, got {e:?}"
        );
    }
}
