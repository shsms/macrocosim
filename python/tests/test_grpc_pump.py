"""A metric stream the server ends is not served from the cache forever:
once the pump ends, the cached value is dropped and the next read opens a
fresh stream.

Both clients are driven with a fake ``frequenz`` client: no connection is
opened. The fake caches one broadcaster per stream just as the real client
does — a re-subscribe reuses it, and one whose stream ended hands out
receivers that fail at once — so the tests measure streams actually opened,
not pumps started.
"""

from __future__ import annotations

import asyncio
import threading
from datetime import timedelta

import pytest

from macrocosim._grpc import _BROADCASTERS, GrpcClient
from macrocosim.aio._grpc import AsyncGrpcClient

_KEY = (1, "AC_POWER_ACTIVE")


class _Metric:
    def __init__(self, metric, value):
        self.metric = metric
        self._v = value

    def as_single_value(self):
        return self._v


class _Sample:
    def __init__(self, metric, value):
        self.metric_samples = [_Metric(metric, value)]


class _Broadcaster:
    """Stands in for the client's cached ``GrpcStreamBroadcaster``.

    One underlying stream per broadcaster, and the client caches one
    broadcaster per (component, metrics) key. Yields the queued values,
    then holds the stream open until ``ended`` — which a test sets to
    close the stream the way a CLOSED telemetry fault does. After that,
    like the real one with a closed channel, it still hands out receivers
    but they fail on the first ``receive()``.
    """

    def __init__(self, metric, values):
        self._metric = metric
        self._values = list(values)
        self.drained = threading.Event()
        self.ended = threading.Event()
        self.stopped = False
        # `stop()` suspends until `release` is set, which a test clears to
        # hold a pump inside its cleanup and read while it is in there.
        self.stopping = threading.Event()
        self.release = threading.Event()
        self.release.set()

    def new_receiver(self):
        return _Receiver(self)

    async def stop(self):
        self.stopping.set()
        while not self.release.is_set():
            await asyncio.sleep(0.01)
        self.stopped = True
        self.ended.set()

    async def receive(self):
        if self.ended.is_set():
            raise EOFError("stream closed")
        if self._values:
            return _Sample(self._metric, self._values.pop(0))
        # Out of queued values: hold the stream open until the test ends it.
        self.drained.set()
        while not self.ended.is_set():
            await asyncio.sleep(0.01)
        raise EOFError("stream ended")


class _Receiver:
    """A receiver off one broadcaster — several may share a broadcaster."""

    def __init__(self, broadcaster):
        self._broadcaster = broadcaster

    async def receive(self):
        return await self._broadcaster.receive()


class _FakeClient:
    """Mirrors the client's per-(component, metrics) broadcaster cache.

    Re-subscribing reuses the cached broadcaster, so ``streams_opened``
    counts real streams: it only moves when a subscribe had no broadcaster
    to reuse, which is the thing the fix has to make happen again.
    """

    def __init__(self, metric, batches):
        self.metric = metric
        self.batches = list(batches)  # one broadcaster's worth of values each
        # Same name the client uses, so the eviction path is exercised.
        self._component_data_broadcasters: dict[str, _Broadcaster] = {}
        self.opened: list[_Broadcaster] = []  # including the evicted ones
        self.streams_opened = 0
        self.subscribes = 0

    def receive_component_data_samples_stream(self, cid, metrics):
        self.subscribes += 1
        key = f"{int(cid)}-{hash(frozenset(metrics))}"
        broadcaster = self._component_data_broadcasters.get(key)
        if broadcaster is None:
            self.streams_opened += 1
            values = self.batches.pop(0) if self.batches else []
            broadcaster = _Broadcaster(self.metric, values)
            self._component_data_broadcasters[key] = broadcaster
            self.opened.append(broadcaster)
        return broadcaster.new_receiver()

    def live_broadcasters(self) -> list[_Broadcaster]:
        return list(self._component_data_broadcasters.values())


def _metric():
    from frequenz.client.microgrid import metrics as m

    return m.Metric["AC_POWER_ACTIVE"]


async def test_aio_read_returns_none_after_the_stream_ends_and_resubscribes() -> None:
    fake = _FakeClient(_metric(), [[7.0], [9.0]])
    c = AsyncGrpcClient("dummy://")
    c._client = fake
    try:
        assert await c.active_power(1) == 7.0
        assert fake.streams_opened == 1

        # End the first stream the way a CLOSED telemetry fault does, and
        # wait for the pump to finish reacting to it.
        first = fake.live_broadcasters()[0]
        assert first.drained.wait(2.0)
        reader = c._readers[_KEY]
        first.ended.set()
        await reader.task

        # The next read opens a new stream instead of serving the stale 7.0
        # (or re-subscribing to the dead broadcaster, which reads as None).
        assert await c.active_power(1) == 9.0, "the finished stream's value is evicted"
        assert fake.streams_opened == 2, "the read after the end opened a new stream"
        assert first.stopped, "the dead broadcaster was stopped, not just forgotten"

        # A new stream that publishes nothing reads as None, not as the last
        # value of the stream before it.
        second = fake.live_broadcasters()[0]
        assert second.drained.wait(2.0)
        reader = c._readers[_KEY]
        second.ended.set()
        await reader.task
        value = await c._read_metric(
            1, "AC_POWER_ACTIVE", first_wait=timedelta(seconds=0.2)
        )
        assert value is None, "no value is carried over from the ended stream"
        assert fake.streams_opened == 3
    finally:
        # End the last stream and let its pump finish, so nothing is left
        # pending on the loop at teardown.
        task = c._readers[_KEY].task if _KEY in c._readers else None
        for broadcaster in fake.opened:
            broadcaster.release.set()
            broadcaster.ended.set()
        if task is not None:
            await task
        await c.aclose()


def test_sync_read_returns_none_after_the_stream_ends_and_resubscribes(
    monkeypatch,
) -> None:
    fake = _FakeClient(_metric(), [[7.0], [9.0]])

    async def fake_make_client(_url: str):
        return fake

    # The only bit of __init__ that needs a server; the loop thread and the
    # caches are built for real.
    monkeypatch.setattr(GrpcClient, "_make_client", staticmethod(fake_make_client))
    c = GrpcClient("dummy://")
    try:
        assert c.active_power(1) == 7.0
        assert fake.streams_opened == 1

        # End the first stream. Hold the pump inside its cleanup, where the
        # pump record is still registered, and read: the value must already
        # be gone, or that read is served the pre-fault sample.
        first = fake.live_broadcasters()[0]
        assert first.drained.wait(2.0)
        pump = c._pumps[_KEY]
        first.release.clear()
        first.ended.set()
        assert first.stopping.wait(2.0)
        mid_cleanup = c._read_metric(1, "AC_POWER_ACTIVE", first_wait=0.2)
        assert mid_cleanup is None, "a read during the cleanup gets no stale value"
        first.release.set()
        pump.result(2.0)

        # The next read opens a new stream instead of serving the stale 7.0
        # (or re-subscribing to the dead broadcaster, which reads as None).
        assert c.active_power(1) == 9.0, "the finished stream's value is evicted"
        assert fake.streams_opened == 2, "the read after the end opened a new stream"
        assert first.stopped, "the dead broadcaster was stopped, not just forgotten"

        # A new stream that publishes nothing reads as None, not as the last
        # value of the stream before it.
        second = fake.live_broadcasters()[0]
        assert second.drained.wait(2.0)
        pump = c._pumps[_KEY]
        second.ended.set()
        pump.result(2.0)
        value = c._read_metric(1, "AC_POWER_ACTIVE", first_wait=0.2)
        assert value is None, "no value is carried over from the ended stream"
        assert fake.streams_opened == 3
    finally:
        # End the last stream and let its pump finish, so nothing is left
        # pending on the loop thread at teardown.
        pump = c._pumps.get(_KEY)
        for broadcaster in fake.opened:
            broadcaster.release.set()
            broadcaster.ended.set()
        if pump is not None:
            pump.result(2.0)
        c.close()


async def test_the_client_still_has_the_private_broadcaster_cache() -> None:
    """The eviction hangs off a private attribute of the upstream client.

    Every test above drives the fake, and the fake defines the attribute it
    is checked for — so an upstream rename would leave the whole suite green
    while `_evict_stream` quietly became a no-op and a closed stream never
    read again. This is the one test that asks the real client. Building it
    only creates a channel; nothing connects, so no server is needed — but
    it does grab the running loop, hence `async def`.
    """
    client_module = pytest.importorskip("frequenz.client.microgrid")

    client = client_module.MicrogridApiClient("grpc://127.0.0.1:1")
    assert hasattr(client, _BROADCASTERS), (
        f"frequenz-client-microgrid no longer caches broadcasters under "
        f"{_BROADCASTERS!r}; _grpc._evict_stream needs the new name"
    )
