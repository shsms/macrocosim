"""Y6: the deterministic, serverless gate (no app under test).

Runs a scenario headless on the stepped clock — bit-reproducible with a
seed, faster than real time. Needs swctl:

    SWCTL_BIN=../target/debug/swctl python examples/stepped_gate.py
"""

from __future__ import annotations

from datetime import timedelta

from frequenz.quantities import Power

import switchyard as sw

TOPOLOGY = sw.Microgrid(
    id=1,
    topology=sw.grid(id=1, successors=[sw.meter(id=2, power=Power.from_watts(5000))]),
)

SCENARIO = sw.Scenario("hold-load", length=timedelta(seconds=3), seed=1).check(
    timedelta(seconds=1),
    component=2,
    metric=sw.Metric.ACTIVE_POWER,
    approx=Power.from_watts(5000),
    tol=Power.from_watts(500),
)


def main() -> None:
    # Builder objects render to one temp config; a .lisp path works too.
    report = sw.run_scenario_stepped([TOPOLOGY, SCENARIO], "hold-load")
    total = report["checks_passed"] + report["checks_failed"]
    print(
        f"OK  stepped gate: {report['checks_passed']}/{total} checks passed "
        f"(elapsed {report['scenario_elapsed_s']}s, deterministic)"
    )


if __name__ == "__main__":
    main()
