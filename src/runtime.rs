//! One startup path per microgrid: physics tick, history sampler,
//! Microgrid gRPC server and UI loopback client. The UI client reads
//! a private copy of the service through memory, so it works whether
//! or not the public server binds. A microgrid that cannot bind,
//! whose site was replaced, or whose public server ends is marked
//! failed; the end of its in-memory copy is only logged. The process
//! stays up. A microgrid's gRPC address is fixed after its first
//! successful bind, until [`MicrogridRuntimes::restart_grpc`] moves
//! it; [`MicrogridRuntimes::stop`] ends every task a runtime started.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Weak};
use std::time::Duration;

use frequenz_microgrid::MicrogridClientHandle;
use frequenz_microgrid::client::proto::microgrid::microgrid_client::MicrogridClient;
use parking_lot::Mutex;
use tokio::sync::watch;
use tokio::task::{AbortHandle, JoinError, JoinHandle};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::{TcpListenerStream, UnboundedReceiverStream};
use tonic::transport::{Endpoint, Server, Uri};

use crate::engine::Engine;
use crate::proto::microgrid::microgrid_server::MicrogridServer as MicrogridGrpcServer;
use crate::server::MicrogridServer;
use crate::sim::MicrogridSite;
use crate::sim::microgrid_site::BackgroundTasks;
use crate::ui::{self, MicrogridLoopbacks};

/// How long a stopping gRPC server may take to close its connections
/// before its task is aborted.
const GRPC_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// How [`MicrogridRuntimes::start`] binds a microgrid's gRPC server.
#[derive(Clone, Copy, Debug)]
pub struct RuntimeOptions {
    /// Bind port 0 (OS-chosen) on the first bind.
    pub ephemeral_ports: bool,
    pub bind_host: IpAddr,
}

/// Where a runtime is, as [`RuntimeView`] reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeStatus {
    Starting,
    Running,
    Failed,
}

/// A runtime as `/api/microgrids` and the create routes report it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RuntimeView {
    pub status: RuntimeStatus,
    pub grpc_addr: Option<String>,
    pub error: Option<String>,
}

/// Why [`MicrogridRuntimes::start`] could not start a microgrid.
#[derive(Debug)]
pub enum StartError {
    NotRegistered(u64),
    SiteReplaced(u64),
    Bind { addr: SocketAddr, error: String },
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRegistered(id) => write!(f, "microgrid {id} is not registered"),
            Self::SiteReplaced(id) => {
                write!(f, "microgrid {id}: site replaced; restart macrocosim")
            }
            Self::Bind { addr, error } => write!(f, "gRPC bind {addr} failed: {error}"),
        }
    }
}

impl std::error::Error for StartError {}

#[derive(Clone, Debug)]
enum Status {
    Starting,
    Running,
    Failed(String),
}

struct Runtime {
    status: Status,
    /// Set by the first successful bind; every later attempt
    /// rebinds it.
    grpc_addr: Option<SocketAddr>,
    site: MicrogridSite,
    /// Bumped on every attempt; a server watcher only touches the
    /// status of its own attempt.
    generation: u64,
    /// What `stop` ends.
    tasks: Tasks,
}

/// The tasks one runtime started, each filled by the start that
/// started it.
#[derive(Default)]
struct Tasks {
    /// The public gRPC server of the last successful bind.
    grpc: Option<GrpcTask>,
    /// The in-memory copy of the service the loopback reads.
    in_memory: Option<AbortHandle>,
    background: Option<BackgroundTasks>,
    loopback: Option<JoinHandle<()>>,
    /// The loopback's client, kept for good once the loopback stops.
    client: Option<MicrogridClientHandle>,
}

/// A public gRPC server task.
struct GrpcTask {
    /// Set to `true` to shut the server down gracefully.
    shutdown: watch::Sender<bool>,
    server: AbortHandle,
    /// Ends once the server task has.
    watcher: JoinHandle<()>,
}

impl GrpcTask {
    /// Shut the server down and wait up to [`GRPC_SHUTDOWN_GRACE`]
    /// for its connections to close, then abort it. Returns once the
    /// task, and with it the listener, is gone.
    async fn stop(self) {
        let _ = self.shutdown.send(true);
        let mut watcher = self.watcher;
        if tokio::time::timeout(GRPC_SHUTDOWN_GRACE, &mut watcher)
            .await
            .is_err()
        {
            self.server.abort();
            let _ = watcher.await;
        }
    }
}

struct Inner {
    engine: Engine,
    options: RuntimeOptions,
    runtimes: Mutex<BTreeMap<u64, Runtime>>,
    /// Serializes `start`, `stop` and `restart_grpc`, so two callers
    /// cannot start one microgrid twice or start one being stopped.
    start_lock: tokio::sync::Mutex<()>,
    /// The loopback clients of stopped runtimes. Dropping the last
    /// clone of one makes its actor spin (see
    /// [`ui::spawn_microgrid_loopback`]).
    retired_clients: Mutex<Vec<MicrogridClientHandle>>,
}

/// The per-microgrid runtimes, keyed by microgrid id.
#[derive(Clone)]
pub struct MicrogridRuntimes {
    /// `None` for [`Self::inert`].
    inner: Option<Arc<Inner>>,
    loopbacks: MicrogridLoopbacks,
}

impl MicrogridRuntimes {
    /// Refuses a headless `Engine`: wall-clock physics must never
    /// start on one. Installs the port-pin check `make-microgrid`
    /// asks on a reload, and refuses an `Engine` that already has
    /// one: one `Engine` has one set of runtimes.
    pub fn new(engine: Engine, options: RuntimeOptions) -> Result<Self, String> {
        if engine.is_headless() {
            return Err("MicrogridRuntimes cannot run a headless Engine".into());
        }
        let inner = Arc::new(Inner {
            engine: engine.clone(),
            options,
            runtimes: Mutex::new(BTreeMap::new()),
            start_lock: tokio::sync::Mutex::new(()),
            retired_clients: Mutex::new(Vec::new()),
        });
        let weak = Arc::downgrade(&inner);
        engine
            .port_pins()
            .set(Arc::new(move |id| {
                weak.upgrade().is_some_and(|inner| {
                    inner.runtimes.lock().get(&id).is_some_and(|rt| {
                        rt.grpc_addr.is_some() || matches!(rt.status, Status::Starting)
                    })
                })
            }))
            .map_err(|_| "this Engine already has a MicrogridRuntimes")?;
        Ok(Self {
            inner: Some(inner),
            loopbacks: ui::new_microgrid_loopbacks(),
        })
    }

    /// Starts nothing: `start` answers `Ok(None)`, `status` `None`.
    pub fn inert() -> Self {
        Self {
            inner: None,
            loopbacks: ui::new_microgrid_loopbacks(),
        }
    }

    /// The loopback slots the UI routes read.
    pub fn loopbacks(&self) -> MicrogridLoopbacks {
        self.loopbacks.clone()
    }

    pub fn status(&self, id: u64) -> Option<RuntimeView> {
        let inner = self.inner.as_ref()?;
        let rts = inner.runtimes.lock();
        let rt = rts.get(&id)?;
        let (status, error) = match &rt.status {
            Status::Starting => (RuntimeStatus::Starting, None),
            Status::Running => (RuntimeStatus::Running, None),
            Status::Failed(e) => (RuntimeStatus::Failed, Some(e.clone())),
        };
        Some(RuntimeView {
            status,
            grpc_addr: rt.grpc_addr.map(|a| a.to_string()),
            error,
        })
    }

    /// Start `id`'s runtime, or return the address of the one already
    /// running. Safe to call any number of times.
    pub async fn start(&self, id: u64) -> Result<Option<SocketAddr>, StartError> {
        let Some(inner) = &self.inner else {
            return Ok(None);
        };
        let _serialized = inner.start_lock.lock().await;
        // Registry, then status: the order make-microgrid's pin check
        // takes them in. The port is read under both, so a reload
        // cannot move it once this runtime is Starting.
        let (site, addr, generation) = {
            let reg = inner.engine.microgrids();
            let reg = reg.lock();
            let e = reg.get(&id).ok_or(StartError::NotRegistered(id))?;
            let site = e.site.clone();
            let mut rts = inner.runtimes.lock();
            let (fixed, generation) = match rts.get_mut(&id) {
                Some(rt) if !rt.site.ptr_eq(&site) => {
                    let err = StartError::SiteReplaced(id);
                    // A new generation, so the old server's watcher
                    // cannot overwrite this error.
                    rt.generation += 1;
                    rt.status = Status::Failed(err.to_string());
                    return Err(err);
                }
                Some(rt) if matches!(rt.status, Status::Running) => return Ok(rt.grpc_addr),
                Some(rt) => {
                    rt.status = Status::Starting;
                    rt.generation += 1;
                    (rt.grpc_addr, rt.generation)
                }
                None => {
                    rts.insert(
                        id,
                        Runtime {
                            status: Status::Starting,
                            grpc_addr: None,
                            site: site.clone(),
                            generation: 1,
                            tasks: Tasks::default(),
                        },
                    );
                    (None, 1)
                }
            };
            let addr = match fixed {
                Some(addr) => addr,
                None => inner.bind_addr(e.def.grpc_port),
            };
            (site, addr, generation)
        };
        if let Some(background) = site.spawn_background() {
            inner.with_tasks(id, |t| t.background = Some(background));
        }
        // The first start makes the loopback slot and its in-memory
        // client; a later start reuses both.
        let new_slot = match self.loopbacks.write().entry(id) {
            Entry::Vacant(e) => Some(e.insert(ui::new_microgrid_slot()).clone()),
            Entry::Occupied(_) => None,
        };
        if let Some(slot) = new_slot {
            let server = MicrogridServer::new(inner.engine.clone(), id, site.clone());
            let (client, in_memory) = in_memory_client(server);
            let loopback = ui::spawn_microgrid_loopback(client.clone(), slot, site.clone());
            inner.with_tasks(id, |t| {
                t.in_memory = Some(in_memory);
                t.loopback = Some(loopback);
                t.client = Some(client);
            });
        }
        inner.serve_grpc(id, site, addr, generation).await.map(Some)
    }

    /// Stop `id`'s runtime and forget it: end its telemetry streams,
    /// shut its gRPC server down (aborted after 2 s), abort its
    /// in-memory server, physics tick, history sampler and loopback,
    /// and drop its loopback slot. Its port is free and its address
    /// forgotten once this returns, so a microgrid registered later
    /// under `id` starts clean. Does nothing for a microgrid with no
    /// runtime.
    pub async fn stop(&self, id: u64) {
        let Some(inner) = &self.inner else {
            return;
        };
        let _serialized = inner.start_lock.lock().await;
        let Some(rt) = inner.runtimes.lock().remove(&id) else {
            return;
        };
        let Runtime { site, tasks, .. } = rt;
        site.cancel_all_streams();
        if let Some(grpc) = tasks.grpc {
            grpc.stop().await;
        }
        if let Some(in_memory) = tasks.in_memory {
            in_memory.abort();
        }
        if let Some(background) = tasks.background {
            site.stop_background(background).await;
        }
        let slot = self.loopbacks.write().remove(&id);
        if let Some(loopback) = tasks.loopback {
            ui::stop_microgrid_loopback(loopback, slot).await;
        }
        if let Some(client) = tasks.client {
            inner.retired_clients.lock().push(client);
        }
        log::info!("Microgrid #{id}: runtime stopped");
    }

    /// Move `id`'s gRPC server to `port`: end its telemetry streams,
    /// shut the old server down as [`Self::stop`] does, forget its
    /// address and bind the new one (port 0 under ephemeral ports).
    /// The physics, the in-memory server and the loopback keep
    /// running. `Ok(None)` for a microgrid with no runtime, which
    /// binds `port` when it starts.
    pub async fn restart_grpc(&self, id: u64, port: u16) -> Result<Option<SocketAddr>, StartError> {
        let Some(inner) = &self.inner else {
            return Ok(None);
        };
        let _serialized = inner.start_lock.lock().await;
        let (site, old, generation) = {
            let mut rts = inner.runtimes.lock();
            let Some(rt) = rts.get_mut(&id) else {
                return Ok(None);
            };
            rt.generation += 1;
            rt.status = Status::Starting;
            rt.grpc_addr = None;
            (rt.site.clone(), rt.tasks.grpc.take(), rt.generation)
        };
        site.cancel_all_streams();
        if let Some(old) = old {
            old.stop().await;
        }
        let addr = inner.bind_addr(port);
        inner.serve_grpc(id, site, addr, generation).await.map(Some)
    }

    /// Start every registered microgrid, then keep starting each one
    /// the Lisp layer announces. Subscribes before reading the
    /// registry, so none is missed; returns once the first pass is
    /// done.
    pub async fn spawn_registration_listener(&self) -> JoinHandle<()> {
        let Some(inner) = self.inner.clone() else {
            return tokio::spawn(async {});
        };
        let mut rx = inner.engine.subscribe_microgrid_registered();
        self.start_all(&inner).await;
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(id) => {
                        if let Err(e) = me.start(id).await {
                            log::error!("Microgrid #{id}: {e}");
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        log::warn!("runtime listener lagged {n} registrations; re-snapshotting");
                        me.start_all(&inner).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        })
    }

    /// `start` every id in the registry, lowest first, logging each
    /// error.
    async fn start_all(&self, inner: &Inner) {
        let ids: Vec<u64> = inner.engine.microgrids().lock().keys().copied().collect();
        for id in ids {
            if let Err(e) = self.start(id).await {
                log::error!("Microgrid #{id}: {e}");
            }
        }
    }
}

impl Inner {
    /// The address a first bind of `port` uses.
    fn bind_addr(&self, port: u16) -> SocketAddr {
        let port = if self.options.ephemeral_ports {
            0
        } else {
            port
        };
        SocketAddr::new(self.options.bind_host, port)
    }

    /// Change the tasks of `id`'s runtime. The caller holds the start
    /// lock, so the runtime is there.
    fn with_tasks(&self, id: u64, f: impl FnOnce(&mut Tasks)) {
        let mut rts = self.runtimes.lock();
        f(&mut rts.get_mut(&id).expect("the runtime is registered").tasks);
    }

    /// Bind `addr` and serve `id`'s gRPC server on it, as attempt
    /// `generation`. The caller holds the start lock.
    async fn serve_grpc(
        self: &Arc<Self>,
        id: u64,
        site: MicrogridSite,
        addr: SocketAddr,
        generation: u64,
    ) -> Result<SocketAddr, StartError> {
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                let err = StartError::Bind {
                    addr,
                    error: e.to_string(),
                };
                set_status(self, id, generation, Status::Failed(err.to_string()));
                return Err(err);
            }
        };
        let bound = listener.local_addr().unwrap_or(addr);
        // Marked running before the server task exists, so a server
        // that ends at once leaves the microgrid failed.
        {
            let mut rts = self.runtimes.lock();
            let rt = rts.get_mut(&id).expect("the runtime is registered");
            rt.grpc_addr = Some(bound);
            if rt.generation == generation {
                rt.status = Status::Running;
            }
        }
        let server = MicrogridServer::new(self.engine.clone(), id, site);
        let (shutdown, stopping) = watch::channel(false);
        let mut signal = stopping.clone();
        let task = tokio::spawn(
            Server::builder()
                .add_service(MicrogridGrpcServer::new(server))
                .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async move {
                    // A dropped sender shuts it down too.
                    let _ = signal.wait_for(|stop| *stop).await;
                }),
        );
        let server = task.abort_handle();
        let watcher = tokio::spawn(watch_server(
            Arc::downgrade(self),
            id,
            generation,
            task,
            stopping,
        ));
        self.with_tasks(id, |t| {
            t.grpc = Some(GrpcTask {
                shutdown,
                server,
                watcher,
            });
        });
        log::info!("Microgrid #{id} gRPC listening on {bound}");
        Ok(bound)
    }
}

/// Bytes each in-memory connection buffers in either direction.
const IN_MEMORY_BUFFER: usize = 64 * 1024;

/// A client for `server` that talks to it through memory, not a
/// socket, and the server's task. Each connect makes a new in-memory
/// pipe and hands the server its end. The server runs until the task
/// is aborted; if it ends any other way, that is logged.
fn in_memory_client(server: MicrogridServer) -> (MicrogridClientHandle, AbortHandle) {
    let (conn_tx, conn_rx) = tokio::sync::mpsc::unbounded_channel::<tokio::io::DuplexStream>();
    let id = server.microgrid_id;
    let incoming = UnboundedReceiverStream::new(conn_rx).map(Ok::<_, std::io::Error>);
    let serving = tokio::spawn(
        Server::builder()
            .add_service(MicrogridGrpcServer::new(server))
            .serve_with_incoming(incoming),
    );
    let abort = serving.abort_handle();
    tokio::spawn(async move {
        match serving.await {
            Err(e) if e.is_cancelled() => {}
            ended => log::error!("Microgrid #{id}: in-memory {}", end_reason(ended)),
        }
    });
    let channel = Endpoint::from_static("http://in-memory.invalid").connect_with_connector_lazy(
        tower::service_fn(move |_: Uri| {
            let (client, server) = tokio::io::duplex(IN_MEMORY_BUFFER);
            let sent = conn_tx.send(server);
            async move {
                sent.map_err(|_| std::io::Error::other("in-memory gRPC server is gone"))?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(client))
            }
        }),
    );
    let client = MicrogridClientHandle::new_from_client(MicrogridClient::new(channel));
    (client, abort)
}

fn set_status(inner: &Inner, id: u64, generation: u64, status: Status) {
    if let Some(rt) = inner.runtimes.lock().get_mut(&id)
        && rt.generation == generation
    {
        rt.status = status;
    }
}

/// Await a gRPC server task and mark its microgrid failed when it
/// ends, unless `stopping` says it was shut down on purpose.
async fn watch_server(
    inner: Weak<Inner>,
    id: u64,
    generation: u64,
    handle: JoinHandle<Result<(), tonic::transport::Error>>,
    stopping: watch::Receiver<bool>,
) {
    let reason = end_reason(handle.await);
    if *stopping.borrow() {
        log::info!("Microgrid #{id}: gRPC server stopped");
        return;
    }
    log::error!("Microgrid #{id}: {reason}");
    if let Some(inner) = inner.upgrade() {
        set_status(&inner, id, generation, Status::Failed(reason));
    }
}

/// How a gRPC server task ended, whether it returned or panicked.
fn end_reason(ended: Result<Result<(), tonic::transport::Error>, JoinError>) -> String {
    match ended {
        Ok(Ok(())) => "gRPC server exited".to_string(),
        Ok(Err(e)) => format!("gRPC server error: {e}"),
        Err(e) if e.is_panic() => {
            let p = e.into_panic();
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            format!("gRPC server panicked: {msg}")
        }
        Err(e) => format!("gRPC server task ended: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lisp::Config;
    use std::net::{IpAddr, Ipv6Addr, SocketAddr};

    const V6: IpAddr = IpAddr::V6(Ipv6Addr::LOCALHOST);

    /// A port nobody holds right now, for tests that need a fixed
    /// one.
    fn free_v6_port() -> u16 {
        std::net::TcpListener::bind((Ipv6Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// A temp dir holding one file with `decl`.
    fn mg_file_with(decl: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::TempDir::with_prefix("mc-runtime-").unwrap();
        let path = dir.path().join("mg.lisp");
        std::fs::write(&path, decl).unwrap();
        (dir, path)
    }

    /// A temp dir holding one file that declares microgrid `id`.
    fn mg_file(id: u64, port: u16) -> (tempfile::TempDir, std::path::PathBuf) {
        mg_file_with(&mg_decl(id, port))
    }

    fn config_with_mg(id: u64, port: u16) -> (Config, tempfile::TempDir) {
        let (dir, path) = mg_file(id, port);
        (Config::new(path.to_str().unwrap()).unwrap(), dir)
    }

    /// Hand `watch_server` an already-ended server task for `id`.
    async fn end_server(inner: &Arc<Inner>, id: u64, generation: u64) {
        let ended = tokio::spawn(async { Ok::<(), tonic::transport::Error>(()) });
        let (_shutdown, stopping) = watch::channel(false);
        watch_server(Arc::downgrade(inner), id, generation, ended, stopping).await;
    }

    fn mg_decl(id: u64, port: u16) -> String {
        format!("(make-microgrid :id {id} :grpc-port {port} :topology (lambda () nil))")
    }

    fn opts(ephemeral: bool) -> RuntimeOptions {
        RuntimeOptions {
            ephemeral_ports: ephemeral,
            bind_host: V6,
        }
    }

    #[tokio::test]
    async fn start_twice_returns_the_same_address() {
        let (cfg, _d) = config_with_mg(41, free_v6_port());
        let rt = MicrogridRuntimes::new(cfg.engine().clone(), opts(true)).unwrap();
        let a = rt.start(41).await.unwrap().unwrap();
        let b = rt.start(41).await.unwrap().unwrap();
        assert_eq!(a, b);
        assert_eq!(rt.status(41).unwrap().status, RuntimeStatus::Running);
        assert_eq!(rt.status(41).unwrap().grpc_addr, Some(a.to_string()));
    }

    #[tokio::test]
    async fn a_held_port_fails_and_a_freed_one_retries() {
        let port = free_v6_port();
        let holder = std::net::TcpListener::bind((Ipv6Addr::LOCALHOST, port)).unwrap();
        let (cfg, _d) = config_with_mg(42, port);
        let rt = MicrogridRuntimes::new(cfg.engine().clone(), opts(false)).unwrap();
        assert!(matches!(rt.start(42).await, Err(StartError::Bind { .. })));
        let v = rt.status(42).unwrap();
        assert_eq!(v.status, RuntimeStatus::Failed);
        assert!(v.error.unwrap().contains(&port.to_string()));
        let slot = rt.loopbacks().read().get(&42).cloned().unwrap();
        drop(holder);
        let addr = rt.start(42).await.unwrap().unwrap();
        assert_eq!(addr, SocketAddr::new(V6, port));
        // The restart keeps the first start's loopback slot.
        assert!(Arc::ptr_eq(&slot, &rt.loopbacks().read()[&42]));
    }

    #[tokio::test]
    async fn an_unbound_microgrid_adopts_a_new_declared_port() {
        let port = free_v6_port();
        let holder = std::net::TcpListener::bind((Ipv6Addr::LOCALHOST, port)).unwrap();
        let (_dir, file) = mg_file(43, port);
        let cfg = Config::new(file.to_str().unwrap()).unwrap();
        let rt = MicrogridRuntimes::new(cfg.engine().clone(), opts(false)).unwrap();
        assert!(matches!(rt.start(43).await, Err(StartError::Bind { .. })));
        let new_port = free_v6_port();
        std::fs::write(&file, mg_decl(43, new_port)).unwrap();
        cfg.load_file(&file).unwrap();
        let addr = rt.start(43).await.unwrap().unwrap();
        assert_eq!(addr.port(), new_port);
        drop(holder);
    }

    #[tokio::test]
    async fn a_bound_microgrid_keeps_its_port_on_reload() {
        let port = free_v6_port();
        let (_dir, file) = mg_file(44, port);
        let cfg = Config::new(file.to_str().unwrap()).unwrap();
        let rt = MicrogridRuntimes::new(cfg.engine().clone(), opts(false)).unwrap();
        rt.start(44).await.unwrap();
        std::fs::write(&file, mg_decl(44, free_v6_port())).unwrap();
        cfg.load_file(&file).unwrap();
        assert_eq!(cfg.engine().microgrids().lock()[&44].def.grpc_port, port);
    }

    #[tokio::test]
    async fn physics_starts_once_across_starts() {
        let (cfg, _d) = config_with_mg(45, free_v6_port());
        let site = cfg.engine().microgrids().lock()[&45].site.clone();
        let rt = MicrogridRuntimes::new(cfg.engine().clone(), opts(true)).unwrap();
        rt.start(45).await.unwrap();
        rt.start(45).await.unwrap();
        assert!(
            site.spawn_background().is_none(),
            "start left physics unstarted"
        );
    }

    #[tokio::test]
    async fn unknown_id_errors_and_records_nothing() {
        let (cfg, _d) = config_with_mg(46, free_v6_port());
        let rt = MicrogridRuntimes::new(cfg.engine().clone(), opts(true)).unwrap();
        assert!(matches!(
            rt.start(999).await,
            Err(StartError::NotRegistered(999))
        ));
        assert!(rt.status(999).is_none());
    }

    #[tokio::test]
    async fn a_headless_engine_is_refused() {
        let (_dir, path) = mg_file(54, free_v6_port());
        let (cfg, _clock) = Config::new_headless(path.to_str().unwrap()).unwrap();
        assert!(MicrogridRuntimes::new(cfg.engine().clone(), opts(true)).is_err());
    }

    /// One Engine holds one port-pin check, so a second runtimes
    /// instance on it is refused and the first keeps answering.
    #[tokio::test]
    async fn a_second_runtimes_on_one_engine_is_refused() {
        let (cfg, _d) = config_with_mg(55, free_v6_port());
        let first = MicrogridRuntimes::new(cfg.engine().clone(), opts(true)).unwrap();
        let second = MicrogridRuntimes::new(cfg.engine().clone(), opts(true));
        assert!(second.is_err_and(|e| e.contains("already")));
        first.start(55).await.unwrap();
        assert!(crate::sim::microgrids::port_pinned(
            &cfg.engine().port_pins(),
            55
        ));
    }

    /// A runtime that is still starting holds its port, so a reload
    /// cannot move it under the bind.
    #[tokio::test]
    async fn a_starting_runtime_holds_its_port() {
        let (cfg, _d) = config_with_mg(56, free_v6_port());
        let site = cfg.engine().microgrids().lock()[&56].site.clone();
        let rt = MicrogridRuntimes::new(cfg.engine().clone(), opts(true)).unwrap();
        rt.inner.as_ref().unwrap().runtimes.lock().insert(
            56,
            Runtime {
                status: Status::Starting,
                grpc_addr: None,
                site,
                generation: 1,
                tasks: Tasks::default(),
            },
        );
        assert!(crate::sim::microgrids::port_pinned(
            &cfg.engine().port_pins(),
            56
        ));
    }

    #[tokio::test]
    async fn inert_starts_nothing() {
        let rt = MicrogridRuntimes::inert();
        assert_eq!(rt.start(1).await.unwrap(), None);
        assert!(rt.status(1).is_none());
    }

    /// The UI's client reaches the microgrid through memory, so a
    /// microgrid whose public port is held still has live data.
    #[tokio::test]
    async fn the_ui_client_works_without_the_public_port() {
        let port = free_v6_port();
        let holder = std::net::TcpListener::bind((Ipv6Addr::LOCALHOST, port)).unwrap();
        let (_dir, path) = mg_file_with(&format!(
            "(make-microgrid :id 57 :grpc-port {port} \
             :topology (lambda () (%make-grid-connection-point :id 1 \
             :successors (list (%make-meter :id 2)))))"
        ));
        let cfg = Config::new(path.to_str().unwrap()).unwrap();
        let rt = MicrogridRuntimes::new(cfg.engine().clone(), opts(false)).unwrap();
        assert!(matches!(rt.start(57).await, Err(StartError::Bind { .. })));
        let slot = rt.loopbacks().read().get(&57).cloned().unwrap();
        // The UI's graph handle builds over the in-memory channel.
        let client = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(mg) = &*slot.microgrid.read() {
                    break mg.client();
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the UI's graph handle is built");
        let components = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.list_electrical_components(vec![], vec![]),
        )
        .await
        .expect("the in-memory server answers")
        .unwrap();
        let mut ids: Vec<u64> = components.iter().map(|c| c.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![1, 2]);
        drop(holder);
    }

    /// A microgrid registered by an eval after the listener runs gets
    /// a runtime without anyone calling start.
    #[tokio::test]
    async fn the_listener_starts_microgrids_registered_later() {
        let (cfg, _d) = config_with_mg(48, free_v6_port());
        let rt = MicrogridRuntimes::new(cfg.engine().clone(), opts(true)).unwrap();
        let _listener = rt.spawn_registration_listener().await;
        assert_eq!(rt.status(48).unwrap().status, RuntimeStatus::Running);
        cfg.eval(&mg_decl(49, free_v6_port())).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while rt.status(49).map(|v| v.status) != Some(RuntimeStatus::Running) {
            assert!(
                std::time::Instant::now() < deadline,
                "microgrid 49 never started"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// The address is fixed after the first bind: under ephemeral
    /// ports a recomputed one would be a fresh OS-chosen port. The
    /// first server still holds the port, so the restart's bind of
    /// that same address fails.
    #[tokio::test]
    async fn a_failed_microgrid_restarts_on_its_first_address() {
        let (cfg, _d) = config_with_mg(52, free_v6_port());
        let rt = MicrogridRuntimes::new(cfg.engine().clone(), opts(true)).unwrap();
        let a = rt.start(52).await.unwrap().unwrap();
        let inner = rt.inner.clone().unwrap();
        let generation = inner.runtimes.lock()[&52].generation;
        end_server(&inner, 52, generation).await;
        assert_eq!(rt.status(52).unwrap().status, RuntimeStatus::Failed);
        match rt.start(52).await {
            Err(StartError::Bind { addr, .. }) => assert_eq!(addr, a),
            other => panic!("expected a bind of {a}, got {other:?}"),
        }
        assert_eq!(rt.status(52).unwrap().grpc_addr, Some(a.to_string()));
    }

    /// The old server's watcher cannot overwrite the site-replaced
    /// error.
    #[tokio::test]
    async fn a_replaced_site_stays_failed_when_the_old_server_ends() {
        let (cfg, _d) = config_with_mg(53, free_v6_port());
        let rt = MicrogridRuntimes::new(cfg.engine().clone(), opts(true)).unwrap();
        rt.start(53).await.unwrap();
        let inner = rt.inner.clone().unwrap();
        let generation = {
            let mut rts = inner.runtimes.lock();
            let r = rts.get_mut(&53).unwrap();
            r.site = MicrogridSite::new();
            r.generation
        };
        assert!(matches!(
            rt.start(53).await,
            Err(StartError::SiteReplaced(53))
        ));
        end_server(&inner, 53, generation).await;
        assert!(
            rt.status(53)
                .unwrap()
                .error
                .unwrap()
                .contains("site replaced")
        );
    }

    /// A stopped runtime frees its port and its loopback slot, and a
    /// later start of the same microgrid starts it afresh.
    #[tokio::test]
    async fn stop_frees_the_port_and_a_later_start_starts_afresh() {
        let port = free_v6_port();
        let (cfg, _d) = config_with_mg(58, port);
        let rt = MicrogridRuntimes::new(cfg.engine().clone(), opts(false)).unwrap();
        rt.start(58).await.unwrap();
        let slot = rt.loopbacks().read()[&58].clone();
        let started = std::time::Instant::now();
        rt.stop(58).await;
        assert!(
            started.elapsed() < GRPC_SHUTDOWN_GRACE,
            "an idle server stops at once"
        );
        assert!(rt.status(58).is_none());
        assert!(rt.loopbacks().read().get(&58).is_none());
        assert!(!crate::sim::microgrids::port_pinned(
            &cfg.engine().port_pins(),
            58
        ));
        drop(std::net::TcpListener::bind((Ipv6Addr::LOCALHOST, port)).expect("the port is free"));
        rt.stop(58).await;
        assert_eq!(rt.start(58).await.unwrap(), Some(SocketAddr::new(V6, port)));
        assert!(!Arc::ptr_eq(&slot, &rt.loopbacks().read()[&58]));
        assert_eq!(rt.status(58).unwrap().status, RuntimeStatus::Running);
        let site = cfg.engine().microgrids().lock()[&58].site.clone();
        assert!(
            site.spawn_background().is_none(),
            "the second start restarted physics"
        );
    }

    /// `restart_grpc` serves on the new port and frees the old one;
    /// the loopback keeps its slot.
    #[tokio::test]
    async fn restart_grpc_moves_the_server() {
        let (old, new) = (free_v6_port(), free_v6_port());
        let (cfg, _d) = config_with_mg(59, old);
        let rt = MicrogridRuntimes::new(cfg.engine().clone(), opts(false)).unwrap();
        rt.start(59).await.unwrap();
        let slot = rt.loopbacks().read()[&59].clone();
        let addr = rt.restart_grpc(59, new).await.unwrap().unwrap();
        assert_eq!(addr, SocketAddr::new(V6, new));
        let v = rt.status(59).unwrap();
        assert_eq!(v.status, RuntimeStatus::Running);
        assert_eq!(v.grpc_addr, Some(addr.to_string()));
        drop(
            std::net::TcpListener::bind((Ipv6Addr::LOCALHOST, old)).expect("the old port is free"),
        );
        tokio::net::TcpStream::connect(addr)
            .await
            .expect("the new port serves");
        assert!(Arc::ptr_eq(&slot, &rt.loopbacks().read()[&59]));
    }

    /// A runtime that never started has nothing to move.
    #[tokio::test]
    async fn restart_grpc_without_a_runtime_does_nothing() {
        let (cfg, _d) = config_with_mg(60, free_v6_port());
        let rt = MicrogridRuntimes::new(cfg.engine().clone(), opts(true)).unwrap();
        assert!(rt.restart_grpc(60, free_v6_port()).await.unwrap().is_none());
        assert!(rt.status(60).is_none());
    }

    #[tokio::test]
    async fn a_server_that_returns_or_panics_marks_the_microgrid_failed() {
        let (cfg, _d) = config_with_mg(47, free_v6_port());
        let rt = MicrogridRuntimes::new(cfg.engine().clone(), opts(true)).unwrap();
        rt.start(47).await.unwrap();
        let inner = rt.inner.clone().unwrap();
        let generation = inner.runtimes.lock()[&47].generation;
        end_server(&inner, 47, generation).await;
        assert_eq!(rt.status(47).unwrap().status, RuntimeStatus::Failed);
        let panicked = tokio::spawn(async { panic!("boom") });
        let (_shutdown, stopping) = watch::channel(false);
        watch_server(
            std::sync::Arc::downgrade(&inner),
            47,
            generation,
            panicked,
            stopping,
        )
        .await;
        assert!(rt.status(47).unwrap().error.unwrap().contains("boom"));
    }
}
