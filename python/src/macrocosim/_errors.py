"""The error tree, and the formatting every error message shares.

Internal; imports nothing from the package. Each class takes its finished
message plus structured fields as keyword arguments. The raise sites build the
message with the `fmt_*` helpers here, so every message about the same kind of
thing reads the same way.

The client's own errors are `MacrocosimError` subclasses. A wrong argument type
raises `TypeError`, and a wrong value `ValueError`. When a spec is built, a
value that breaks one of the client's rules (for example an id, a pairing or a
fuse current) raises `SpecError`, a `ValueError` subclass. A NaN or infinite
value, or an int too large for a float in a float field, raises a plain
`ValueError`.
"""

from __future__ import annotations

from collections.abc import Iterable, Sequence
from datetime import timedelta
from pathlib import Path

WRONG_SHAPE = (AttributeError, KeyError, TypeError, ValueError)
"""What reading an answer or file of an unexpected shape raises."""


class MacrocosimError(Exception):
    """Base of the client's own errors; see each subclass. A wrong argument type
    raises `TypeError`, and a wrong value `ValueError` or its subclass
    `SpecError`."""


class SpecError(MacrocosimError, ValueError):
    """A topology spec is invalid; raised while the spec tree is built."""


class LaunchError(MacrocosimError):
    """The simulator could not be started or did not become ready."""

    def __init__(
        self, message: str, *, log_tail: str = "", log_path: Path | None = None
    ) -> None:
        super().__init__(message)
        self.log_tail = log_tail
        self.log_path = log_path


class ServerError(MacrocosimError):
    """The simulator's HTTP API failed or answered something the client did not
    expect (which usually means the client and binary do not match)."""

    def __init__(
        self, message: str, *, method: str, path: str, status: int | None = None
    ) -> None:
        super().__init__(message)
        self.method = method
        self.path = path
        self.status = status


class SimulatorExited(ServerError):
    """The simulator process is gone."""

    def __init__(
        self,
        message: str,
        *,
        method: str,
        path: str,
        returncode: int,
        log_tail: str = "",
        log_path: Path | None = None,
    ) -> None:
        super().__init__(message, method=method, path=path)
        self.returncode = returncode
        self.log_tail = log_tail
        self.log_path = log_path


class Rejected(MacrocosimError):
    """The simulator refused a value the test set, or a fault it asked for."""

    def __init__(self, message: str, *, call: str, text: str) -> None:
        super().__init__(message)
        self.call = call
        self.text = text


class UnknownComponent(MacrocosimError, LookupError):
    """The key names no component of this microgrid."""

    def __init__(self, message: str, *, key: object, valid_ids: Sequence[int]) -> None:
        super().__init__(message)
        self.key = key
        self.valid_ids = tuple(valid_ids)


class NoSample(MacrocosimError):
    """No usable sample arrived: none in time, a stall, a gap, a null, or a
    signal the microgrid does not publish at all."""

    def __init__(self, message: str, *, signal: str, log_path: Path | None) -> None:
        super().__init__(message)
        self.signal = signal
        self.log_path = log_path


class ExpectationFailed(MacrocosimError, AssertionError):
    """The samples arrived, and disagreed with the expectation."""

    def __init__(self, message: str, *, signal: str, log_path: Path | None) -> None:
        super().__init__(message)
        self.signal = signal
        self.log_path = log_path


class MicrogridClosed(MacrocosimError):
    """The microgrid was used after its launch block exited."""

    def __init__(self, message: str, *, microgrid_id: int) -> None:
        super().__init__(message)
        self.microgrid_id = microgrid_id


def fmt_duration(value: timedelta | float) -> str:
    """Seconds, at most one decimal, trailing ``.0`` dropped: ``60 s``, ``2.5
    s``."""
    seconds = value.total_seconds() if isinstance(value, timedelta) else float(value)
    text = f"{round(seconds, 1):.1f}"
    if text.endswith(".0"):
        text = text[:-2]
    if text == "-0":
        text = "0"
    return f"{text} s"


def fmt_at(offset: timedelta | float) -> str:
    """A time inside a hold, relative to the first match: ``T0+7 s``."""
    return f"T0+{fmt_duration(offset)}"


def fmt_value(value: object | None) -> str:
    """A sample: ``str()`` of the quantity, or ``null``."""
    return "null" if value is None else str(value)


def fmt_last(values: Sequence[object | None], *, limit: int = 5) -> str:
    """The newest samples, as the ``{last}`` part of a message (may be empty)."""
    newest = list(values)[-limit:]
    if not newest:
        return ""
    joined = ", ".join(fmt_value(v) for v in newest)
    return f"\n  Last {len(newest)} samples (newest last): {joined}"


def fmt_log(log_path: Path | None) -> str:
    """The ``{log}`` part: where the log is, or how to keep it."""
    if log_path is None:
        return "\n  Pass state_dir to launch to keep the simulator log."
    return f"\n  Simulator log: {log_path}"


def fmt_tail(log_tail: str) -> str:
    """The ``{tail}`` part: the end of the simulator log, on its own lines."""
    return f"\n{log_tail}" if log_tail else ""


def fmt_ids(ids: Iterable[int]) -> str:
    """Component ids, ascending and comma-separated."""
    return ", ".join(str(i) for i in sorted(ids))
