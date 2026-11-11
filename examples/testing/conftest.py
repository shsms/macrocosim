"""A fixture that launches the spec a test hands it, and stops it afterwards."""

import contextlib
import itertools
from collections.abc import AsyncIterator, Awaitable, Callable
from pathlib import Path

import pytest

import macrocosim as mc

LaunchMicrogrid = Callable[[mc.MicrogridSpec], Awaitable[mc.Microgrid]]


@pytest.fixture
async def launch_microgrid(tmp_path: Path) -> AsyncIterator[LaunchMicrogrid]:
    """Return a function that launches a spec; stop every simulator it started
    when the test ends.

    Each simulator's log stays in tmp_path.
    """
    count = itertools.count(1)
    async with contextlib.AsyncExitStack() as stack:

        async def start(spec: mc.MicrogridSpec) -> mc.Microgrid:
            state_dir = tmp_path / f"macrocosim-{next(count)}"
            return await stack.enter_async_context(mc.launch(spec, state_dir=state_dir))

        yield start
