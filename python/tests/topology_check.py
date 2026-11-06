"""Compare a launched simulator's ``/topology`` with the spec it was built from.

Shared by the live tests. The runtime client does not run this check; the
package's own tests do, to catch renderer bugs before a release.
"""

from __future__ import annotations

from typing import Any

import macrocosim._spec as s

EXPECTED_KIND: dict[type, tuple[str, str | None]] = {
    s.GridConnectionPointSpec: ("grid", None),
    s.MeterSpec: ("meter", None),
    s.BatterySpec: ("battery", None),
    s.BatteryInverterSpec: ("inverter", "battery"),
    s.SolarInverterSpec: ("inverter", "solar"),
    s.ChpSpec: ("chp", None),
}
"""(category, subtype) the server reports for each spec class."""


def topology_problems(spec: s.MicrogridSpec, topology: dict[str, Any]) -> list[str]:
    """Every way the server's topology differs from `spec` (empty: it matches)."""
    problems: list[str] = []
    ids = spec._ids
    reported = {c["id"]: c for c in topology["components"]}
    expected_ids = set(ids.values())
    for extra in sorted(set(reported) - expected_ids):
        problems.append(f"component {extra} is not in the spec")
    for component, cid in ids.items():
        got = reported.get(cid)
        if got is None:
            problems.append(f"{component!r} (id {cid}) is missing")
            continue
        want = EXPECTED_KIND[type(component)]
        if (got["category"], got["subtype"]) != want:
            problems.append(
                f"component {cid}: expected {want}, got "
                f"{(got['category'], got['subtype'])}"
            )
    want_edges = {
        (ids[parent], ids[child])
        for parent in spec._order
        for child in s.children(parent)
    }
    got_edges = {tuple(e) for e in topology["connections"]} | {
        tuple(e) for e in topology["hidden_connections"]
    }
    if want_edges != got_edges:
        problems.append(
            f"edges differ: missing {sorted(want_edges - got_edges)}, "
            f"extra {sorted(got_edges - want_edges)}"
        )
    if topology["graph_status"] is not None:
        problems.append(f"graph_status: {topology['graph_status']}")
    return problems
