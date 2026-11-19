//! Read the company-file stream in a zlib-compressed QBB backup.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};

use flate2::{Decompress, FlushDecompress, Status};

use crate::error::{BackupFailure, Error, Result};

const COMPRESSION_HEADER: &str = "/Compression_Header";
const COMPANY_FILE: &str = "/file_COMPANY_FILE";
const ZLIB_HEADER: &[u8] = b"Compression_Format=Deflate_ZLIB;";
/// Compound File Binary signature at offset 0 of a QBB backup.
const CFB_SIGNATURE: [u8; 8] = [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1];

/// `true` when `file` starts with the compound-file signature. The file
/// position is reset to the start.
pub(crate) fn has_signature(file: &mut File) -> io::Result<bool> {
    let mut prefix = Vec::with_capacity(CFB_SIGNATURE.len());
    file.by_ref()
        .take(CFB_SIGNATURE.len() as u64)
        .read_to_end(&mut prefix)?;
    file.seek(SeekFrom::Start(0))?;
    Ok(prefix == CFB_SIGNATURE)
}

/// Inflate the company file of the backup in `input`, up to `limit` bytes.
pub(crate) fn company_bytes(input: impl Read + Seek, limit: usize) -> Result<Vec<u8>> {
    let mut backup = cfb::CompoundFile::open(input).map_err(container_error)?;
    let mut header = [0; ZLIB_HEADER.len()];
    backup
        .open_stream(COMPRESSION_HEADER)
        .map_err(|e| stream_error(e, COMPRESSION_HEADER))?
        .read_exact(&mut header)
        .map_err(|e| match e.kind() {
            io::ErrorKind::UnexpectedEof => backup_error(BackupFailure::ShortHeader),
            _ => Error::Io(e),
        })?;
    if header.as_slice() != ZLIB_HEADER {
        return Err(backup_error(BackupFailure::UnsupportedCompression));
    }
    let stream = backup
        .open_stream(COMPANY_FILE)
        .map_err(|e| stream_error(e, COMPANY_FILE))?;
    inflate(&mut BufReader::new(stream), limit)
}

fn backup_error(reason: BackupFailure) -> Error {
    Error::Backup { reason }
}

/// A failure to open the compound file: a damaged container, or a read error.
fn container_error(e: io::Error) -> Error {
    match e.kind() {
        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof => {
            backup_error(BackupFailure::BadContainer(e.to_string()))
        }
        _ => Error::Io(e),
    }
}

/// A failure to open a named stream: absent from the backup, or a read error.
fn stream_error(e: io::Error, name: &'static str) -> Error {
    match e.kind() {
        io::ErrorKind::NotFound => backup_error(BackupFailure::MissingStream(name)),
        _ => container_error(e),
    }
}

/// Make room for `extra` more bytes, doubling but never past `limit`, so an
/// allocation failure is an error and not an abort. The caller has checked
/// that `bytes.len() + extra <= limit`.
fn grow(bytes: &mut Vec<u8>, extra: usize, limit: usize) -> Result<()> {
    if bytes.capacity() - bytes.len() >= extra {
        return Ok(());
    }
    let len = bytes.len();
    let doubled = len.min(limit - len);
    let want = extra.max(doubled);
    bytes
        .try_reserve_exact(want)
        .map_err(|_| backup_error(BackupFailure::OutOfMemory { bytes: len + want }))
}

fn inflate(input: &mut impl BufRead, limit: usize) -> Result<Vec<u8>> {
    let mut inflater = Decompress::new(true);
    let mut bytes = Vec::new();
    let mut chunk = [0; 16 * 1024];

    loop {
        let before_in = inflater.total_in();
        let before_out = inflater.total_out();
        let status = inflater
            .decompress(input.fill_buf()?, &mut chunk, FlushDecompress::None)
            .map_err(|e| backup_error(BackupFailure::BadCompressedData(e.to_string())))?;
        // The increments are bounded by the input and output slices.
        let consumed = (inflater.total_in() - before_in) as usize;
        let produced = (inflater.total_out() - before_out) as usize;
        input.consume(consumed);

        if produced > limit - bytes.len() {
            return Err(backup_error(BackupFailure::TooLarge { limit }));
        }
        grow(&mut bytes, produced, limit)?;
        bytes.extend_from_slice(&chunk[..produced]);

        if status == Status::StreamEnd {
            if !input.fill_buf()?.is_empty() {
                return Err(backup_error(BackupFailure::TrailingData));
            }
            return Ok(bytes);
        }
        if consumed == 0 && produced == 0 {
            return Err(backup_error(BackupFailure::Truncated));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    use flate2::{Compression, write::ZlibEncoder};

    fn compressed(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    fn backup(header: &[u8], stream: &[u8]) -> Cursor<Vec<u8>> {
        let mut backup = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
        backup
            .create_stream(COMPRESSION_HEADER)
            .unwrap()
            .write_all(header)
            .unwrap();
        backup
            .create_stream(COMPANY_FILE)
            .unwrap()
            .write_all(stream)
            .unwrap();
        backup.flush().unwrap();
        backup.into_inner()
    }

    #[test]
    fn reads_company_stream_from_synthetic_backup() {
        let bytes = include_bytes!("../tests/data/synthetic.qbw");
        let input = backup(
            b"Compression_Format=Deflate_ZLIB;Version=1.2.3;",
            &compressed(bytes),
        );
        let inflated = company_bytes(input, crate::ReadOptions::DEFAULT_MAX_BACKUP_BYTES).unwrap();
        assert_eq!(inflated, bytes.as_slice());
        let reader = crate::Reader::from_bytes(inflated, &crate::ReadOptions::strict()).unwrap();
        assert_eq!(reader.page_count(), 8);
        assert_eq!(reader.catalog().items.len(), 3);
    }

    #[test]
    fn rejects_unsupported_compression() {
        let input = backup(b"Compression_Format=invalid_compression;", &[0; 4]);
        assert_eq!(
            company_bytes(input, crate::ReadOptions::DEFAULT_MAX_BACKUP_BYTES)
                .unwrap_err()
                .kind(),
            crate::ErrorKind::Backup
        );
    }

    #[test]
    fn rejects_truncated_corrupt_and_trailing_data() {
        let encoded = compressed(b"contents");
        assert!(inflate(&mut &encoded[..encoded.len() - 1], 100).is_err());

        let mut bad_checksum = encoded.clone();
        *bad_checksum.last_mut().unwrap() ^= 1;
        assert!(inflate(&mut bad_checksum.as_slice(), 100).is_err());

        let mut trailing = encoded;
        trailing.push(0);
        assert!(inflate(&mut trailing.as_slice(), 100).is_err());
    }

    #[test]
    fn enforces_decompression_limit() {
        let encoded = compressed(&[0; 500]);
        assert!(inflate(&mut encoded.as_slice(), 499).is_err());
        assert_eq!(inflate(&mut encoded.as_slice(), 500).unwrap().len(), 500);
    }

    #[test]
    fn limit_is_part_of_the_error_and_growth_stays_within_it() {
        let encoded = compressed(&vec![7; 100_000]);
        let err = inflate(&mut encoded.as_slice(), 99_999).unwrap_err();
        assert!(matches!(
            err,
            Error::Backup {
                reason: BackupFailure::TooLarge { limit: 99_999 }
            }
        ));
        let bytes = inflate(&mut encoded.as_slice(), 100_000).unwrap();
        assert_eq!(bytes.len(), 100_000);
        assert!(bytes.capacity() <= 100_000 + 16 * 1024);
    }

    #[test]
    fn default_limit_is_one_gibibyte() {
        assert_eq!(crate::ReadOptions::default().max_backup_bytes, 1 << 30);
    }
}
