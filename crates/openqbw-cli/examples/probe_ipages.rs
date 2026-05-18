//! WP-4B: dump every I-page (PageType::Index) in the file, decode under
//! the AP model, and print plaintext hex + ASCII for structural analysis.
//!
//! Interior B-tree nodes typically have:
//!   * header u16/u32 with entry count
//!   * variable-length entries: [key_len: u16][key_bytes][child_page: u32 LE]
//!   * possibly a trailing high-key pointer
//!
//! We emit:
//!   * full 4 KiB hex dump (offset / 16-byte rows) of each I-page
//!   * the SA17 trailer (last 16 bytes) parsed
//!   * scan for plausible "page number" u32 LE values (< total_pages) that
//!     repeat at fixed strides (B-tree fanout fingerprint)

use openqbw::{deobfuscate_with_bv, recover_bv_qb_data};
use opensqlany::{ApModel, PageStore, PageType};

fn hex_row(off: usize, row: &[u8]) -> String {
    let hex: String = row.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" ");
    let ascii: String = row
        .iter()
        .map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' })
        .collect();
    format!("  {:04x}  {:<47}  |{}|", off, hex, ascii)
}

fn dump_page(pn: u64, plain: &[u8], full: bool) {
    println!("\n=== I-page pn={} ===", pn);
    let n = if full { plain.len() } else { 256 };
    for off in (0..n).step_by(16) {
        let end = (off + 16).min(plain.len());
        println!("{}", hex_row(off, &plain[off..end]));
    }
    // trailer at 0xFF0..0x1000
    println!("  --- trailer 0xff0..0x1000 ---");
    let end = plain.len();
    let trailer = &plain[end - 16..];
    println!("{}", hex_row(end - 16, trailer));
}

fn scan_page_refs(pn: u64, plain: &[u8], max_pn: u32) -> Vec<(usize, u32)> {
    let mut out = Vec::new();
    // Skip header (first 16 bytes) and trailer (last 16 bytes).
    for off in (16..plain.len() - 16).step_by(2) {
        if off + 4 > plain.len() {
            break;
        }
        let v = u32::from_le_bytes([plain[off], plain[off + 1], plain[off + 2], plain[off + 3]]);
        if v > 0 && v != pn as u32 && v < max_pn {
            out.push((off, v));
        }
    }
    out
}

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).unwrap();
    let full_dump = std::env::args().any(|a| a == "--full");
    let store = PageStore::open(&path)?;
    let model = ApModel::learn(&store);
    let total = store.page_count() as u32;

    // Find all I-pages.
    let mut i_pages = Vec::new();
    for pn in 0..store.page_count() {
        if let Ok(page) = store.page(pn) {
            if page.trailer().page_type() == PageType::Index {
                i_pages.push(pn);
            }
        }
    }
    println!("# I-pages found: {}", i_pages.len());
    println!("# total pages: {}", total);

    for &pn in &i_pages {
        let page = store.page(pn)?;
        let raw = page.bytes();
        let plain = if let Some(bv) = recover_bv_qb_data(pn, raw) {
            deobfuscate_with_bv(raw, pn, bv)
        } else {
            model.deobfuscate_with_store(raw, pn, &store)
        };

        dump_page(pn, &plain, full_dump);

        // Scan for plausible child page refs.
        let refs = scan_page_refs(pn, &plain, total);
        // Reduce noise: only report u32 values that look like real page
        // numbers AND have at least 3-byte stride symmetry, but keep it
        // simple here: print a histogram of which offsets have references.
        println!("  # plausible u32 page refs (val < total_pages): {}", refs.len());
        if !refs.is_empty() {
            // Print first 20 refs as (offset, page).
            let head = refs.iter().take(20)
                .map(|(o, v)| format!("({:#x}={})", o, v))
                .collect::<Vec<_>>().join(", ");
            println!("  first refs: {}", head);
            // Detect strides.
            let mut strides: std::collections::BTreeMap<usize, usize> = Default::default();
            for w in refs.windows(2) {
                let d = w[1].0 - w[0].0;
                *strides.entry(d).or_default() += 1;
            }
            let mut sv: Vec<(usize, usize)> = strides.into_iter().collect();
            sv.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
            let top: String = sv.iter().take(6).map(|(s, c)| format!("(d={} n={})", s, c)).collect::<Vec<_>>().join(" ");
            println!("  ref stride hist: {}", top);
        }
    }

    Ok(())
}
