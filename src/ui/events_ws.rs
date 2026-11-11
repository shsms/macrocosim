//! WebSocket push channel for the SPA.
//!
//! `/ws/events` upgrades to a long-lived socket; `event_pump`
//! subscribes to every microgrid's `SiteEvent` broadcast and fans
//! them out as JSON wrapped in a per-event microgrid_id tag.
//! Enterprise-scoped events (terminal log lines) ride without a tag.
//! An unloaded microgrid's forwarder stops and the socket sends
//! `microgrid_unloaded` for it.

use std::collections::{BTreeSet, HashMap};

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket};
use axum::response::IntoResponse;
use serde::Serialize;

use crate::engine::Engine;
use crate::sim::MicrogridSite;
use crate::sim::events::SiteEvent;
use crate::ui::api::WsUpgrade;

/// WebSocket event push. Subscribers receive SiteEvent JSON for every
/// TopologyChanged + Sample broadcast. Client-sent frames are drained
/// but ignored — the channel is server-push only. A request that is
/// not an upgrade gets a JSON error.
pub(super) async fn events_ws(
    WsUpgrade(ws): WsUpgrade,
    State(engine): State<Engine>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| event_pump(socket, engine))
}

/// Wrap a SiteEvent with the originating microgrid id so the SPA
/// can filter samples / topology bumps / setpoint events by the
/// currently-active microgrid. `microgrid_id` is `None` for enterprise-
/// scoped events (terminal log lines, ws lag notices).
#[derive(Serialize)]
struct WireEvent<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    microgrid_id: Option<u64>,
    #[serde(flatten)]
    event: &'a SiteEvent,
}

/// One socket's forwarder for one microgrid: the task that tags the
/// site's events with its id, and the site it follows, so a microgrid
/// created again under the same id is told from the one it replaced.
struct EventForwarder {
    site: MicrogridSite,
    task: tokio::task::JoinHandle<()>,
}

/// The forwarders of one socket, keyed by microgrid id, each feeding
/// `tx`.
struct EventForwarders {
    by_id: HashMap<u64, EventForwarder>,
    tx: tokio::sync::mpsc::Sender<(u64, SiteEvent)>,
}

impl EventForwarders {
    /// Make the forwarder for `id` follow the registry: start one for
    /// a site it does not follow yet (a new microgrid, or one created
    /// again under an unloaded id), and stop the one of a microgrid
    /// no longer registered. Returns the event that tells the SPA
    /// what changed: `topology_changed` with the current version for
    /// a site just followed, whose own first `topology_changed` went
    /// out before this socket subscribed to it; `microgrid_unloaded`
    /// for a microgrid gone; none for one already followed.
    fn reconcile(&mut self, engine: &Engine, id: u64) -> Option<SiteEvent> {
        let site = engine.microgrids().lock().get(&id).map(|e| e.site.clone());
        match site {
            Some(site) if self.by_id.get(&id).is_some_and(|f| f.site.ptr_eq(&site)) => None,
            Some(site) => {
                let task = spawn_forwarder(id, site.subscribe_events(), self.tx.clone());
                let version = site.version();
                if let Some(old) = self.by_id.insert(id, EventForwarder { site, task }) {
                    old.task.abort();
                }
                Some(SiteEvent::TopologyChanged { version })
            }
            None => {
                if let Some(old) = self.by_id.remove(&id) {
                    old.task.abort();
                }
                Some(SiteEvent::MicrogridUnloaded)
            }
        }
    }

    /// [`Self::reconcile`] every registered id and every followed
    /// one; returns each id with its event.
    fn reconcile_all(&mut self, engine: &Engine) -> Vec<(u64, Option<SiteEvent>)> {
        let mut ids: BTreeSet<u64> = engine.microgrids().lock().keys().copied().collect();
        ids.extend(self.by_id.keys().copied());
        ids.into_iter()
            .map(|id| (id, self.reconcile(engine, id)))
            .collect()
    }

    fn abort_all(self) {
        for f in self.by_id.into_values() {
            f.task.abort();
        }
    }
}

/// Forward one site's events, tagged with `mg_id`, onto `tx`.
fn spawn_forwarder(
    mg_id: u64,
    mut rx: tokio::sync::broadcast::Receiver<SiteEvent>,
    tx: tokio::sync::mpsc::Sender<(u64, SiteEvent)>,
) -> tokio::task::JoinHandle<()> {
    use tokio::sync::broadcast::error::RecvError;
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if tx.send((mg_id, ev)).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    log::warn!("ws: microgrid {mg_id} event bus lagged {n} samples");
                    continue;
                }
                Err(RecvError::Closed) => break,
            }
        }
    })
}

/// Send `event` for `microgrid_id` as one JSON text frame; `false`
/// when the socket is gone.
async fn send_wire(socket: &mut WebSocket, microgrid_id: Option<u64>, event: &SiteEvent) -> bool {
    let wire = WireEvent {
        microgrid_id,
        event,
    };
    match serde_json::to_string(&wire) {
        Ok(json) => socket.send(Message::Text(json.into())).await.is_ok(),
        Err(e) => {
            log::error!("ws: serde error: {e}");
            true
        }
    }
}

/// One registry notice (an id registered or unloaded): reconcile the
/// id, or, when the channel lagged, re-snapshot the whole registry.
/// `false` when the socket is gone.
async fn on_registry_notice(
    socket: &mut WebSocket,
    forwarders: &mut EventForwarders,
    engine: &Engine,
    notice: Result<u64, tokio::sync::broadcast::error::RecvError>,
    channel: &str,
) -> bool {
    use tokio::sync::broadcast::error::RecvError;
    match notice {
        Ok(id) => match forwarders.reconcile(engine, id) {
            Some(event) => send_wire(socket, Some(id), &event).await,
            None => true,
        },
        Err(RecvError::Lagged(n)) => {
            log::warn!("ws: {channel} channel lagged {n} ids; re-snapshotting registry");
            for (id, event) in forwarders.reconcile_all(engine) {
                if let Some(event) = event
                    && !send_wire(socket, Some(id), &event).await
                {
                    return false;
                }
            }
            true
        }
        Err(RecvError::Closed) => true,
    }
}

async fn event_pump(mut socket: WebSocket, engine: Engine) {
    use tokio::sync::broadcast::error::RecvError as BroadcastRecv;
    let mut log_rx = crate::ui_log::LOG_TAP.get().map(|t| t.subscribe());
    // Enterprise-wide dispatch lifecycle changes. Wrapped in Option
    // so a Closed (store dropped — shouldn't happen, it lives on the
    // Engine) parks the branch instead of busy-looping, mirroring
    // `log_rx`.
    let mut dispatch_rx = Some(engine.dispatches().subscribe());
    // Subscribe to every microgrid's per-site event bus + the
    // enterprise-wide `microgrid_registered` and `microgrid_unloaded`
    // channels. The initial snapshot covers every entry present at
    // connect time; the two channel branches in the select! below
    // start a forwarder when a microgrid is added mid-session and
    // stop it when one is unloaded. One forwarder task per site tags
    // events with the originating mg_id and pushes onto a shared mpsc
    // that the select! drains into the WebSocket.
    let (fwd_tx, mut fwd_rx) = tokio::sync::mpsc::channel::<(u64, SiteEvent)>(512);
    // Subscribe BEFORE the registry snapshot below: a microgrid
    // registered or unloaded between the two then shows up either in
    // the snapshot or on these receivers — never in neither. Both
    // branches reconcile the id against the registry, so the overlap,
    // and the two channels arriving in either order, are safe.
    // Wrapped in an Option so the select arm can park cleanly after
    // Closed.
    let mut registered_rx = Some(engine.subscribe_microgrid_registered());
    let mut unloaded_rx = Some(engine.subscribe_microgrid_unloaded());
    // Aborted when the socket closes — without that, a forwarder for
    // an idle microgrid parks until that site's NEXT event before
    // noticing the closed mpsc and exiting. The set keeps a clone of
    // fwd_tx, so fwd_rx stays open across a window of "no microgrids
    // yet".
    let mut forwarders = EventForwarders {
        by_id: HashMap::new(),
        tx: fwd_tx,
    };
    forwarders.reconcile_all(&engine);
    loop {
        tokio::select! {
            ev = fwd_rx.recv() => match ev {
                Some((mg_id, event)) => {
                    if !send_wire(&mut socket, Some(mg_id), &event).await {
                        break;
                    }
                }
                None => break, // unreachable: `forwarders` holds a sender
            },
            // Log tap branch — only fires when LOG_TAP was initialised
            // (i.e. running under the binary, not in a unit test).
            log = async {
                match log_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => match log {
                Ok(line) => {
                    let event = SiteEvent::Log {
                        ts_ms: line.ts_ms,
                        level: line.level,
                        target: line.target,
                        message: line.message,
                    };
                    if !send_wire(&mut socket, None, &event).await {
                        break;
                    }
                }
                Err(BroadcastRecv::Lagged(_)) => continue,
                Err(BroadcastRecv::Closed) => log_rx = None,
            },
            // Dispatch store changes — re-emit as a DispatchChanged
            // wire event tagged with the affected microgrid so the
            // SPA's per-microgrid Dispatches view refetches.
            dispatch = async {
                match dispatch_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => match dispatch {
                Ok(dev) => {
                    let change = match dev.change {
                        crate::sim::dispatch::DispatchChange::Created => "created",
                        crate::sim::dispatch::DispatchChange::Updated => "updated",
                        crate::sim::dispatch::DispatchChange::Deleted => "deleted",
                    };
                    let event = SiteEvent::DispatchChanged {
                        dispatch_id: dev.dispatch_id(),
                        change,
                    };
                    if !send_wire(&mut socket, Some(dev.microgrid_id), &event).await {
                        break;
                    }
                }
                Err(BroadcastRecv::Lagged(_)) => continue,
                Err(BroadcastRecv::Closed) => dispatch_rx = None,
            },
            msg = socket.recv() => match msg {
                Some(Ok(_)) => {}
                Some(Err(_)) | None => break,
            },
            // A microgrid landed in the registry (POST /api/microgrids, an
            // import or a declaration), or was registered again. Follow its
            // site so this WS session receives its sample / topology_changed
            // events. Subscribers can lag if registrations burst past the
            // channel capacity: re-snapshot the registry then. Closed never
            // fires since the Engine keeps the Sender alive for the process
            // lifetime.
            new_id = async {
                match registered_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if matches!(new_id, Err(BroadcastRecv::Closed)) {
                    // Park this arm via the None branch of its future
                    // (awaiting pending() HERE would park the whole
                    // select loop, not just this arm) — the existing
                    // forwarders keep working.
                    registered_rx = None;
                } else if !on_registry_notice(
                    &mut socket,
                    &mut forwarders,
                    &engine,
                    new_id,
                    "microgrid_registered",
                )
                .await
                {
                    break;
                }
            }
            // A microgrid was unloaded: stop following it and tell
            // the SPA. One created again meanwhile under the same id
            // is followed instead, and the SPA is told its topology
            // changed.
            gone = async {
                match unloaded_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if matches!(gone, Err(BroadcastRecv::Closed)) {
                    unloaded_rx = None;
                } else if !on_registry_notice(
                    &mut socket,
                    &mut forwarders,
                    &engine,
                    gone,
                    "microgrid_unloaded",
                )
                .await
                {
                    break;
                }
            }
        }
    }
    // Socket closed (every `break` above lands here): abort the
    // forwarders now instead of letting each park until its site's
    // next event reveals the dropped mpsc.
    forwarders.abort_all();
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use crate::engine::Metadata;
    use crate::engine::topology;
    use crate::runtime::MicrogridRuntimes;
    use crate::sim::sim_clock::{ManualClock, NowSource, headless_base};
    use crate::spec::TopologyDoc;

    fn engine() -> Engine {
        Engine::new(
            PathBuf::from("."),
            Metadata::default(),
            NowSource::sim(headless_base(), Arc::new(ManualClock::new())),
        )
    }

    async fn create(engine: &Engine, meter: u64) -> u64 {
        let doc = format!(
            r#"{{"schema_version": 1, "microgrids": [{{"id": 7, "components": [
                {{"type": "grid_connection_point", "id": 1}},
                {{"type": "meter", "id": {meter}}}], "connections": [[1, {meter}]]}}]}}"#
        );
        let spec = TopologyDoc::from_json(&doc).unwrap().microgrids.remove(0);
        topology::create(
            &engine.structural().await,
            &MicrogridRuntimes::inert(),
            spec,
            None,
        )
        .await
        .unwrap()
        .id
    }

    /// A site followed after the connect is announced with its
    /// current topology version, also when it replaced an unloaded
    /// one under the same id; an unloaded one is announced as such.
    #[tokio::test]
    async fn a_site_followed_later_is_announced() {
        let engine = engine();
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let mut forwarders = EventForwarders {
            by_id: HashMap::new(),
            tx,
        };
        let id = create(&engine, 2).await;
        let version = engine.microgrids().lock()[&id].site.version();
        assert!(matches!(
            forwarders.reconcile(&engine, id),
            Some(SiteEvent::TopologyChanged { version: v }) if v == version
        ));
        assert!(forwarders.reconcile(&engine, id).is_none());

        // Unloaded and created again before the socket looks: the new
        // site is followed and announced.
        topology::unload(&engine.structural().await, &MicrogridRuntimes::inert(), id)
            .await
            .unwrap();
        create(&engine, 50).await;
        assert!(matches!(
            forwarders.reconcile(&engine, id),
            Some(SiteEvent::TopologyChanged { .. })
        ));
        assert!(forwarders.by_id[&id].site.get(50).is_some());

        topology::unload(&engine.structural().await, &MicrogridRuntimes::inert(), id)
            .await
            .unwrap();
        assert!(matches!(
            forwarders.reconcile(&engine, id),
            Some(SiteEvent::MicrogridUnloaded)
        ));
        forwarders.abort_all();
    }
}
