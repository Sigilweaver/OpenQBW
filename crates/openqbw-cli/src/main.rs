//! `openqbw` command-line tool: reads a `.qbw` file and produces either
//! an invoice line-item SQLite export (`export`) or a system-catalog
//! listing (`catalog`).
//!
//! ```text
//! openqbw export  <input.qbw> <output.db>
//! openqbw catalog <input.qbw>
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use openqbw::{collect_unique, iter_lineitems, AmountType, LineItem, SysTableEntry};
use opensqlany::{ApModel, PageStore};
use rusqlite::{params, Connection};

#[derive(Parser, Debug)]
#[command(name = "openqbw", version, about = "QuickBooks .qbw inspector and exporter")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Export invoice line items and synthesized headers to SQLite.
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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Export { input, output } => run_export(input, output),
        Cmd::Catalog { input, user_only } => run_catalog(input, user_only),
    }
}

fn run_export(input: PathBuf, output: PathBuf) -> Result<()> {
    if output.exists() {
        std::fs::remove_file(&output)
            .with_context(|| format!("removing existing {:?}", output))?;
    }

    let store = PageStore::open(&input)
        .with_context(|| format!("opening {:?}", input))?;
    let model = ApModel::learn(&store);

    let mut items: Vec<LineItem> = iter_lineitems(&store, &model).collect();
    // Stable order: (page_number, page_offset).
    items.sort_by_key(|li| (li.page_number, li.page_offset));

    let mut conn = Connection::open(&output)
        .with_context(|| format!("opening {:?}", output))?;
    create_schema(&conn)?;

    let tx = conn.transaction()?;
    insert_invoices(&tx, &items)?;
    insert_lineitems(&tx, &items)?;
    tx.commit()?;

    let n_li = items.len();
    let n_inv: i64 = conn.query_row("SELECT COUNT(*) FROM invoices", [], |r| r.get(0))?;
    let total: i64 = conn
        .query_row("SELECT COALESCE(SUM(total_cents), 0) FROM invoices", [], |r| {
            r.get(0)
        })?;
    println!(
        "openqbw: pages={} invoices={} lineitems={} total=${:.2}",
        store.page_count(),
        n_inv,
        n_li,
        total as f64 / 100.0,
    );
    Ok(())
}

fn run_catalog(input: PathBuf, user_only: bool) -> Result<()> {
    let store = PageStore::open(&input)
        .with_context(|| format!("opening {:?}", input))?;
    let model = ApModel::learn(&store);

    let mut entries: Vec<SysTableEntry> = collect_unique(&store, &model);
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

fn is_system_name(name: &str) -> bool {
    name.starts_with("SYS")
        || name.starts_with("ISYS")
        || name.starts_with("RS_")
        || name.starts_with("dbo")
}

fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE invoices (
            invoice_id        TEXT PRIMARY KEY,
            txn_date_raw      INTEGER,
            counter           INTEGER,
            line_count        INTEGER NOT NULL,
            total_cents       INTEGER NOT NULL,
            has_deferred      INTEGER NOT NULL
        );

        CREATE TABLE invoice_line_items (
            invoice_id        TEXT NOT NULL,
            line_number       INTEGER NOT NULL,
            item_qb_id        TEXT,
            amount_type       INTEGER NOT NULL,
            amount_cents      INTEGER,
            amount_raw_hex    TEXT NOT NULL,
            txn_date_raw      INTEGER,
            counter           INTEGER,
            page_number       INTEGER NOT NULL,
            page_offset       INTEGER NOT NULL,
            PRIMARY KEY (invoice_id, line_number),
            FOREIGN KEY (invoice_id) REFERENCES invoices(invoice_id)
        );

        CREATE INDEX idx_lineitems_page ON invoice_line_items(page_number);
        "#,
    )?;
    Ok(())
}

fn insert_lineitems(tx: &rusqlite::Transaction, items: &[LineItem]) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO invoice_line_items \
         (invoice_id, line_number, item_qb_id, amount_type, amount_cents, \
          amount_raw_hex, txn_date_raw, counter, page_number, page_offset) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?;

    let mut line_no: BTreeMap<&str, i64> = BTreeMap::new();
    for li in items {
        let n = line_no.entry(&li.invoice_id).or_insert(0);
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
            hex,
            li.txn_date_raw,
            li.counter,
            li.page_number as i64,
            li.page_offset as i64,
        ])?;
    }
    Ok(())
}

fn insert_invoices(tx: &rusqlite::Transaction, items: &[LineItem]) -> Result<()> {
    // Group by invoice_id, preserving order of first sighting.
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Vec<&LineItem>> =
        std::collections::HashMap::new();
    for li in items {
        groups
            .entry(li.invoice_id.clone())
            .or_insert_with(|| {
                order.push(li.invoice_id.clone());
                Vec::new()
            })
            .push(li);
    }

    let mut stmt = tx.prepare(
        "INSERT INTO invoices \
         (invoice_id, txn_date_raw, counter, line_count, total_cents, has_deferred) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )?;
    for invoice_id in order {
        let lines = &groups[&invoice_id];
        let total: i64 = lines.iter().filter_map(|l| l.amount_cents.map(|c| c as i64)).sum();
        let has_deferred = lines.iter().any(|l| l.amount_type == AmountType::Deferred);
        let date = lines.iter().find_map(|l| l.txn_date_raw);
        let counter = lines.iter().find_map(|l| l.counter);
        stmt.execute(params![
            invoice_id,
            date,
            counter,
            lines.len() as i64,
            total,
            has_deferred as i64,
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
