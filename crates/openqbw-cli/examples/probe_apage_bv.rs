//! WP-5D: validate `recover_bv_apage` (C.37 magic anchor) on real A-pages.
//!
//! For each A-page in the file:
//!   1. Run `recover_bv_apage` (new C.37 oracle).
//!   2. Run `recover_bv_qb_data` (existing C.36 oracle) for comparison.
//!   3. Report agreement, disagreement, and pages where only one oracle
//!      fires.
//!
//! Read-only diagnostic. Usage:
//!   cargo run --release --example probe_apage_bv -- <file.qbw>

use openqbw::{recover_bv_apage, recover_bv_qb_data};
use opensqlany::{PageStore, PageType};

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: probe_apage_bv <file.qbw>");
    let store = PageStore::open(&path).expect("open");

    let mut total_a = 0u64;
    let mut both_agree = 0u64;
    let mut both_disagree = 0u64;
    let mut only_apage = 0u64;
    let mut only_qb = 0u64;
    let mut neither = 0u64;

    for pn in 1..store.page_count() {
        let page = match store.page(pn) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if page.trailer().page_type() != PageType::Alloc {
            continue;
        }
        total_a += 1;
        let raw = page.bytes();
        let bv_a = recover_bv_apage(pn, raw);
        let bv_q = recover_bv_qb_data(pn, raw);
        match (bv_a, bv_q) {
            (Some(a), Some(q)) if a == q => both_agree += 1,
            (Some(_), Some(_)) => {
                both_disagree += 1;
                if both_disagree <= 6 {
                    println!(
                        "pn={pn:6}: apage={:?} qb={:?} (disagree)",
                        bv_a, bv_q
                    );
                }
            }
            (Some(_), None) => {
                only_apage += 1;
                if only_apage <= 6 {
                    println!("pn={pn:6}: apage={:?} qb=None", bv_a);
                }
            }
            (None, Some(_)) => {
                only_qb += 1;
                if only_qb <= 6 {
                    println!("pn={pn:6}: apage=None qb={:?}", bv_q);
                }
            }
            (None, None) => {
                neither += 1;
            }
        }
    }

    println!();
    println!("== A-page bv recovery summary ==");
    println!("total A-pages   : {total_a}");
    println!("both agree      : {both_agree}");
    println!("both disagree   : {both_disagree}");
    println!("only apage(C.37): {only_apage}");
    println!("only qb_data(C.36): {only_qb}");
    println!("neither         : {neither}");
    let recovered = both_agree + both_disagree + only_apage + only_qb;
    if total_a > 0 {
        let pct_apage =
            (both_agree + both_disagree + only_apage) as f64 * 100.0 / total_a as f64;
        let pct_qb = (both_agree + both_disagree + only_qb) as f64 * 100.0 / total_a as f64;
        let pct_any = recovered as f64 * 100.0 / total_a as f64;
        println!("apage coverage  : {pct_apage:6.2}%");
        println!("qb    coverage  : {pct_qb:6.2}%");
        println!("either coverage : {pct_any:6.2}%");
    }
}
