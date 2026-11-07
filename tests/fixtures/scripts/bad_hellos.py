"""While it loads, this script sends the simulator two hellos of its own: one
with a wrong token, then one of `KIND` with the child's token. It writes the
status of each answer to `answers.json` in the directory above its own, outside
the tree the simulator watches. Its own hello then finds the child's turn used,
so the script fails to load. The runner tests edit `KIND` in a copy of this
file."""

import importlib.metadata
import json
import os
import pathlib
import urllib.error
import urllib.request

import macrocosim as mc

KIND = "version"


def _post(body: dict) -> int:
    request = urllib.request.Request(
        os.environ["MACROCOSIM_URL"] + "/api/runner/hello",
        data=json.dumps(body).encode(),
        headers={"content-type": "application/json"},
    )
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(request, timeout=30) as response:
            return response.status
    except urllib.error.HTTPError as e:
        return e.code


VERSION = importlib.metadata.version("macrocosim")
TOKEN = os.environ["MACROCOSIM_RUNNER_TOKEN"]
GOOD = {"schema_version": 1, "microgrids": [{"id": 61, "components": []}]}
NO_ID = {
    "schema_version": 1,
    "microgrids": [{"id": 61, "components": [{"type": "grid_connection_point"}]}],
}

answers = {
    "wrong_token": _post({"token": "nope", "client_version": VERSION, "topology": GOOD})
}
if KIND == "version":
    answers["bad"] = _post({"token": TOKEN, "client_version": "0.0.0", "topology": GOOD})
else:
    answers["bad"] = _post({"token": TOKEN, "client_version": VERSION, "topology": NO_ID})
(pathlib.Path(__file__).parent.parent / "answers.json").write_text(json.dumps(answers))

MICROGRIDS = [mc.make_microgrid(id=61, topology=mc.make_grid_connection_point())]
