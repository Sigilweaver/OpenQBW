//! `openqbw` command-line tool.
//!
//! Subcommands:
//!
//! ```text
//! openqbw export  <input.qbw> <output.db>   # multi-transaction SQLite export
//! openqbw catalog <input.qbw>               # SYSTABLE listing
//! openqbw verify  <input.qbw>               # validation summary
//! ```

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use openqbw::{
    iter_lineitems_with_attribution, iter_transaction_headers, AmountType, ContentAttribution,
    LineItem, PageAttribution, SysTableEntry, TransactionHeader,
};
use opensqlany::{ApModel, PageStore};
use rusqlite::{params, Connection, Transaction};

const PHASE5_INVOICE_TOTAL_CENTS: i64 = 39_991_479_278;
const PHASE5_INVOICE_COUNT: i64 = 13_375;

#[derive(Parser, Debug)]
#[command(name = "openqbw", version, about = "QuickBooks .qbw inspector and exporter")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Export transactions and line items to SQLite, attributing each
    /// line item to its source table via SYSTABLE.
    Export {
        /// Input QBW file.
        input: PathBuf,
        /// Output SQLite database (will be overwritten).
        output: PathBuf,
    },
    /// List the SYSTABLE catalog (table_id, name, root page) recovered
    /// directly from the QBW file.
    Catalog {
        /// Input QBW file.
        input: PathBuf,
        /// Show only user-looking tables (skip SYS*/ISYS*/RS_*/dbo*).
        #[arg(long)]
        user_only: bool,
    },
    /// Validate an export against known invariants (invoice regression,
    /// journal sum-to-zero, per-table coverage).
    Verify {
        /// Input QBW file.
        input: PathBuf,
        /// Optional output SQLite path. Defaults to a temp file.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Also build a content-signature attribution map and report
        /// per-page agreement against the default position-based
        /// attribution.
        #[arg(long)]
        strict_attribution: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Export { input, output } => run_export(input, output).map(|_| ()),
        Cmd::Catalog { input, user_only } => run_catalog(input, user_only),
        Cmd::Verify {
            input,
            output,
            strict_attribution,
        } => run_verify(input, output, strict_attribution),
    }
}

fn run_export(input: PathBuf, output: PathBuf) -> Result<ExportStats> {
    if output.exists() {
        std::fs::remove_file(&output)
            .with_context(|| format!("removing existing {:?}", output))?;
    }

    let store = PageStore::open(&input)
        .with_context(|| format!("opening {:?}", input))?;
    let model = ApModel::learn(&store);
    let attribution = PageAttribution::build(&store, &model);

    let mut items: Vec<LineItem> =
        iter_lineitems_with_attribution(&store, &model, &attribution).collect();
    items.sort_by_key(|li| (li.page_number, li.page_offset));

    let mut headers: Vec<TransactionHeader> =
        iter_transaction_headers(&store, &model, &attribution).collect();
    headers.sort_by_key(|h| (h.page_number, h.page_offset));

    let mut conn = Connection::open(&output)
        .with_context(|| format!("opening {:?}", output))?;
    create_schema(&conn)?;

    {
        let tx = conn.transaction()?;
        let header_map = insert_transaction_headers(&tx, &headers)?;
        insert_synthesized_transactions(&tx, &items, &header_map)?;
        insert_transaction_line_items(&tx, &items)?;
        tx.commit()?;
    }

    let stats = collect_export_stats(&conn, &store, &items, &headers)?;
    println!("{}", stats.summary());
    Ok(stats)
}

fn run_catalog(input: PathBuf, user_only: bool) -> Result<()> {
    let store = PageStore::open(&input)
        .with_context(|| format!("opening {:?}", input))?;
    let model = ApModel::learn(&store);

    let mut entries: Vec<SysTableEntry> = openqbw::collect_unique(&store, &model);
    entries.sort_by_key(|e| e.table_id);

    let total = entries.len();
    let user: Vec<&SysTableEntry> = entries
        .iter()
        .filter(|e| !is_system_name(&e.name))
        .collect();

    println!(
        "file: {}  pages={}  unique_tables={}  user_tables={}",
        input.file_name().and_then(|s| s.to_str()).unwrap_or("?"),
        store.page_count(),
        total,
        user.len(),
    );
    println!("{:>6}  {:>5}  {:>6}  {:>6}  {}", "tid", "cols", "root", "last", "name");
    let iter: Box<dyn Iterator<Item = &SysTableEntry>> = if user_only {
        Box::new(user.into_iter())
    } else {
        Box::new(entries.iter())
    };
    for e in iter {
        let cols = e.col_count.map(|c| c.to_string()).unwrap_or_else(|| "-".into());
        let root = e.data_root_page.map(|p| p.to_string()).unwrap_or_else(|| "-".into());
        let last = e.last_page.map(|p| p.to_string()).unwrap_or_else(|| "-".into());
        println!("{:>6}  {:>5}  {:>6}  {:>6}  {}", e.table_id, cols, root, last, e.name);
    }
    Ok(())
}

fn run_verify(
    input: PathBuf,
    output: Option<PathBuf>,
    strict_attribution: bool,
) -> Result<()> {
    let out = match output {
        Some(p) => p,
        None => std::env::temp_dir().join(format!(
            "openqbw-verify-{}.sqlite",
            std::process::id()
        )),
    };
    let stats = run_export(input.clone(), out.clone())?;
    println!();
    println!("=== verification report ===");

    let conn = Connection::open(&out)?;
    println!();
    println!("Per source_table line-item counts:");
    let mut stmt = conn.prepare(
        "SELECT source_table, COUNT(*), COALESCE(SUM(amount_cents), 0)
         FROM transaction_line_items
         GROUP BY source_table
         ORDER BY source_table",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
    })?;
    for row in rows {
        let (t, n, sum) = row?;
        println!("  {:44}  {:>8}  ${:>15.2}", t, n, sum as f64 / 100.0);
    }

    println!();
    println!("Per type transaction counts:");
    let mut stmt = conn.prepare(
        "SELECT type, COUNT(*), COALESCE(SUM(total_cents), 0)
         FROM transactions
         GROUP BY type
         ORDER BY type",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
    })?;
    for row in rows {
        let (t, n, sum) = row?;
        println!("  {:44}  {:>8}  ${:>15.2}", t, n, sum as f64 / 100.0);
    }

    println!();
    let parent_count: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT qb_id_parent) FROM transaction_line_items",
        [],
        |r| r.get(0),
    )?;
    let grand_total: i64 = conn.query_row(
        "SELECT COALESCE(SUM(amount_cents), 0) FROM transaction_line_items",
        [],
        |r| r.get(0),
    )?;
    let regression_ok = grand_total == PHASE5_INVOICE_TOTAL_CENTS
        && parent_count == PHASE5_INVOICE_COUNT;
    println!(
        "Phase 5 regression (universal anchor):  parents={}/{}  grand_total=${:.2}/${:.2}  {}",
        parent_count,
        PHASE5_INVOICE_COUNT,
        grand_total as f64 / 100.0,
        PHASE5_INVOICE_TOTAL_CENTS as f64 / 100.0,
        if regression_ok { "PASS" } else { "DIFF" },
    );

    // Per-source-table parent counts (gives a sense of attribution
    // distribution; expect most parents under the dominant lineitem
    // tables on Rock Castle: abmc_invoice_inventory_lineitem,
    // abmc_general_journal_inventory_lineitem, abmc_credit_memo_inventory_lineitem).
    println!();
    println!("Top 10 source_tables by distinct parent QB-IDs:");
    let mut stmt = conn.prepare(
        "SELECT source_table, COUNT(DISTINCT qb_id_parent) as n
         FROM transaction_line_items
         GROUP BY source_table
         ORDER BY n DESC
         LIMIT 10",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    for row in rows {
        let (t, n) = row?;
        println!("  {:44}  {:>8}", t, n);
    }

    println!();
    println!("Journal sum-to-zero check (Phase 2.2, signed amounts, C.48 Track A):");
    // Same-type 2-line journal pair-balance: the strictest case where the
    // high-bit-of-byte-1 sign hypothesis is unambiguous. Acceptance target:
    // 100% same-type pair-balance.
    let pair_stats: (i64, i64) = conn.query_row(
        "SELECT
           COUNT(*) AS pairs,
           SUM(CASE WHEN signed_sum = 0 THEN 1 ELSE 0 END) AS balanced
         FROM (
           SELECT qb_id_parent,
                  SUM(amount_cents_signed) AS signed_sum,
                  COUNT(*) AS n,
                  COUNT(DISTINCT amount_type) AS types
           FROM transaction_line_items
           WHERE source_table = 'abmc_general_journal_inventory_lineitem'
             AND amount_type IN (1, 2)
             AND amount_cents_signed IS NOT NULL
           GROUP BY qb_id_parent
           HAVING n = 2 AND types = 1
         )",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let (pairs, balanced) = pair_stats;
    if pairs == 0 {
        println!("  no same-type 2-line journal pairs found");
    } else {
        let pct = (balanced as f64) / (pairs as f64) * 100.0;
        let ok = balanced == pairs;
        println!(
            "  same-type 2-line journal pairs: {}/{} balance ({:.1}%) {}",
            balanced,
            pairs,
            pct,
            if ok { "PASS" } else { "DIFF" },
        );
    }

    // Broader (all journals) for visibility - not an acceptance criterion
    // because 0x03 entries are not amount records (see C.48 Track A) and
    // attribution is fuzzy (see C.47).
    let all_stats: (i64, i64) = conn.query_row(
        "SELECT
           COUNT(*) AS p,
           SUM(CASE WHEN signed_sum = 0 THEN 1 ELSE 0 END) AS bal
         FROM (
           SELECT qb_id_parent,
                  COALESCE(SUM(amount_cents_signed), 0) AS signed_sum
           FROM transaction_line_items
           WHERE source_table = 'abmc_general_journal_inventory_lineitem'
           GROUP BY qb_id_parent
         )",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let (allp, allb) = all_stats;
    if allp > 0 {
        println!(
            "  all journal-attributed parents (signed sum incl. type-0x03 nulls): {}/{} ({:.1}%)",
            allb,
            allp,
            (allb as f64) / (allp as f64) * 100.0,
        );
    }
    println!(
        "  note: type-0x03 entries are NOT signed amounts (727 distinct values across"
    );
    println!(
        "        11,754 occurrences in Rock Castle - see NOTES.md C.48 Track A)."
    );

    if strict_attribution {
        println!();
        println!("=== content-signature attribution (--strict-attribution) ===");
        let store = PageStore::open(&input)
            .with_context(|| format!("re-opening {:?}", input))?;
        let model = ApModel::learn(&store);
        let content = ContentAttribution::build(&store, &model);
        let position = PageAttribution::build(&store, &model);
        println!(
            "  unique signatures: {}  ambiguous: {}  skipped roots: {}",
            content.len(),
            content.ambiguous_count(),
            content.skipped_count(),
        );

        // Collect the distinct E-page numbers that contributed at least
        // one line item, so the comparison runs over the pages we
        // actually attribute in production.
        let mut stmt = conn.prepare(
            "SELECT DISTINCT page_number FROM transaction_line_items ORDER BY page_number",
        )?;
        let pages: Vec<u64> = stmt
            .query_map([], |r| r.get::<_, i64>(0))?
            .filter_map(|r| r.ok())
            .map(|n| n as u64)
            .collect();
        let agree = content.compare(&store, &model, &position, pages.iter().copied());
        let total = agree.total().max(1);
        println!(
            "  pages compared: {}  agree: {} ({:.2}%)  disagree: {}  only_position: {}  only_content: {}  neither: {}",
            agree.total(),
            agree.agree,
            (agree.agree as f64) * 100.0 / (total as f64),
            agree.disagree,
            agree.only_position,
            agree.only_content,
            agree.neither,
        );
    }

    println!();
    println!("Overall: {}", stats.summary());
    Ok(())
}

fn is_system_name(name: &str) -> bool {
    name.starts_with("SYS")
        || name.starts_with("ISYS")
        || name.starts_with("RS_")
        || name.starts_with("dbo")
}

fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE transactions (
            qb_id        TEXT PRIMARY KEY,
            type         TEXT NOT NULL,
            source_table TEXT NOT NULL,
            txn_date_raw INTEGER,
            counter      INTEGER,
            line_count   INTEGER NOT NULL,
            total_cents  INTEGER NOT NULL,
            has_deferred INTEGER NOT NULL,
            source_page  INTEGER
        );

        CREATE TABLE transaction_line_items (
            qb_id_parent        TEXT NOT NULL,
            line_number         INTEGER NOT NULL,
            item_qb_id          TEXT,
            amount_type         INTEGER NOT NULL,
            amount_cents        INTEGER,
            amount_cents_signed INTEGER,
            amount_raw_hex      TEXT NOT NULL,
            txn_date_raw        INTEGER,
            counter             INTEGER,
            source_table        TEXT NOT NULL,
            page_number         INTEGER NOT NULL,
            page_offset         INTEGER NOT NULL,
            PRIMARY KEY (qb_id_parent, line_number, source_table)
        );

        CREATE INDEX idx_lineitems_page   ON transaction_line_items(page_number);
        CREATE INDEX idx_lineitems_source ON transaction_line_items(source_table);
        CREATE INDEX idx_tx_type          ON transactions(type);
        CREATE INDEX idx_tx_source        ON transactions(source_table);
        "#,
    )?;
    Ok(())
}

fn insert_transaction_headers(
    tx: &Transaction<'_>,
    headers: &[TransactionHeader],
) -> Result<HashMap<String, TransactionHeader>> {
    let mut map: HashMap<String, TransactionHeader> = HashMap::new();
    for h in headers {
        map.entry(h.qb_id.clone()).or_insert_with(|| h.clone());
    }
    let mut stmt = tx.prepare(
        "INSERT INTO transactions \
         (qb_id, type, source_table, txn_date_raw, counter, \
          line_count, total_cents, has_deferred, source_page) \
         VALUES (?, ?, ?, ?, ?, 0, 0, 0, ?)",
    )?;
    let mut ordered: Vec<&TransactionHeader> = map.values().collect();
    ordered.sort_by_key(|h| (h.page_number, h.page_offset));
    for h in ordered {
        stmt.execute(params![
            h.qb_id,
            h.txn_type(),
            h.source_table,
            h.txn_date_raw,
            h.counter,
            h.page_number as i64,
        ])?;
    }
    Ok(map)
}

/// Insert synthesized rows for parent QB-IDs that appear in line items
/// but have no matching header record (or update aggregates for those
/// that do). Synthesized rows get `type='unknown'`.
fn insert_synthesized_transactions(
    tx: &Transaction<'_>,
    items: &[LineItem],
    header_map: &HashMap<String, TransactionHeader>,
) -> Result<()> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<&LineItem>> = HashMap::new();
    for li in items {
        groups
            .entry(li.invoice_id.clone())
            .or_insert_with(|| {
                order.push(li.invoice_id.clone());
                Vec::new()
            })
            .push(li);
    }

    let mut update_stmt = tx.prepare(
        "UPDATE transactions SET line_count=?, total_cents=?, has_deferred=? WHERE qb_id=?",
    )?;
    let mut insert_stmt = tx.prepare(
        "INSERT INTO transactions \
         (qb_id, type, source_table, txn_date_raw, counter, \
          line_count, total_cents, has_deferred, source_page) \
         VALUES (?, 'unknown', ?, ?, ?, ?, ?, ?, ?)",
    )?;
    for qb_id in order {
        let lines = &groups[&qb_id];
        let total: i64 = lines
            .iter()
            .filter_map(|l| l.amount_cents.map(|c| c as i64))
            .sum();
        let has_deferred =
            lines.iter().any(|l| l.amount_type == AmountType::Deferred) as i64;
        let line_count = lines.len() as i64;
        if header_map.contains_key(&qb_id) {
            update_stmt.execute(params![line_count, total, has_deferred, qb_id])?;
            continue;
        }
        let date = lines.iter().find_map(|l| l.txn_date_raw);
        let counter = lines.iter().find_map(|l| l.counter);
        let src = lines
            .iter()
            .find_map(|l| l.source_table.clone())
            .unwrap_or_default();
        let page = lines.first().map(|l| l.page_number as i64).unwrap_or(0);
        insert_stmt.execute(params![
            qb_id, src, date, counter, line_count, total, has_deferred, page
        ])?;
    }
    Ok(())
}

fn insert_transaction_line_items(tx: &Transaction<'_>, items: &[LineItem]) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT OR IGNORE INTO transaction_line_items \
         (qb_id_parent, line_number, item_qb_id, amount_type, amount_cents, \
          amount_cents_signed, amount_raw_hex, txn_date_raw, counter, source_table, \
          page_number, page_offset) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?;
    let mut line_no: BTreeMap<(String, String), i64> = BTreeMap::new();
    for li in items {
        let src = li.source_table.clone().unwrap_or_default();
        let key = (li.invoice_id.clone(), src.clone());
        let n = line_no.entry(key).or_insert(0);
        *n += 1;
        let type_byte = amount_type_byte(li.amount_type);
        let hex = li
            .amount_raw
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        stmt.execute(params![
            li.invoice_id,
            *n,
            li.item_qb_id,
            type_byte,
            li.amount_cents,
            li.amount_cents_signed,
            hex,
            li.txn_date_raw,
            li.counter,
            src,
            li.page_number as i64,
            li.page_offset as i64,
        ])?;
    }
    Ok(())
}

fn amount_type_byte(t: AmountType) -> i64 {
    match t {
        AmountType::None => 0,
        AmountType::OneByteOne => 1,
        AmountType::Standard => 2,
        AmountType::Deferred => 3,
        AmountType::Other(b) => b as i64,
    }
}

#[derive(Debug)]
struct ExportStats {
    pages: u64,
    transactions: i64,
    headers: i64,
    line_items: i64,
    grand_total_cents: i64,
}

impl ExportStats {
    fn summary(&self) -> String {
        format!(
            "openqbw: pages={} transactions={} headers={} lineitems={} grand_total=${:.2}",
            self.pages,
            self.transactions,
            self.headers,
            self.line_items,
            self.grand_total_cents as f64 / 100.0,
        )
    }
}

fn collect_export_stats(
    conn: &Connection,
    store: &PageStore,
    items: &[LineItem],
    headers: &[TransactionHeader],
) -> Result<ExportStats> {
    let txns: i64 = conn.query_row("SELECT COUNT(*) FROM transactions", [], |r| r.get(0))?;
    let total: i64 = conn.query_row(
        "SELECT COALESCE(SUM(total_cents), 0) FROM transactions",
        [],
        |r| r.get(0),
    )?;
    Ok(ExportStats {
        pages: store.page_count(),
        transactions: txns,
        headers: headers.len() as i64,
        line_items: items.len() as i64,
        grand_total_cents: total,
    })
}
