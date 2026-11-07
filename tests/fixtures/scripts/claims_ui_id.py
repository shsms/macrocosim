"""A script that claims microgrid 70, which the test made in the UI."""

import asyncio

import macrocosim as mc

MICROGRIDS = [
    mc.make_microgrid(id=61, topology=mc.make_grid_connection_point()),
    mc.make_microgrid(id=70, topology=mc.make_grid_connection_point()),
]


async def run(microgrids: dict[int, mc.Microgrid]) -> None:
    await asyncio.Event().wait()
