//! WP-5A: decode-failure population recon.
//!
//! Classify every page in a QBW file into one of:
//!   * decoded_ok_qb     - recover_bv_qb_data succeeded AND slot dir parsed
//!   * decoded_ok_oracle - oracle_bv_e_page produced a parseable slot dir
//!   * decoded_ok_model  - model.deobfuscate_with_store produced a parseable dir
//!   * uniform_cipher    - raw page is a uniform-fill ciphertext (unwritten)
//!   * uniform_plain     - decoded plaintext is uniform / all-zero
//!   * decode_failed     - no decoder yielded a parseable structure
//!   * non_e_meta        - A/I/M/H/C/@/G or other meta page type
//!
//! Emits a histogram + 5 sample page numbers per class for follow-up.

use openqbw::{
    deobfuscate_with_bv, is_opaque_high_entropy, oracle_bv_e_page, recover_bv_brute,
    recover_bv_qb_data,
};
use opensqlany::{ApModel, Page, PageStore, PageType, SlottedPage};
use std::collections::BTreeMap;

#[derive(Copy, Clone, Debug, Ord, PartialOrd, Eq, PartialEq)]
enum Class {
    NonEMeta(u8),
    DecodedRowsQb,
    DecodedRowsOracle,
    DecodedRowsBrute,
    DecodedRowsModel,
    DecodedEmptyQb,
    DecodedEmptyOracle,
    DecodedEmptyBrute,
    DecodedEmptyModel,
    UniformCipher,
    OpaqueHighEntropy,
    DecodeFailed,
}

fn class_label(c: Class) -> String {
    match c {
        Class::NonEMeta(t) => format!("non_e_meta:{:#04x}", t),
        Class::DecodedRowsQb => "decoded_rows_qb".into(),
        Class::DecodedRowsOracle => "decoded_rows_oracle".into(),
        Class::DecodedRowsBrute => "decoded_rows_brute".into(),
        Class::DecodedRowsModel => "decoded_rows_model".into(),
        Class::DecodedEmptyQb => "decoded_empty_qb".into(),
        Class::DecodedEmptyOracle => "decoded_empty_oracle".into(),
        Class::DecodedEmptyBrute => "decoded_empty_brute".into(),
        Class::DecodedEmptyModel => "decoded_empty_model".into(),
        Class::UniformCipher => "uniform_cipher".into(),
        Class::OpaqueHighEntropy => "opaque_high_entropy".into(),
        Class::DecodeFailed => "decode_failed".into(),
    }
}

fn is_uniform(buf: &[u8]) -> bool {
    if buf.is_empty() {
        return true;
    }
    let b0 = buf[0];
    buf.iter().all(|&b| b == b0)
}

fn try_parse(pn: u64, plain: &[u8]) -> bool {
    let p = Page::from_bytes(pn, plain);
    let sp = SlottedPage::parse(p);
    if let Some(d) = sp.directory.as_ref() {
        d.live_count() > 0 && d.array_start < d.end && d.end <= 4096
    } else {
        false
    }
}

/// Decode plausibility check based on byte-frequency structure.
///
/// Encrypted/random data is uniformly distributed (~0.4% of any specific
/// byte value). Decoded SA17 plaintext exhibits one of:
///   * many zero bytes (length fields, padding, NULLs) — `zeros / 4080 >= 5%`
///   * a dominant low-value byte (e.g. 0x10 for some E-pages where the
///     ciphertext step happens to make the modal plaintext byte non-zero) —
///     mode count >= 20% AND mode byte < 0x40.
fn looks_decoded(plain: &[u8]) -> bool {
    let body = &plain[..0xFF0];
    let zeros = body.iter().filter(|&&b| b == 0).count();
    if zeros * 100 / body.len() >= 5 {
        return true;
    }
    let mut hist = [0u32; 256];
    for &b in body {
        hist[b as usize] += 1;
    }
    let (mode_byte, &mode_count) = hist
        .iter()
        .enumerate()
        .max_by_key(|(_, c)| *c)
        .map(|(b, c)| (b as u8, c))
        .unwrap();
    (mode_count as usize) * 100 / body.len() >= 20 && mode_byte < 0x40
}

fn classify(pn: u64, page: &Page<'_>, model: &ApModel, store: &PageStore) -> Class {
    let pt = page.trailer().page_type();
    if pt != PageType::Extent {
        return Class::NonEMeta(pt.as_byte());
    }
    let raw = page.bytes();

    // 1. Uniform-fill ciphertext = unwritten page allocated as E.
    if is_uniform(&raw[..0xFF0]) {
        return Class::UniformCipher;
    }

    // 2. QB d5 0b anchor decode.
    if let Some(bv) = recover_bv_qb_data(pn, raw) {
        let plain = deobfuscate_with_bv(raw, pn, bv);
        if try_parse(pn, &plain) {
            return Class::DecodedRowsQb;
        }
        if looks_decoded(&plain) {
            return Class::DecodedEmptyQb;
        }
    }

    // 3. Oracle bv (plain[0] == 0 assumption).
    let obv = oracle_bv_e_page(pn, raw);
    let plain = deobfuscate_with_bv(raw, pn, obv);
    if try_parse(pn, &plain) {
        return Class::DecodedRowsOracle;
    }
    if looks_decoded(&plain) {
        return Class::DecodedEmptyOracle;
    }

    // 4. Brute-force bv search (max zero count).
    if let Some(bv) = recover_bv_brute(pn, raw) {
        let plain = deobfuscate_with_bv(raw, pn, bv);
        if try_parse(pn, &plain) {
            return Class::DecodedRowsBrute;
        }
        if looks_decoded(&plain) {
            return Class::DecodedEmptyBrute;
        }
    }

    // 5. Model fallback.
    let plain = model.deobfuscate_with_store(raw, pn, store);
    if try_parse(pn, &plain) {
        return Class::DecodedRowsModel;
    }
    if looks_decoded(&plain) {
        return Class::DecodedEmptyModel;
    }

    // 6. None of the bv recovery tiers produced structured plaintext. If
    //    the page body is uniformly random it is opaque (likely a
    //    compressed/encrypted blob page); otherwise treat as a true decode
    //    failure that warrants further investigation.
    if is_opaque_high_entropy(raw) {
        return Class::OpaqueHighEntropy;
    }

    Class::DecodeFailed
}

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("usage: probe_decode_coverage <file.qbw>");
    let store = PageStore::open(&path)?;
    let model = ApModel::learn(&store);
    let total = store.page_count();

    let mut hist: BTreeMap<Class, Vec<u64>> = BTreeMap::new();

    for pn in 0..total {
        let page = match store.page(pn) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let class = classify(pn, &page, &model, &store);
        hist.entry(class).or_default().push(pn);
    }

    let label = std::path::Path::new(&path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.clone());
    println!("## decode coverage: {}", label);
    println!("  total pages: {}", total);

    let total_f = total as f64;
    let mut grand_decoded = 0u64;
    let mut grand_provably_empty = 0u64;
    let mut grand_non_e_meta = 0u64;
    let mut grand_opaque = 0u64;
    let mut grand_failed = 0u64;
    for (cls, pages) in &hist {
        let n = pages.len();
        let sample: Vec<u64> = pages.iter().take(5).copied().collect();
        let pct = (n as f64) / total_f * 100.0;
        println!("  {:<26}  {:>6} pages  ({:5.1}%)  sample={:?}", class_label(*cls), n, pct, sample);
        match cls {
            Class::DecodedRowsQb
            | Class::DecodedRowsOracle
            | Class::DecodedRowsBrute
            | Class::DecodedRowsModel
            | Class::DecodedEmptyQb
            | Class::DecodedEmptyOracle
            | Class::DecodedEmptyBrute
            | Class::DecodedEmptyModel => grand_decoded += n as u64,
            Class::UniformCipher => grand_provably_empty += n as u64,
            Class::NonEMeta(_) => grand_non_e_meta += n as u64,
            Class::OpaqueHighEntropy => grand_opaque += n as u64,
            Class::DecodeFailed => grand_failed += n as u64,
        }
    }
    println!("  ---");
    println!("  decoded_ok        : {} ({:.1}%)", grand_decoded, grand_decoded as f64 / total_f * 100.0);
    println!("  provably_empty    : {} ({:.1}%)", grand_provably_empty, grand_provably_empty as f64 / total_f * 100.0);
    println!("  non_e_meta        : {} ({:.1}%)", grand_non_e_meta, grand_non_e_meta as f64 / total_f * 100.0);
    println!("  opaque_high_entropy: {} ({:.1}%)", grand_opaque, grand_opaque as f64 / total_f * 100.0);
    println!("  decode_failed     : {} ({:.1}%)", grand_failed, grand_failed as f64 / total_f * 100.0);
    let classified = grand_decoded + grand_provably_empty + grand_non_e_meta + grand_opaque;
    println!("  Gate 1 coverage   : {}/{} = {:.2}%", classified, total, classified as f64 / total_f * 100.0);

    Ok(())
}
