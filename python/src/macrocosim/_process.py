"""Shared process plumbing: binary discovery, spawn, the endpoints wait.

Internal. The 0.1 clients (:mod:`macrocosim.runtime`, :mod:`macrocosim.aio`)
use `spawn_macrocosim`; the 0.2 client uses `spawn` and `wait_endpoints`.
Both stop the process with `terminate`.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
import shutil
import subprocess
import sysconfig
import tempfile
import time
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, cast

if TYPE_CHECKING:
    from .build import ConfigSource, LispRenderable

from ._errors import WRONG_SHAPE


def which_binary(name: str) -> str | None:
    """Find ``name`` in this interpreter's scripts directory, else on PATH.

    A platform wheel bundles the binaries there (``<venv>/bin``), which is
    reachable even when that directory isn't on PATH (unactivated venv, some
    test runners). It comes first so that an older build elsewhere on PATH
    (``~/.cargo/bin``, say) never shadows the binary the wheel shipped with;
    ``shutil.which`` also resolves the ``.exe`` on Windows.
    """
    return shutil.which(name, path=sysconfig.get_path("scripts")) or shutil.which(name)


def resolve_binary(
    name: str,
    *,
    env_var: str,
    explicit: str | os.PathLike[str] | None,
    flag: str | None,
) -> str:
    """Locate a bundled binary: an explicit path → ``$env_var`` → wheel / PATH.

    ``flag`` names the launch/run keyword that overrides it, for the not-found
    message (``bin`` for macrocosim, ``macroctl_bin`` for macroctl); ``None``
    when there is none.
    """
    for candidate in (explicit, os.environ.get(env_var)):
        if candidate:
            path = Path(candidate)
            if path.is_file():
                return str(path)
            raise FileNotFoundError(f"{name} binary not found at {path}")
    found = which_binary(name)
    if found:
        return found
    pass_flag = f"pass {flag}=..., " if flag else ""
    raise FileNotFoundError(
        f"no {name} binary; install the macrocosim platform wheel, {pass_flag}"
        f"set {env_var}, or put it on PATH"
    )


def render_config(config: ConfigSource, tmpdir: Path) -> Path:
    """A config file path as-is, or render to_lisp object(s) to a temp file."""
    if hasattr(config, "to_lisp"):
        # A run-time check that needs no import of `build`; the checker does
        # not narrow on hasattr, hence the cast.
        forms: list[LispRenderable] = [cast("LispRenderable", config)]
    elif isinstance(config, (str, os.PathLike)):
        # config is str | PathLike here; the structural narrowing leaves a
        # spurious Protocol-and-PathLike intersection the checker can't shed.
        return Path(config)  # ty: ignore[invalid-argument-type]
    else:
        forms = list(config)
    path = tmpdir / "config.lisp"
    path.write_text("\n".join(f.to_lisp() for f in forms) + "\n")
    return path


@dataclass
class SpawnedMacrocosim:
    """A freshly spawned simulator, before the endpoint handshake."""

    process: subprocess.Popen[bytes]
    """The simulator process."""

    endpoints_file: Path
    """Written by the process once its servers are up (the ready signal)."""

    log_file: Path
    """The process's combined stdout+stderr, for failure diagnostics."""

    tmpdir: Path
    """Holds the rendered config + endpoints + log. Removed by
    ``Site.close()`` on the happy path; deliberately kept when a launch
    fails so the log survives for post-mortem reading."""

    def fail(self, exc_type: type[Exception], message: str) -> None:
        """Stop the process and raise ``exc_type`` with the log tail attached."""
        terminate(self.process)
        tail = self.log_file.read_text(errors="replace")[-4000:]
        raise exc_type(f"{message}\n{tail}")


def spawn_macrocosim(
    config: ConfigSource, bin: str | os.PathLike[str] | None
) -> SpawnedMacrocosim:
    """Spawn the simulator on ephemeral ports; the caller awaits the handshake.

    Sends the child's output to a file, not a PIPE: an undrained pipe fills
    its ~64KB buffer and the child blocks on write before it can emit the
    endpoints handshake. The file also carries diagnostics for failures.
    """
    binary = resolve_binary(
        "macrocosim", env_var="MACROCOSIM_BIN", explicit=bin, flag="bin"
    )
    tmpdir = Path(tempfile.mkdtemp(prefix="macrocosim-py-"))
    config_path = render_config(config, tmpdir)
    # Rendered configs (builder objects) anchor their persistent state
    # (overrides journals, snapshots, created-microgrid stubs) in the
    # tempdir — a test's structural evals must never litter the
    # process cwd, and the dir is cleaned up with the Site. A
    # user-supplied .lisp path keeps its long-standing contract
    # instead: relative (load …)s and journals anchor next to the
    # config file, as when the binary derived the anchor from the
    # config's own location.
    if isinstance(config, (str, os.PathLike)):
        state_dir = config_path.parent
    else:
        state_dir = tmpdir
    endpoints_file = tmpdir / "endpoints.json"
    log_file = tmpdir / "macrocosim.log"
    with log_file.open("wb") as log:
        process = subprocess.Popen(
            [
                binary,
                str(config_path),
                "--ephemeral-ports",
                f"--emit-endpoints={endpoints_file}",
                f"--state-dir={state_dir}",
            ],
            stdout=log,
            stderr=subprocess.STDOUT,
        )
    return SpawnedMacrocosim(
        process=process,
        endpoints_file=endpoints_file,
        log_file=log_file,
        tmpdir=tmpdir,
    )


def terminate(process: subprocess.Popen[bytes] | None) -> None:
    """Stop a child process, escalating to kill if terminate doesn't take."""
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=5.0)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5.0)


LOG_TAIL_CHARS = 4000
"""How much of the end of the simulator log an error carries."""

LOG_NAME = "macrocosim.log"
ENDPOINTS_NAME = "endpoints.json"


@dataclass
class _Handle:
    """A simulator spawned by `spawn`, and the directory it runs in."""

    process: subprocess.Popen[bytes]
    directory: Path
    temporary: bool
    """Whether `directory` was made by `spawn` (and is deleted after use)."""

    @property
    def log_file(self) -> Path:
        return self.directory / LOG_NAME

    @property
    def kept_log(self) -> Path | None:
        """The log file when the directory is kept, else ``None``."""
        return None if self.temporary else self.log_file

    @property
    def endpoints_file(self) -> Path:
        return self.directory / ENDPOINTS_NAME


def _prepare_directory(state_dir: Path | None) -> tuple[Path, bool]:
    """The directory to run in, and whether it is a new temporary one."""
    if state_dir is None:
        return Path(tempfile.mkdtemp(prefix="macrocosim-")), True
    state_dir = Path(state_dir)
    if state_dir.exists():
        if not state_dir.is_dir() or any(state_dir.iterdir()):
            raise ValueError(
                f"state_dir must be absent or an empty directory: {state_dir}"
            )
    else:
        state_dir.mkdir(parents=True)
    return state_dir, False


def spawn(
    binary: str,
    text: str,
    *,
    env: Mapping[str, str] | None,
    state_dir: Path | None,
) -> _Handle:
    """Write `text` as the boot config and start `binary` on ephemeral ports.

    Runs in `state_dir` (which must be absent or empty, and is kept) or in a new
    temporary directory. The child's output goes to a file, never a pipe: an
    undrained pipe fills and blocks the child before it writes its endpoints. On
    any failure the temporary directory is deleted.
    """
    directory, temporary = _prepare_directory(state_dir)
    try:
        config = directory / "config.lisp"
        config.write_text(text, encoding="utf-8")
        argv = [
            binary,
            "--ephemeral-ports",
            f"--emit-endpoints={directory / ENDPOINTS_NAME}",
            f"--state-dir={directory}",
            str(config),
        ]
        child_env = None if env is None else {**os.environ, **env}
        with (directory / LOG_NAME).open("wb") as log:
            process = subprocess.Popen(
                argv, stdout=log, stderr=subprocess.STDOUT, env=child_env
            )
    except BaseException:
        if temporary:
            shutil.rmtree(directory, ignore_errors=True)
        raise
    return _Handle(process=process, directory=directory, temporary=temporary)


@dataclass(frozen=True)
class Endpoints:
    """Where a launched simulator listens."""

    ui: str
    """``host:port`` of the UI HTTP server."""

    microgrid_id: int
    """The one microgrid's id."""

    grpc_addr: str
    """``host:port`` of that microgrid's gRPC server."""


class _ExitedEarly(Exception):
    def __init__(self, returncode: int) -> None:
        super().__init__(returncode)
        self.returncode = returncode


class _EndpointsTimeout(Exception):
    pass


class _MalformedEndpoints(Exception):
    """The endpoints file is not in the form this client reads."""


class _NotOneMicrogrid(Exception):
    def __init__(self, count: int) -> None:
        super().__init__(count)
        self.count = count


async def wait_endpoints(handle: _Handle, *, deadline: float) -> Endpoints:
    """Wait for the endpoints file, until the monotonic `deadline`.

    Never stops the process; the caller does.
    """
    path = handle.endpoints_file
    while True:
        # The binary writes the file with a rename, so a non-empty file is
        # complete.
        if path.exists() and path.stat().st_size > 0:
            return _read_endpoints(path)
        returncode = handle.process.poll()
        if returncode is not None:
            raise _ExitedEarly(returncode)
        if time.monotonic() >= deadline:
            raise _EndpointsTimeout()
        await asyncio.sleep(0.1)


def _read_endpoints(path: Path) -> Endpoints:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
        microgrids = data.get("microgrids") or []
        if len(microgrids) != 1:
            raise _NotOneMicrogrid(len(microgrids))
        [mg] = microgrids
        return Endpoints(
            ui=str(data["ui"]),
            microgrid_id=int(mg["id"]),
            grpc_addr=str(mg["grpc_addr"]),
        )
    except WRONG_SHAPE as exc:
        raise _MalformedEndpoints(repr(exc)) from exc


def log_tail(handle: _Handle) -> str:
    """The end of the simulator log, or ``""`` when there is none."""
    try:
        with handle.log_file.open("rb") as log:
            # Up to 4 bytes per character in UTF-8.
            log.seek(max(0, log.seek(0, os.SEEK_END) - 4 * LOG_TAIL_CHARS))
            return log.read().decode(errors="replace")[-LOG_TAIL_CHARS:]
    except FileNotFoundError:
        return ""
