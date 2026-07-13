"""The async-native core of the switchyard client (v2).

Everything here runs on the caller's event loop: reads, writes, waits,
and assertions are coroutines, with no background loop thread and no
worker threads. See ``docs/python-api-redesign.org``.

Typical use::

    import switchyard as sw
    from switchyard.metrics import GRID_POWER, BATTERY_ENERGY

    async with sw.aio.launch(topology) as site:
        await site[5].drive(power=Power.from_kilowatts(20))
        await site.expect(GRID_POWER, max=Power.from_kilowatts(13))
"""

from ._site import ComponentHandle, ScenarioRun, Site, connect, launch

__all__ = [
    "ComponentHandle",
    "ScenarioRun",
    "Site",
    "connect",
    "launch",
]
