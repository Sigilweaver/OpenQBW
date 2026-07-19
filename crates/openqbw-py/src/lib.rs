//! Python bindings for the `openqbw` crate.
//!
//! Exposes a small read-only API for opening a QBW file and iterating
//! over its catalog, line items, transaction headers, and indexes.

#![allow(clippy::useless_conversion)] // pyo3 macro expansions can trigger this on newer clippy

use openqbw_rs::{
    collect_unique as collect_unique_systable, collect_unique_sysindex,
    iter_lineitems_with_attribution, iter_transaction_headers, PageAttribution, SysIndexEntry,
    SysTableEntry, TransactionHeader,
};
use opensqlany::{ApModel, PageStore};
use pyo3::exceptions::PyIOError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::path::PathBuf;
use std::sync::Arc;

/// A QBW reader. Construct with `openqbw.open(path)`.
#[pyclass(module = "openqbw", frozen)]
struct Reader {
    path: PathBuf,
    store: Arc<PageStore>,
    model: Arc<ApModel>,
    attribution: Arc<PageAttribution>,
}

#[pymethods]
impl Reader {
    /// The filesystem path the reader was opened from.
    #[getter]
    fn path(&self) -> String {
        self.path.display().to_string()
    }

    /// Number of pages in the underlying page-store.
    #[getter]
    fn page_count(&self) -> u64 {
        self.store.page_count()
    }

    /// File size in bytes.
    #[getter]
    fn file_size(&self) -> u64 {
        std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0)
    }

    fn __repr__(&self) -> String {
        format!(
            "<openqbw.Reader path={:?} pages={}>",
            self.path,
            self.store.page_count()
        )
    }

    /// Return the SYSTABLE catalog as a list of dicts.
    fn tables<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let entries: Vec<SysTableEntry> = collect_unique_systable(&self.store, &self.model);
        let mut out = Vec::with_capacity(entries.len());
        for e in entries {
            let d = PyDict::new(py);
            d.set_item("table_id", e.table_id)?;
            d.set_item("name", e.name)?;
            d.set_item("col_count", e.col_count)?;
            d.set_item("data_root_page", e.data_root_page)?;
            d.set_item("last_page", e.last_page)?;
            d.set_item("page_number", e.page_number)?;
            out.push(d);
        }
        Ok(out)
    }

    /// Return SYSINDEX entries as a list of dicts.
    fn indexes<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let entries: Vec<SysIndexEntry> = collect_unique_sysindex(&self.store, &self.model);
        let mut out = Vec::with_capacity(entries.len());
        for e in entries {
            let d = PyDict::new(py);
            d.set_item("name", e.name)?;
            d.set_item("table_id", e.table_id)?;
            d.set_item("root_page", e.root_page)?;
            d.set_item("page_number", e.page_number)?;
            out.push(d);
        }
        Ok(out)
    }

    /// Return all line items as a list of dicts. Each dict carries the
    /// parent `invoice_id`, decoded amount fields, the source-table name,
    /// and the page coordinates.
    fn line_items<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let iter = iter_lineitems_with_attribution(&self.store, &self.model, &self.attribution);
        let mut out = Vec::new();
        for li in iter {
            let d = PyDict::new(py);
            let date = li.txn_date_days_since_unix();
            d.set_item("invoice_id", li.invoice_id)?;
            d.set_item("item_qb_id", li.item_qb_id)?;
            d.set_item("amount_cents", li.amount_cents)?;
            d.set_item("amount_cents_signed", li.amount_cents_signed)?;
            d.set_item("txn_date_days_since_unix", date)?;
            d.set_item("source_table", li.source_table)?;
            d.set_item("page_number", li.page_number)?;
            d.set_item("page_offset", li.page_offset)?;
            out.push(d);
        }
        Ok(out)
    }

    /// Return all transaction headers as a list of dicts.
    fn transactions<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let iter: Vec<TransactionHeader> =
            iter_transaction_headers(&self.store, &self.model, &self.attribution).collect();
        let mut out = Vec::with_capacity(iter.len());
        for h in iter {
            let d = PyDict::new(py);
            d.set_item("qb_id", &h.qb_id)?;
            d.set_item("source_table", &h.source_table)?;
            d.set_item("txn_type", h.txn_type().to_string())?;
            d.set_item("page_number", h.page_number)?;
            d.set_item("page_offset", h.page_offset)?;
            out.push(d);
        }
        Ok(out)
    }
}

/// Open a QBW file and return a `Reader`.
#[pyfunction]
fn open(path: PathBuf) -> PyResult<Reader> {
    let store = PageStore::open(&path)
        .map_err(|e| PyIOError::new_err(format!("opening {:?}: {e}", path)))?;
    let model = ApModel::learn(&store);
    let attribution = PageAttribution::build(&store, &model);
    Ok(Reader {
        path,
        store: Arc::new(store),
        model: Arc::new(model),
        attribution: Arc::new(attribution),
    })
}

/// The `openqbw` Python extension module.
#[pymodule]
fn openqbw(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_class::<Reader>()?;
    m.add_function(wrap_pyfunction!(open, m)?)?;
    Ok(())
}
