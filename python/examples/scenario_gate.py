"""Y6: run a registered Lisp scenario and gate on its checks.

Loads a config that both wires a topology and registers a scenario via
``(define-scenario …)``, runs it to completion, and fails if any
``(check …)`` failed — the CI gate.

    SWITCHYARD_BIN=../target/debug/switchyard python examples/scenario_gate.py
"""

from __future__ import annotations

from pathlib import Path

import switchyard as sw

CONFIG = Path(__file__).with_name("scenario.lisp")


def main() -> None:
    with sw.launch(CONFIG) as site:
        registered = [s["name"] for s in site._http.get_json("/api/scenarios")]
        print(f"registered scenarios: {registered}")

        report = site.scenario("hold-load").run(wait=True).assert_passed()
        total = report["checks_passed"] + report["checks_failed"]
        print(f"OK  hold-load: {report['checks_passed']}/{total} checks passed")


if __name__ == "__main__":
    main()
