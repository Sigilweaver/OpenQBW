//! WP-5B prototype: brute-force bv selection by maximising total decoded zeros.
//!
//! For each E-page that the existing decoders fail to crack, sweep bv across
//! 0..256 and pick the value that yields the most zero bytes in the decoded
//! body. Report success rate on the WP-5A residual-failure population.

use openqbw::{deobfuscate_with_bv, oracle_bv_e_page, recover_bv_qb_data};
use opensqlany::{ApModel, PageStore, PageType};

fn zero_score(plain: &[u8]) -> usize {
    plain[..0xFF0].iter().filter(|&&b| b == 0).count()
}

fn brute_bv(raw: &[u8], pn: u64) -> (u8, usize) {
    let mut best_bv = 0u8;
    let mut best_zeros = 0usize;
    for bv in 0u16..=255 {
        let bv = bv as u8;
        let plain = deobfuscate_with_bv(raw, pn, bv);
        let z = zero_score(&plain);
        if z > best_zeros {
            best_zeros = z;
            best_bv = bv;
        }
    }
    (best_bv, best_zeros)
}

fn existing_decode_fails(raw: &[u8], pn: u64, model: &ApModel, store: &PageStore) -> bool {
    fn looks_decoded(plain: &[u8]) -> bool {
        let body = &plain[..0xFF0];
        let zeros = body.iter().filter(|&&b| b == 0).count();
        if zeros * 100 / body.len() >= 5 {
            return true;
        }
        let mut h = [0u32; 256];
        for &b in body {
            h[b as usize] += 1;
        }
        let (mb, &mc) = h
            .iter()
            .enumerate()
            .max_by_key(|(_, c)| *c)
            .map(|(b, c)| (b as u8, c))
            .unwrap();
        (mc as usize) * 100 / body.len() >= 20 && mb < 0x40
    }
    if let Some(bv) = recover_bv_qb_data(pn, raw) {
        if looks_decoded(&deobfuscate_with_bv(raw, pn, bv)) {
            return false;
        }
    }
    let obv = oracle_bv_e_page(pn, raw);
    if looks_decoded(&deobfuscate_with_bv(raw, pn, obv)) {
        return false;
    }
    if looks_decoded(&model.deobfuscate_with_store(raw, pn, store)) {
        return false;
    }
    true
}

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).unwrap();
    let store = PageStore::open(&path)?;
    let model = ApModel::learn(&store);
    let total = store.page_count();

    let mut failed = Vec::new();
    for pn in 0..total {
        let page = match store.page(pn) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if page.trailer().page_type() != PageType::Extent {
            continue;
        }
        let raw = page.bytes();
        if existing_decode_fails(raw, pn, &model, &store) {
            failed.push((pn, raw.to_vec()));
        }
    }

    println!("residual failures from WP-5A: {}", failed.len());
    let mut bf_strong = 0; // >= 5% zeros
    let mut bf_high = 0; // >= 20% zeros
    let mut samples = 0;
    for (pn, raw) in &failed {
        let (bv, z) = brute_bv(raw, *pn);
        let pct = z * 100 / 0xFF0;
        if pct >= 5 {
            bf_strong += 1;
        }
        if pct >= 20 {
            bf_high += 1;
        }
        if samples < 8 {
            println!(
                "  pn={:5}  best_bv={:#04x}  zeros={:.1}%",
                pn, bv, pct as f64
            );
            samples += 1;
        }
    }
    println!(
        "brute_bv rescue:  >=5%: {}/{}  >=20%: {}/{}",
        bf_strong,
        failed.len(),
        bf_high,
        failed.len()
    );
    Ok(())
}
