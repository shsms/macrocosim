"""Suite-wide test configuration: the ``live`` marker.

A test marked ``live`` boots a real simulator. When no ``macrocosim`` binary can
be found (``MACROCOSIM_BIN``, the wheel's scripts directory, or PATH), such
tests are skipped, or, with ``MACROCOSIM_REQUIRE_LIVE=1`` (as CI sets),
collection stops so a missing binary cannot pass as a green run.
"""

from __future__ import annotations

import os

import pytest

from macrocosim._process import which_binary


def _binary() -> str | None:
    return os.environ.get("MACROCOSIM_BIN") or which_binary("macrocosim")


def pytest_configure(config: pytest.Config) -> None:
    config.addinivalue_line(
        "markers", "live: boots a real macrocosim simulator (needs the binary)"
    )


def pytest_collection_modifyitems(
    config: pytest.Config, items: list[pytest.Item]
) -> None:
    live = [item for item in items if item.get_closest_marker("live")]
    if not live or _binary():
        return
    if os.environ.get("MACROCOSIM_REQUIRE_LIVE") == "1":
        raise pytest.UsageError(
            "MACROCOSIM_REQUIRE_LIVE=1 but no macrocosim binary was found "
            "(MACROCOSIM_BIN, the wheel's scripts directory, or PATH)"
        )
    skip = pytest.mark.skip(reason="no macrocosim binary available")
    for item in live:
        item.add_marker(skip)
