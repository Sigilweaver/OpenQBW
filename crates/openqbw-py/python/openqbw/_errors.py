"""Exceptions and warnings raised by openqbw."""


class OpenQBWError(ValueError):
    """Base class for every error openqbw raises.

    Every instance carries ``kind`` (a stable snake_case code such as
    ``"bad_magic"``), ``page_number`` and ``page_offset`` (``None`` when the
    error is not about one page or byte).
    """

    kind = None
    page_number = None
    page_offset = None


class OpenQBWIOError(OpenQBWError, OSError):
    """The file could not be read. Also an ``OSError``, with ``errno``,
    ``strerror`` and ``filename`` when the operating system supplied them."""

    def __init__(self, *args):
        OSError.__init__(self, *args)


class FormatError(OpenQBWError):
    """The file is not a supported QuickBooks company file, a page is
    damaged, or its keystream cannot be calibrated."""


class RecordError(FormatError):
    """A record could not be decoded with confidence. Open with
    ``lenient=True`` to skip such records and list them in ``rejected``."""


class ContentError(FormatError):
    """The file content is not readable (for example password protected)."""


class OpenQBWWarning(UserWarning):
    """Issued for records a lenient read skipped and for conditions that
    apply to a whole read. The same facts are on the result object's
    ``rejected`` and ``warnings`` attributes."""
