"""Shared process plumbing: binary discovery, config rendering, spawn.

Internal. Both the sync client (:mod:`macrocosim.runtime`) and the async
core (:mod:`macrocosim.aio`) launch the same simulator process; only the
endpoint-handshake *wait* differs (blocking vs awaiting), so it stays
with each flavor.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sysconfig
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from .build import ConfigSource

from .build import LispRenderable


def which_binary(name: str) -> str | None:
    """Find ``name`` on PATH, or in this interpreter's scripts directory.

    A platform wheel bundles the binaries there (``<venv>/bin``), which is
    reachable even when that directory isn't on PATH (unactivated venv, some
    test runners); ``shutil.which`` also resolves the ``.exe`` on Windows.
    """
    return shutil.which(name) or shutil.which(name, path=sysconfig.get_path("scripts"))


def resolve_binary(
    name: str,
    *,
    env_var: str,
    explicit: str | os.PathLike[str] | None,
    flag: str,
) -> str:
    """Locate a bundled binary: an explicit path → ``$env_var`` → PATH / wheel.

    ``flag`` names the launch/run keyword that overrides it, for the not-found
    message (``bin`` for macrocosim, ``macroctl_bin`` for macroctl).
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
    raise FileNotFoundError(
        f"no {name} binary; install the macrocosim platform wheel, pass "
        f"{flag}=..., set {env_var}, or put it on PATH"
    )


def render_config(config: ConfigSource, tmpdir: Path) -> Path:
    """A config file path as-is, or render to_lisp object(s) to a temp file."""
    if isinstance(config, LispRenderable):
        forms: list[LispRenderable] = [config]
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
