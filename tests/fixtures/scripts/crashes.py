"""A script whose `run` raises once its microgrid is in place."""

import macrocosim as mc

MICROGRIDS = [mc.make_microgrid(id=64, topology=mc.make_grid_connection_point())]


async def run(microgrids: dict[int, mc.Microgrid]) -> None:
    raise RuntimeError("the script broke")
