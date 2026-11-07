"""A script that never sends its hello: it writes its pid beside itself, then
sleeps before macrocosim is even imported."""

import os
import pathlib
import time

pathlib.Path(__file__).with_suffix(".pid").write_text(str(os.getpid()))
time.sleep(120)

import macrocosim  # noqa: E402,F401
