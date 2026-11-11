# Scenarios

*Superseded in 0.2.* Lisp scenarios are gone, with Lisp. A scenario is now an
async Python function in a script, and a recording (`async with
microgrid.record(name)`) takes the reporter's place: see [Animation and
scenarios](../docs/python-scripts.md#animation-and-scenarios) and
[Recording](../docs/python-scripts.md#recording) in the scripting guide.
[`examples/testing/test_recording.py`](../examples/testing/test_recording.py)
records a run, and [`examples/faults/`](../examples/faults/) injects faults.
