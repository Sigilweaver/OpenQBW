"""Read QuickBooks company files (.qbw).

>>> import openqbw
>>> reader = openqbw.open("mybooks.qbw")           # doctest: +SKIP
>>> lines = reader.line_items()                      # doctest: +SKIP
>>> lines.rejected, lines.warnings                   # doctest: +SKIP
"""

from ._errors import (
    ContentError,
    FormatError,
    OpenQBWError,
    OpenQBWIOError,
    OpenQBWWarning,
    RecordError,
)
from ._openqbw import (
    Index,
    LineItem,
    open,
    Reader,
    ReadWarning,
    Rejected,
    ScanReport,
    Table,
    Transaction,
    __version__,
)

__all__ = [
    "ContentError",
    "FormatError",
    "Index",
    "LineItem",
    "OpenQBWError",
    "OpenQBWIOError",
    "OpenQBWWarning",
    "ReadWarning",
    "Reader",
    "RecordError",
    "Rejected",
    "ScanReport",
    "Table",
    "Transaction",
    "__version__",
    "open",
]
