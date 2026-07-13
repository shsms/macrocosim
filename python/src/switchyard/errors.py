"""Public exception types."""

from __future__ import annotations


class SwitchyardError(Exception):
    """Base for errors raised by the switchyard client."""


class SetpointRejected(SwitchyardError):
    """A setpoint or bounds command was rejected by the API gateway.

    Most often the commanded value is outside the component's live
    (augmentation-narrowed) envelope — the same hard error production
    returns. The originating client error is chained as ``__cause__``.
    """


class EvalRejected(SwitchyardError, ValueError):
    """The interpreter rejected a Lisp form sent over ``/api/eval``.

    Carries the server's error text. Subclasses ``ValueError`` so callers
    that caught the historic ``ValueError`` keep working.
    """


class ControlRejected(SwitchyardError, ValueError):
    """The server rejected a typed control request (drive, status, ...).

    Carries the server's error text — e.g. an unknown component id, or a
    stimulus that does not apply to the component's type.
    """


class NoSample(SwitchyardError):
    """A signal produced no value within the read's wait window.

    Raised by ``Signal.read()`` so its return type is the plain quantity;
    use ``Signal.try_read()`` when ``None`` is an expected outcome.
    """
