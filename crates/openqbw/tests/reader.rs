//! `Reader` on the synthetic company file in `tests/common`.

mod common;

use std::path::PathBuf;

use openqbw::{AmountEncoding, ErrorKind, ReadOptions, Reader, RecordFlag};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

#[test]
fn committed_fixtures_match_the_generator() {
    // Set OPENQBW_REGEN_FIXTURES=1 to rewrite them after changing the
    // generator.
    for (name, bad) in [("synthetic.qbw", false), ("synthetic-bad-record.qbw", true)] {
        let path = fixture_dir().join(name);
        let bytes = common::synthetic_qbw(bad);
        if std::env::var_os("OPENQBW_REGEN_FIXTURES").is_some() {
            std::fs::create_dir_all(fixture_dir()).unwrap();
            std::fs::write(&path, &bytes).unwrap();
        }
        let committed = std::fs::read(&path).unwrap();
        assert!(committed == bytes, "{name} is stale; regenerate it");
    }
}

#[test]
fn reads_catalog_headers_and_line_items() {
    let r = Reader::from_bytes(common::synthetic_qbw(false), &ReadOptions::default()).unwrap();
    assert_eq!(r.page_count(), 8);
    assert_eq!(r.attribution_gap(), None);
    let names: Vec<&str> = r.catalog().items.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names.len(), 3, "{names:?}");

    let headers = r.transactions().unwrap();
    assert_eq!(headers.items.len(), 1);
    assert_eq!(headers.items[0].qb_id, common::HEADER_ID);
    assert_eq!(headers.items[0].txn_type(), "invoice");
    assert_eq!(headers.items[0].txn_date_raw, Some(common::SA_DAY));

    let lines = r.line_items().unwrap();
    assert!(lines.rejected.is_empty());
    let amounts: Vec<(Option<i64>, AmountEncoding)> = lines
        .items
        .iter()
        .map(|l| (l.amount_cents, l.amount_encoding))
        .collect();
    assert_eq!(
        amounts,
        [
            (Some(266_048), AmountEncoding::UnsignedU24),
            (Some(266_112), AmountEncoding::UnsignedU24),
            (Some(77_120), AmountEncoding::SignBitU23),
            (Some(-77_120), AmountEncoding::SignBitU23),
        ]
    );
    for l in &lines.items {
        assert!(
            !l.flags.contains(&RecordFlag::KeystreamUnverified),
            "{:?}",
            l.flags
        );
        assert_eq!(l.txn_date_days_since_unix(), Some(19_800));
    }
    assert_eq!(
        lines.items[0].item_qb_id.as_deref(),
        Some("ITEM000000000001")
    );
}

#[test]
fn undecodable_record_is_an_error_unless_lenient() {
    let bytes = common::synthetic_qbw(true);
    let strict = Reader::from_bytes(bytes.clone(), &ReadOptions::strict()).unwrap();
    let e = strict.line_items().unwrap_err();
    assert_eq!(e.kind(), ErrorKind::UnknownAmountType);
    assert_eq!(e.page_number(), Some(7));

    let lenient = Reader::from_bytes(bytes, &ReadOptions::lenient()).unwrap();
    let scan = lenient.line_items().unwrap();
    assert_eq!(scan.items.len(), 4);
    assert_eq!(scan.rejected.len(), 1);
    assert_eq!(scan.rejected[0].kind(), ErrorKind::UnknownAmountType);
}

#[test]
fn not_a_company_file_is_a_format_error() {
    let mut bytes = common::synthetic_qbw(false);
    bytes[0x14] ^= 0xFF;
    let e = Reader::from_bytes(bytes, &ReadOptions::default()).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::BadMagic);
}

#[test]
fn damaged_page_is_an_error() {
    let mut bytes = common::synthetic_qbw(false);
    bytes[5 * common::PAGE + 0x700] ^= 0x01;
    let e = Reader::from_bytes(bytes, &ReadOptions::default()).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::BadCrc);
    assert_eq!(e.page_number(), Some(5));
}

#[test]
fn missing_file_is_an_io_error() {
    let e = Reader::open("/nonexistent/openqbw.qbw", &ReadOptions::default()).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Io);
}
