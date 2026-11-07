//! The supervisor of a script: it starts a child for the script, and
//! a new one on every save, puts the new child's microgrids in place
//! when its hello arrives, and stops the old child just before.
//!
//! A reload goes:
//!
//! 1. A save of a `.py` file in the script's tree starts a new child
//!    with a new token; the old child keeps running. A newer save
//!    kills a child that has not sent its hello yet, so at most one
//!    is pending.
//! 2. A child that exits before its hello, or sends none within
//!    [`HELLO_DEADLINE`] (it is then killed), fails to load; the old
//!    child and the world are untouched.
//! 3. The hello is checked and its microgrids built aside under the
//!    create lock ([`topology::apply_hello`]). A refusal fails the
//!    load and changes nothing.
//! 4. If the new child is still alive, the old one is stopped (stdin
//!    closed, up to [`STOP_GRACE`](super::child::STOP_GRACE) to exit,
//!    then killed), then the microgrids are unloaded, rebuilt and
//!    created, and the hello is answered.
//!
//! The status keeps the running child's state apart from the last
//! reload's result: a failed reload leaves the old child running. A
//! child that crashed or failed to load is not started again until a
//! watched file changes.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;
use tokio::sync::watch;

use super::child::{Child, Exit};
use super::hello::{HelloAnswer, HelloError, HelloMicrogrid, HelloRequest};
use super::script_watch::{script_tree, state_dirs};
use crate::engine::{Engine, topology};
use crate::runtime::MicrogridRuntimes;
use crate::spec::TopologyDoc;
use std::time::Instant;

/// How long a new child has to send its hello.
pub const HELLO_DEADLINE: Duration = Duration::from_secs(30);

/// `GET /api/runner`, and the `script_status` event.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ScriptStatus {
    /// The script's absolute path; `null` without a script.
    pub script: Option<String>,
    /// The child that owns the script's microgrids, or the first
    /// child while it starts; `null` when there is neither.
    pub child: Option<ChildStatus>,
    pub last_reload: Option<ReloadStatus>,
    /// The last lines of `child`'s output.
    pub output: Vec<String>,
    /// The microgrid ids `child` declared.
    pub microgrids: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ChildStatus {
    pub state: ChildState,
    pub exit_code: Option<i32>,
    pub pid: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChildState {
    /// The first child, before its hello is answered.
    Starting,
    Running,
    /// `run` returned.
    Finished,
    /// The child exited with an error or was ended by a signal.
    Crashed,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReloadStatus {
    pub result: ReloadResult,
    /// When the reload started, or ended once it has.
    pub updated_at: String,
    /// The reloading child's output, and why it failed.
    pub output: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ReloadResult {
    #[serde(rename = "ok")]
    Ok,
    #[serde(rename = "failed_to_load")]
    FailedToLoad,
    #[serde(rename = "pending")]
    Pending,
}

impl ReloadStatus {
    fn now(result: ReloadResult, output: Vec<String>) -> Self {
        Self {
            result,
            updated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            output,
        }
    }
}

/// Where a child's hello is.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Hello {
    NotSent,
    /// Being checked and applied.
    InFlight,
    /// Refused, with the line that says why.
    Refused(String),
    /// Not sent within [`HELLO_DEADLINE`]; the child is being killed.
    TimedOut,
    /// Applied: the child owns the world.
    Applied,
}

struct Slot {
    child: Arc<Child>,
    token: String,
    hello: Hello,
    microgrids: Vec<u64>,
}

#[derive(Default)]
struct State {
    /// The child whose hello was applied last.
    running: Option<Slot>,
    /// The newest child, until its hello is applied or it fails.
    pending: Option<Slot>,
    last_reload: Option<ReloadStatus>,
    shutting_down: bool,
}

/// Runs one script for one engine; see the module docs.
pub struct ScriptSupervisor {
    engine: Engine,
    runtimes: MicrogridRuntimes,
    script: PathBuf,
    python: PathBuf,
    url: String,
    /// The script's file name, which leads each line of the
    /// children's output in the log, under the target `script:{name}`.
    name: String,
    state: Mutex<State>,
    /// Held by a reload while it replaces the pending child, and by a
    /// hello while it is applied: one at a time.
    ops: tokio::sync::Mutex<()>,
    /// Set once the first child's hello was answered, or it failed to
    /// load.
    first_load: watch::Sender<bool>,
}

impl ScriptSupervisor {
    /// The log target of the children's output: `script:plant.py`.
    fn target(&self) -> String {
        format!("script:{}", self.name)
    }

    /// A supervisor for `script` (an absolute path) run with
    /// `python`, its children reaching the engine's UI server at
    /// `ui`. Nothing runs until [`Self::start`].
    pub fn new(
        engine: Engine,
        runtimes: MicrogridRuntimes,
        script: PathBuf,
        python: PathBuf,
        ui: SocketAddr,
    ) -> Arc<Self> {
        let name = script.file_name().map_or_else(
            || script.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        Arc::new(Self {
            engine,
            runtimes,
            name,
            script,
            python,
            url: format!("http://{ui}"),
            state: Mutex::new(State::default()),
            ops: tokio::sync::Mutex::new(()),
            first_load: watch::channel(false).0,
        })
    }

    /// Watch the script's directory tree, then start the first child.
    pub async fn start(self: &Arc<Self>) {
        let dir = self
            .script
            .parent()
            .map_or_else(|| PathBuf::from("/"), PathBuf::from);
        let skipped = state_dirs(&dir, self.engine.state_dir());
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let me = self.clone();
        tokio::spawn(async move {
            script_tree(dir, skipped)
                .run(Some(ready_tx), |_| {
                    let me = me.clone();
                    async move { me.reload().await }
                })
                .await;
        });
        let _ = ready_rx.await;
        self.reload().await;
    }

    /// Wait until the first child's hello was answered or it failed
    /// to load.
    pub async fn first_load(&self) {
        let mut rx = self.first_load.subscribe();
        let _ = rx.wait_for(|done| *done).await;
    }

    /// Start a new child for the script, killing a pending one first.
    pub async fn reload(self: &Arc<Self>) {
        let _ops = self.ops.lock().await;
        let stale = {
            let mut state = self.state.lock();
            if state.shutting_down {
                return;
            }
            state.pending.take()
        };
        if let Some(stale) = stale {
            stale.child.kill().await;
        }
        let token = format!("{:032x}", rand::random::<u128>());
        let child = match Child::spawn(
            &self.python,
            &self.script,
            &self.url,
            &token,
            self.name.clone(),
        ) {
            Ok(child) => Arc::new(child),
            Err(e) => {
                let line = format!("could not start {}: {e}", self.python.display());
                log::error!(target: self.target().as_str(), "{line}");
                self.state.lock().last_reload =
                    Some(ReloadStatus::now(ReloadResult::FailedToLoad, vec![line]));
                self.first_load.send_replace(true);
                self.broadcast();
                return;
            }
        };
        {
            let mut state = self.state.lock();
            state.pending = Some(Slot {
                child: child.clone(),
                token: token.clone(),
                hello: Hello::NotSent,
                microgrids: Vec::new(),
            });
            state.last_reload = Some(ReloadStatus::now(ReloadResult::Pending, Vec::new()));
        }
        self.broadcast();
        let me = self.clone();
        let exiting = child.clone();
        let exited_token = token.clone();
        tokio::spawn(async move {
            let exit = exiting.wait_exit().await;
            me.child_exited(&exited_token, exit);
        });
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(HELLO_DEADLINE).await;
            me.hello_deadline(&token).await;
        });
    }

    /// A child ended: a pending one has failed to load, and the
    /// running one is logged.
    fn child_exited(&self, token: &str, exit: Exit) {
        let mut failed = false;
        {
            let mut state = self.state.lock();
            let state = &mut *state;
            if state.running.as_ref().is_some_and(|r| r.token == token) && !state.shutting_down {
                if exit.code == Some(0) {
                    log::info!(target: self.target().as_str(), "the script's run returned");
                } else {
                    log::warn!(target: self.target().as_str(), "the script {}", describe(exit));
                }
            }
            if let Some(pending) = state.pending.as_ref().filter(|p| p.token == token) {
                let note = match &pending.hello {
                    // The hello sees the exit and records it.
                    Hello::InFlight | Hello::Applied => None,
                    Hello::Refused(line) => Some(line.clone()),
                    Hello::TimedOut => Some(format!(
                        "the script sent no hello within {} s and was stopped",
                        HELLO_DEADLINE.as_secs()
                    )),
                    Hello::NotSent => Some(format!(
                        "the script {} before it sent its microgrids",
                        describe(exit)
                    )),
                };
                if let Some(note) = note {
                    log::warn!(target: self.target().as_str(), "the reload failed: {note}");
                    let mut output = pending.child.output();
                    output.push(note);
                    state.pending = None;
                    state.last_reload = Some(ReloadStatus::now(ReloadResult::FailedToLoad, output));
                    failed = true;
                }
            }
        }
        if failed {
            self.first_load.send_replace(true);
        }
        self.broadcast();
    }

    /// Kill the pending child named by `token` if it has not sent its
    /// hello by now, or sent one that was refused and lingers.
    async fn hello_deadline(&self, token: &str) {
        let child = {
            let mut state = self.state.lock();
            match state.pending.as_mut() {
                Some(p) if p.token == token => match p.hello {
                    Hello::NotSent => {
                        p.hello = Hello::TimedOut;
                        let secs = HELLO_DEADLINE.as_secs();
                        Some((p.child.clone(), format!("sent no hello within {secs} s")))
                    }
                    Hello::Refused(_) => {
                        let secs = HELLO_DEADLINE.as_secs();
                        let why = format!(
                            "still runs {secs} s after it started, though its microgrids were refused"
                        );
                        Some((p.child.clone(), why))
                    }
                    _ => None,
                },
                _ => None,
            }
        };
        if let Some((child, why)) = child {
            log::warn!(target: self.target().as_str(), "the script {why}; stopping it");
            child.kill().await;
        }
    }

    /// Check and apply the hello of the pending child; see the module
    /// docs. Runs to the end even if the request is dropped, so call
    /// it from its own task.
    pub async fn hello(
        self: Arc<Self>,
        body: serde_json::Value,
    ) -> Result<HelloAnswer, HelloError> {
        let started = Instant::now();
        let _ops = self.ops.lock().await;
        let token = body.get("token").and_then(|t| t.as_str()).unwrap_or("");
        let child = {
            let mut state = self.state.lock();
            match state.pending.as_mut() {
                Some(p) if p.token == token && p.hello == Hello::NotSent => {
                    p.hello = Hello::InFlight;
                    p.child.clone()
                }
                _ => return Err(HelloError::Token),
            }
        };
        let result = async { self.apply(&child, HelloRequest::parse(body)?).await }.await;
        let answer = {
            let mut state = self.state.lock();
            match result {
                Ok(ids) => {
                    log::info!(
                        target: self.target().as_str(),
                        "the script's microgrids {ids:?} are in place ({} ms)",
                        started.elapsed().as_millis()
                    );
                    // The in-flight child stays pending, unless the
                    // engine is shutting down.
                    if let Some(mut slot) = state.pending.take() {
                        slot.hello = Hello::Applied;
                        slot.microgrids.clone_from(&ids);
                        state.running = Some(slot);
                    }
                    state.last_reload = Some(ReloadStatus::now(ReloadResult::Ok, child.output()));
                    Ok(HelloAnswer {
                        microgrids: ids
                            .into_iter()
                            .map(|id| HelloMicrogrid {
                                id,
                                grpc_addr: self.runtimes.status(id).and_then(|v| v.serving_addr()),
                            })
                            .collect(),
                    })
                }
                Err(e) => {
                    // A live child prints the refusal, and its output
                    // is logged; one that has exited cannot.
                    let line = format!("the engine refused the script's microgrids: {e}");
                    if child.alive() {
                        if let Some(p) = state.pending.as_mut() {
                            p.hello = Hello::Refused(line.clone());
                        }
                    } else {
                        log::warn!(target: self.target().as_str(), "the reload failed: {line}");
                        state.pending = None;
                    }
                    let mut output = child.output();
                    output.push(line);
                    state.last_reload = Some(ReloadStatus::now(ReloadResult::FailedToLoad, output));
                    Err(e)
                }
            }
        };
        self.first_load.send_replace(true);
        self.broadcast();
        answer
    }

    async fn apply(&self, child: &Child, req: HelloRequest) -> Result<Vec<u64>, HelloError> {
        let doc = TopologyDoc::from_value(req.topology)
            .map_err(|e| HelloError::Invalid(e.message().to_string()))?;
        let create_lock = self.engine.create_lock();
        let _structural = create_lock.lock().await;
        let stop_old = || async {
            if !child.alive() {
                return Err("the script exited before its microgrids were put in place".to_string());
            }
            let old = self.state.lock().running.take();
            if let Some(old) = old {
                old.child.stop().await;
            }
            Ok(())
        };
        topology::apply_hello(&self.engine, &self.runtimes, &self.script, doc, stop_old)
            .await
            .map_err(HelloError::from)
    }

    /// Stop the children the way a reload stops the old one.
    pub async fn shutdown(&self) {
        let (pending, running) = {
            let mut state = self.state.lock();
            state.shutting_down = true;
            (state.pending.take(), state.running.take())
        };
        let stop = |slot: Option<Slot>| async move {
            if let Some(slot) = slot {
                slot.child.stop().await;
            }
        };
        tokio::join!(stop(pending), stop(running));
    }

    pub fn status(&self) -> ScriptStatus {
        let state = self.state.lock();
        let shown = match (&state.running, &state.pending) {
            (Some(running), _) => Some((running, false)),
            (None, Some(pending)) => Some((pending, true)),
            (None, None) => None,
        };
        ScriptStatus {
            script: Some(self.script.display().to_string()),
            child: shown.map(|(slot, first)| child_status(&slot.child, first)),
            last_reload: state.last_reload.clone(),
            output: shown.map_or_else(Vec::new, |(slot, _)| slot.child.output()),
            microgrids: shown.map_or_else(Vec::new, |(slot, _)| slot.microgrids.clone()),
        }
    }

    fn broadcast(&self) {
        let _ = self.engine.script_status.send(self.status());
    }
}

fn child_status(child: &Child, starting: bool) -> ChildStatus {
    let exit = child.try_exit_status();
    let state = match exit {
        None if starting => ChildState::Starting,
        None => ChildState::Running,
        Some(Exit { code: Some(0) }) => ChildState::Finished,
        Some(_) => ChildState::Crashed,
    };
    ChildStatus {
        state,
        exit_code: exit.and_then(|e| e.code),
        pid: child.pid(),
    }
}

fn describe(exit: Exit) -> String {
    match exit.code {
        Some(code) => format!("exited with code {code}"),
        None => "was ended by a signal".to_string(),
    }
}
