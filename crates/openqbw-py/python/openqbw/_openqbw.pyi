from collections.abc import Iterator
from os import PathLike
from typing import Any, Generic, Optional, TypeVar, Union, overload

__version__: str

_T = TypeVar("_T")

class Rejected:
    """A record or page a lenient read skipped."""

    kind: str
    page_number: Optional[int]
    page_offset: Optional[int]
    message: str
    def to_dict(self) -> dict[str, Any]: ...

class ReadWarning:
    """A condition that applies to a whole read."""

    kind: str
    message: str
    def to_dict(self) -> dict[str, Any]: ...

class ScanReport(Generic[_T]):
    """Records found, records skipped, and run-level warnings."""

    items: list[_T]
    rejected: list[Rejected]
    warnings: list[ReadWarning]
    def __len__(self) -> int: ...
    def __iter__(self) -> Iterator[_T]: ...
    @overload
    def __getitem__(self, index: int) -> _T: ...
    @overload
    def __getitem__(self, index: slice) -> list[_T]: ...

class LineItem:
    invoice_id: str
    item_qb_id: Optional[str]
    amount_cents: Optional[int]
    amount_encoding: str
    amount_type: int
    amount_raw: bytes
    txn_date_raw: Optional[int]
    counter: Optional[int]
    date_source: str
    txn_date_days_since_unix: Optional[int]
    source_table: Optional[str]
    page_number: int
    page_offset: int
    flags: list[str]
    def to_dict(self) -> dict[str, Any]: ...

class Transaction:
    qb_id: str
    source_table: str
    txn_type: str
    txn_date_raw: Optional[int]
    counter: Optional[int]
    date_source: str
    txn_date_days_since_unix: Optional[int]
    page_number: int
    page_offset: int
    flags: list[str]
    def to_dict(self) -> dict[str, Any]: ...

class Table:
    table_id: int
    name: str
    col_count: Optional[int]
    data_root_page: Optional[int]
    last_page: Optional[int]
    page_number: int
    flags: list[str]
    def to_dict(self) -> dict[str, Any]: ...

class Index:
    name: str
    table_id: int
    root_page: int
    is_foreign_key: bool
    page_number: int
    flags: list[str]
    def to_dict(self) -> dict[str, Any]: ...

class Reader:
    path: str
    lenient: bool
    page_count: int
    file_size: int
    attribution_gap: Optional[str]
    def tables(self) -> ScanReport[Table]: ...
    def indexes(self) -> ScanReport[Index]: ...
    def line_items(self) -> ScanReport[LineItem]: ...
    def transactions(self) -> ScanReport[Transaction]: ...

def open(path: Union[str, PathLike[str]], *, lenient: bool = False) -> Reader: ...
