"""Binary discovery tests — no binary needed, PATH/scripts-dir mocked."""

from __future__ import annotations

import macrocosim._process as rt


def test_which_binary_prefers_scripts_dir(monkeypatch) -> None:
    # Both places have it: the wheel's own copy wins over an older build on
    # PATH.
    monkeypatch.setattr(
        rt.shutil,
        "which",
        lambda name, path=None: "/on/path/x" if path is None else "/scripts/x",
    )
    assert rt.which_binary("x") == "/scripts/x"


def test_which_binary_falls_back_to_path(monkeypatch) -> None:
    monkeypatch.setattr(
        rt.shutil,
        "which",
        lambda name, path=None: "/on/path/x" if path is None else None,
    )
    assert rt.which_binary("x") == "/on/path/x"


def test_which_binary_looks_in_the_scripts_dir(monkeypatch) -> None:
    # Present only in the interpreter's scripts dir (bundled wheel).
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


def test_resolve_binary_prefers_env_var(monkeypatch, tmp_path) -> None:
    binary = tmp_path / "macrocosim"
    binary.write_text("")
    monkeypatch.setenv("MACROCOSIM_BIN", str(binary))
    monkeypatch.setattr(rt.shutil, "which", lambda name, path=None: "/scripts/x")
    found = rt.resolve_binary(
        "macrocosim", env_var="MACROCOSIM_BIN", explicit=None, flag="bin"
    )
    assert found == str(binary)
