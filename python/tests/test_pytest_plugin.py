"""Tests for the pytest plugin's own fixture, run through pytester.

The inner pytest run gets a conftest that replaces ``macrocosim.launch`` with
a fake, so no simulator binary is needed and the fake site records whether
the scenario was run and gated. The scenario named "bad" fails its gate; the
``boom`` fixture fails in setup.
"""

import pytest

pytest_plugins = ["pytester"]

CONFTEST = """
import contextlib

import macrocosim as mc
import pytest

LOG = []


class _Run:
    def __init__(self, log, name):
        self.log = log
        self.name = name

    def run(self, wait=True):
        self.log.append("run")
        return self

    def assert_passed(self):
        self.log.append("gate")
        if self.name == "bad":
            raise AssertionError("scenario check failed")


class _Site:
    def __init__(self, log):
        self.log = log

    def scenario(self, name):
        return _Run(self.log, name)


@contextlib.contextmanager
def _fake_launch(cfg, bin=None):
    yield _Site(LOG)


mc.launch = _fake_launch


@pytest.fixture
def macrocosim_config():
    return object()


@pytest.fixture
def boom():
    raise RuntimeError("this fixture fails in setup")
"""


def _run(pytester: pytest.Pytester, source: str) -> pytest.RunResult:
    """Run `source` as a test file against the fake-launch conftest."""
    pytester.makeconftest(CONFTEST)
    pytester.makepyfile(source)
    return pytester.runpytest_subprocess("-q")


def test_the_gate_runs_after_a_passing_body(pytester: pytest.Pytester) -> None:
    result = _run(
        pytester,
        """
        import pytest

        from conftest import LOG

        @pytest.mark.macrocosim_scenario("s")
        def test_green(macrocosim):
            pass

        def test_log():
            assert LOG == ["run", "gate"], LOG
        """,
    )
    result.assert_outcomes(passed=2)


def test_the_gate_is_skipped_when_the_body_failed(pytester: pytest.Pytester) -> None:
    result = _run(
        pytester,
        """
        import pytest

        from conftest import LOG

        @pytest.mark.macrocosim_scenario("s")
        def test_red(macrocosim):
            assert False

        def test_log():
            assert LOG == [], LOG
        """,
    )
    result.assert_outcomes(passed=1, failed=1)


def test_the_gate_is_skipped_when_the_body_skipped(pytester: pytest.Pytester) -> None:
    result = _run(
        pytester,
        """
        import pytest

        from conftest import LOG

        @pytest.mark.macrocosim_scenario("s")
        def test_skipped(macrocosim):
            pytest.skip("nothing to check here")

        def test_log():
            assert LOG == [], LOG
        """,
    )
    result.assert_outcomes(passed=1, skipped=1)


def test_the_gate_is_skipped_when_the_body_xfailed(pytester: pytest.Pytester) -> None:
    result = _run(
        pytester,
        """
        import pytest

        from conftest import LOG

        @pytest.mark.xfail(reason="known to be broken")
        @pytest.mark.macrocosim_scenario("s")
        def test_xfail(macrocosim):
            assert False

        def test_log():
            assert LOG == [], LOG
        """,
    )
    result.assert_outcomes(passed=1, xfailed=1)


def test_the_gate_is_skipped_when_another_fixture_errored(
    pytester: pytest.Pytester,
) -> None:
    result = _run(
        pytester,
        """
        import pytest

        from conftest import LOG

        @pytest.mark.macrocosim_scenario("s")
        def test_setup_error(macrocosim, boom):
            pass

        def test_log():
            assert LOG == [], LOG
        """,
    )
    result.assert_outcomes(passed=1, errors=1)


def test_a_failed_gate_is_a_teardown_error(pytester: pytest.Pytester) -> None:
    result = _run(
        pytester,
        """
        import pytest

        from conftest import LOG

        @pytest.mark.macrocosim_scenario("bad")
        def test_gate_fails(macrocosim):
            pass

        def test_log():
            assert LOG == ["run", "gate"], LOG
        """,
    )
    result.assert_outcomes(passed=2, errors=1)
