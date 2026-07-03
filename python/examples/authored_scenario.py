"""Y6: author both the topology AND the scenario in Python — no .lisp file.

Build a topology, author a scenario with a timed check, register it on the
running site, run it, and gate on the check.

    SWITCHYARD_BIN=../target/debug/switchyard python examples/authored_scenario.py
"""

from __future__ import annotations

from datetime import timedelta

from frequenz.quantities import Power

import switchyard as sw

TOPOLOGY = sw.Microgrid(
    id=1,
    topology=sw.grid(id=1, successors=[sw.meter(id=2, power=Power.from_watts(5000))]),
)

SCENARIO = sw.Scenario(
    "hold-load", schedule=sw.Schedule.RELATIVE, length=timedelta(seconds=3)
).check(
    timedelta(seconds=1),
    component=2,
    metric=sw.Metric.ACTIVE_POWER,
    approx=Power.from_watts(5000),
    tol=Power.from_watts(500),
)


def main() -> None:
    with sw.launch(TOPOLOGY) as site:
        report = site.define_scenario(SCENARIO).run(wait=True).assert_passed()
        total = report["checks_passed"] + report["checks_failed"]
        print(f"OK  authored scenario passed: {report['checks_passed']}/{total} checks")


if __name__ == "__main__":
    main()
