"""Y6: author both the topology AND the scenario in Python — no .lisp file.

Build a topology, author a scenario with a timed check, register it on the
running site, run it, and gate on the check.

    MACROCOSIM_BIN=../target/debug/macrocosim python examples/authored_scenario.py
"""

from __future__ import annotations

from datetime import timedelta

from frequenz.quantities import Power

import macrocosim as mc

MAIN_METER = mc.meter(id=2, power=Power.from_watts(5000))
TOPOLOGY = mc.Microgrid(id=1, topology=mc.grid(id=1, successors=[MAIN_METER]))

SCENARIO = mc.Scenario(
    "hold-load", schedule=mc.Schedule.RELATIVE, length=timedelta(seconds=3)
).check(
    timedelta(seconds=1),
    MAIN_METER.power,
    mc.near(Power.from_watts(5000), tol=Power.from_watts(500)),
)


def main() -> None:
    with mc.launch(TOPOLOGY) as site:
        report = site.define_scenario(SCENARIO).run(wait=True).assert_passed()
        total = report["checks_passed"] + report["checks_failed"]
        print(f"OK  authored scenario passed: {report['checks_passed']}/{total} checks")


if __name__ == "__main__":
    main()
