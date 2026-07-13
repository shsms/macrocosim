"""The async-native core of the switchyard client (v2).

Everything here runs on the caller's event loop: reads, writes, waits,
and assertions are coroutines, with no background loop thread and no
worker threads. See ``docs/python-api-redesign.org``.

Typical use::

    import switchyard as sw

    load = sw.meter(id=5, power=Power.zero())  # ... build the topology
    async with sw.aio.launch(mg) as site:      # binds the builders
        await load.power.set(Power.from_kilowatts(20))
        await site.grid_power.expect(sw.at_most(Power.from_kilowatts(13)))
"""

from ._site import ComponentHandle, ScenarioRun, Site, connect, launch

__all__ = [
    "ComponentHandle",
    "ScenarioRun",
    "Site",
    "connect",
    "launch",
]
