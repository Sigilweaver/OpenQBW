//! WP-5C diagnostic: characterise pages that resist all bv-recovery
//! strategies. For each page that `recover_bv_any` cannot crack at >=5%
//! zeros, dump a structural fingerprint:
//!
//! * trailer page-type byte (which sub-class of E page),
//! * raw-byte entropy (close to 8.0 = random, low = sparse/structured),
//! * top three repeating bytes and their frequencies,
//! * length of the longest run of identical bytes,
//! * whether the trailer page-type indicates A/M/H/C/@/I/G instead of E.
//!
//! Aggregates the per-page fingerprint into a global summary so we can
//! decide if the residual is a single structural class (one decoder to
//! write) or many (out-of-scope ciphertext).

use openqbw::{deobfuscate_with_bv, oracle_bv_e_page, recover_bv_brute, recover_bv_qb_data};
use opensqlany::{ApModel, PageStore, PageType, SlottedPage};
use std::collections::BTreeMap;

fn entropy(bytes: &[u8]) -> f64 {
    let mut hist = [0u32; 256];
    for &b in bytes {
        hist[b as usize] += 1;
    }
    let total = bytes.len() as f64;
    let mut h = 0.0;
    for &c in &hist {
        if c == 0 {
            continue;
        }
        let p = c as f64 / total;
        h -= p * p.log2();
    }
    h
}

fn top_bytes(bytes: &[u8], k: usize) -> Vec<(u8, u32)> {
    let mut hist = [0u32; 256];
    for &b in bytes {
        hist[b as usize] += 1;
    }
    let mut v: Vec<(u8, u32)> = hist
        .iter()
        .enumerate()
        .map(|(i, &c)| (i as u8, c))
        .collect();
    v.sort_by_key(|&(_, c)| std::cmp::Reverse(c));
    v.truncate(k);
    v
}

fn longest_run(bytes: &[u8]) -> usize {
    let mut best = 0usize;
    let mut cur = 0usize;
    let mut last: Option<u8> = None;
    for &b in bytes {
        if Some(b) == last {
            cur += 1;
        } else {
            cur = 1;
            last = Some(b);
        }
        if cur > best {
            best = cur;
        }
    }
    best
}

/// Apply the same cascade probe_decode_coverage uses; return Some(plain) if
/// any tier succeeds at >=5% zeros, else None.
fn try_decode(pn: u64, raw: &[u8], model: &ApModel, store: &PageStore) -> Option<Vec<u8>> {
    let looks = |p: &[u8]| {
        let body = &p[..0xFF0];
        let zeros = body.iter().filter(|&&b| b == 0).count();
        if zeros * 20 >= body.len() {
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
            .map(|(i, c)| (i as u8, c))
            .unwrap();
        mode_byte < 0x40 && (mode_count as usize) * 5 >= body.len()
    };
    if let Some(bv) = recover_bv_qb_data(pn, raw) {
        let p = deobfuscate_with_bv(raw, pn, bv);
        if looks(&p) {
            return Some(p);
        }
    }
    let bv = oracle_bv_e_page(pn, raw);
    let p = deobfuscate_with_bv(raw, pn, bv);
    if looks(&p) {
        return Some(p);
    }
    if let Some(bv) = recover_bv_brute(pn, raw) {
        let p = deobfuscate_with_bv(raw, pn, bv);
        if looks(&p) {
            return Some(p);
        }
    }
    let p = model.deobfuscate_with_store(raw, pn, store);
    if looks(&p) {
        return Some(p);
    }
    None
}

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .expect("usage: probe_failed_pages <qbw>");
    let store = PageStore::open(&path)?;
    let model = ApModel::learn(&store);

    let mut failed: Vec<u64> = Vec::new();
    let mut trailer_type_counts: BTreeMap<u8, u64> = BTreeMap::new();
    let mut total_e_pages = 0u64;
    let n = store.page_count();
    for pn in 1..n {
        let page = store.page(pn)?;
        let pt = page.trailer().page_type();
        if pt != PageType::Extent {
            continue;
        }
        total_e_pages += 1;
        let raw = page.bytes();
        if try_decode(pn, raw, &model, &store).is_some() {
            continue;
        }
        failed.push(pn);
        let tb = raw[0xFFC];
        *trailer_type_counts.entry(tb).or_default() += 1;
    }
    eprintln!(
        "{}: {} E-pages, {} failed to decode",
        path,
        total_e_pages,
        failed.len()
    );

    // Aggregate fingerprints.
    let mut entropy_buckets: BTreeMap<u32, u64> = BTreeMap::new();
    let mut run_buckets: BTreeMap<u32, u64> = BTreeMap::new();
    let mut sample_done = 0;
    for &pn in &failed {
        let page = store.page(pn)?;
        let body = &page.bytes()[..0xFF0];
        let e = entropy(body);
        // bucket entropy in 0.5-wide bins
        let bucket = (e * 2.0).floor() as u32;
        *entropy_buckets.entry(bucket).or_default() += 1;
        let lr = longest_run(body);
        let rb = match lr {
            0..=3 => 0,
            4..=7 => 4,
            8..=15 => 8,
            16..=31 => 16,
            32..=63 => 32,
            64..=127 => 64,
            128..=255 => 128,
            _ => 256,
        };
        *run_buckets.entry(rb).or_default() += 1;

        if sample_done < 5 {
            let raw = page.bytes();
            let top = top_bytes(body, 3);
            println!(
                "  pn={} trailer_type=0x{:02x} entropy={:.3} longest_run={} top={:?}",
                pn,
                raw[0xFFC],
                e,
                lr,
                top.iter()
                    .map(|(b, c)| format!("{:#04x}x{}", b, c))
                    .collect::<Vec<_>>()
            );
            println!("    body[0..64]={:02x?}", &body[..64]);
            // Trailer 0xFF0..=0xFFB
            println!(
                "    trailer[0xFF0..0xFFC]={:02x?}  type=0x{:02x}",
                &raw[0xFF0..0xFFC],
                raw[0xFFC]
            );
            sample_done += 1;
        }
    }
    println!("--- aggregate ---");
    println!("trailer page-type byte distribution:");
    for (t, c) in &trailer_type_counts {
        println!("  0x{:02x}  {} pages", t, c);
    }
    println!("entropy bucket (bin width 0.5):");
    for (b, c) in &entropy_buckets {
        println!(
            "  [{:.1}, {:.1})  {} pages",
            *b as f64 / 2.0,
            (*b as f64 + 1.0) / 2.0,
            c
        );
    }
    println!("longest-run buckets:");
    for (b, c) in &run_buckets {
        println!("  >={}  {} pages", b, c);
    }

    // Try sibling-bv hypothesis: do failed pages have neighbors that decode?
    // If neighboring pages (pn-1, pn+1, pn within same 8-page block) share
    // a bv, we could borrow it. Sample first 8 failed pages.
    println!("--- sibling-bv hypothesis (first 8 failures) ---");
    for &pn in failed.iter().take(8) {
        let block = pn / 8;
        let mut neighbors: Vec<(u64, Option<u8>)> = Vec::new();
        for sib in (block * 8)..((block + 1) * 8).min(n) {
            if sib == pn {
                continue;
            }
            let sp = store.page(sib)?;
            if sp.trailer().page_type() != PageType::Extent {
                continue;
            }
            let sraw = sp.bytes();
            let bv = recover_bv_qb_data(sib, sraw).or_else(|| recover_bv_brute(sib, sraw));
            neighbors.push((sib, bv));
        }
        let raw = store.page(pn)?.bytes().to_vec();
        let mut rescued = None;
        for (_, bv_opt) in neighbors.iter() {
            let Some(bv) = bv_opt else { continue };
            let p = deobfuscate_with_bv(&raw, pn, *bv);
            let zeros = p[..0xFF0].iter().filter(|&&b| b == 0).count();
            if zeros * 20 >= 0xFF0 {
                rescued = Some(*bv);
                break;
            }
        }
        println!(
            "  pn={} block={} neighbors={:?} rescued_by={:?}",
            pn,
            block,
            neighbors
                .iter()
                .map(|(s, b)| format!(
                    "{}:{}",
                    s,
                    b.map(|x| format!("0x{:02x}", x)).unwrap_or("?".into())
                ))
                .collect::<Vec<_>>(),
            rescued.map(|b| format!("0x{:02x}", b))
        );
    }

    // Quietly suppress unused-warning if any.
    let _ = SlottedPage::parse;
    Ok(())
}
