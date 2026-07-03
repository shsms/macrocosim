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
