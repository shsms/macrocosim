"""Binary discovery tests — no binary needed, PATH/scripts-dir mocked."""

from __future__ import annotations

import macrocosim._process as rt


def test_which_binary_prefers_path(monkeypatch) -> None:
    monkeypatch.setattr(
        rt.shutil,
        "which",
        lambda name, path=None: "/on/path/x" if path is None else "/scripts/x",
    )
    assert rt.which_binary("x") == "/on/path/x"


def test_which_binary_falls_back_to_scripts_dir(monkeypatch) -> None:
    # Not on PATH, but present in the interpreter's scripts dir (bundled wheel).
    monkeypatch.setattr(
        rt.shutil,
        "which",
        lambda name, path=None: None if path is None else f"{path}/{name}",
    )
    monkeypatch.setattr(rt.sysconfig, "get_path", lambda key: "/venv/bin")
    assert rt.which_binary("macrocosim") == "/venv/bin/macrocosim"


def test_which_binary_none_when_absent(monkeypatch) -> None:
    monkeypatch.setattr(rt.shutil, "which", lambda name, path=None: None)
    assert rt.which_binary("macrocosim") is None
