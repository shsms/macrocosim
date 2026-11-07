"""A script that takes a value from `helper.py` beside it."""

import asyncio

from frequenz.quantities import Power
from helper import LOAD_KW

import macrocosim as mc

MICROGRIDS = [
    mc.make_microgrid(
        id=63,
        topology=mc.make_grid_connection_point(
            successors=[mc.make_meter(id=31, power=Power.from_kilowatts(LOAD_KW))]
        ),
    )
]


async def run(microgrids: dict[int, mc.Microgrid]) -> None:
    print("load", LOAD_KW)
    await asyncio.Event().wait()
