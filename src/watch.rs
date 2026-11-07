//! A debounced file watcher. Editors fire several events for one save
//! (write, close-after-write, a plugin's reformat), so events that
//! arrive within the debounce window of each other are collected into
//! one batch of touched paths, and the caller reacts once per batch.
//!
//! Two modes:
//!
//! - [`Mode::Files`] watches a set of files the caller names, each on
//!   its own. Only `Modify` events count. An atomic save (write a
//!   temporary file, rename it over) leaves the watch on the replaced
//!   inode, so the set is armed again every [`REARM`] and after every
//!   batch.
//! - [`Mode::Tree`] watches every directory under a root that passes
//!   the filter, adds the ones created later, and counts create,
//!   modify, remove and rename events on the paths its `relevant`
//!   predicate accepts. Other events keep the directory watches
//!   current but neither start a batch nor extend one, so a file
//!   written all the time beside the watched ones cannot hold a batch
//!   open. A directory watch follows an atomic save, so it needs no
//!   re-arm.
//!
//! The filter is asked before a path is registered, so a skipped
//! directory never uses one of the system's watches. A watcher that
//! cannot start logs why and ends; the rest of the binary keeps
//! running.

use std::collections::{BTreeSet, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};
use tokio::sync::{mpsc, oneshot};

/// How often [`Mode::Files`] arms its files again when nothing
/// happens. Two things go stale on their own: a file named after the
/// watcher started has no watch yet, and an atomic save leaves the
/// watch on an inode nothing writes to any more. Both fix themselves
/// within this interval; re-arming a handful of paths is cheap.
pub const REARM: Duration = Duration::from_secs(5);

/// The paths [`Mode::Files`] watches, asked again at each re-arm.
pub type FileSet = Box<dyn FnMut() -> Vec<PathBuf> + Send>;

/// Whether a path may be registered.
pub type Filter = Box<dyn Fn(&Path) -> bool + Send>;

/// What a [`Watch`] watches.
pub enum Mode {
    /// Exactly the files the set names, each watched on its own.
    Files(FileSet),
    /// Every directory under `root` that passes the filter. Only
    /// events on a path `relevant` accepts make up a batch.
    Tree { root: PathBuf, relevant: Filter },
}

/// A debounced watcher; see the module docs.
pub struct Watch {
    pub mode: Mode,
    /// Asked of each file ([`Mode::Files`]) or each directory below
    /// the root ([`Mode::Tree`]) before it is registered.
    pub filter: Filter,
    /// How long the events must go quiet before a batch ends.
    pub debounce: Duration,
}

/// The raw notify events the watcher thread sends.
type RawEvents = mpsc::UnboundedReceiver<notify::Result<notify::Event>>;

impl Watch {
    /// Watch until the watcher fails, calling `on_batch` with the
    /// paths each debounced batch touched. `ready` fires once the
    /// initial watches are armed, or once arming failed and the watch
    /// will not run: a change made after the signal is seen, or never
    /// will be, so a test can make it without sleeping.
    pub async fn run<F, Fut>(self, ready: Option<oneshot::Sender<()>>, on_batch: F)
    where
        F: FnMut(HashSet<PathBuf>) -> Fut,
        Fut: Future<Output = ()>,
    {
        let mut ready = ready;
        let mut armed_signal = move || {
            if let Some(tx) = ready.take() {
                let _ = tx.send(());
            }
        };
        // Unbounded, so the handler never blocks notify's one thread:
        // that thread also arms the watches a tree watch adds while
        // it reads events, and a full channel there would wait on a
        // reader that waits on it.
        let (tx, rx) = mpsc::unbounded_channel();
        // notify can fail at construction (out of inotify instances,
        // an IDE running alongside having used them up). That kills
        // this watch, but the rest of the binary should keep serving.
        let watcher = match RecommendedWatcher::new(
            move |res| {
                let _ = tx.send(res);
            },
            notify::Config::default(),
        ) {
            Ok(w) => w,
            Err(e) => {
                log::error!("watch: notify init failed: {e}; watching is off");
                armed_signal();
                return;
            }
        };
        let Self {
            mode,
            filter,
            debounce,
        } = self;
        match mode {
            Mode::Files(files) => {
                let files = FilesWatch {
                    watcher,
                    files,
                    filter,
                    armed: HashSet::new(),
                };
                files.run(rx, debounce, armed_signal, on_batch).await;
            }
            Mode::Tree { root, relevant } => {
                let tree = TreeWatch {
                    watcher,
                    filter,
                    relevant,
                    watched: BTreeSet::new(),
                };
                tree.run(root, rx, debounce, armed_signal, on_batch).await;
            }
        }
    }
}

/// Hand events to `collect` until no event it counts (returns `true`
/// for) has arrived for `debounce`. `false` when the watcher ended or
/// failed, and the watch is over.
async fn drain(
    rx: &mut RawEvents,
    debounce: Duration,
    mut collect: impl FnMut(&notify::Event) -> bool,
) -> bool {
    let mut until = tokio::time::Instant::now() + debounce;
    loop {
        match tokio::time::timeout_at(until, rx.recv()).await {
            Ok(Some(Ok(event))) => {
                if collect(&event) {
                    until = tokio::time::Instant::now() + debounce;
                }
            }
            Ok(Some(Err(e))) => {
                log::error!("watch error: {e:?}");
                return false;
            }
            Ok(None) => return false,
            Err(_) => return true,
        }
    }
}

struct FilesWatch {
    watcher: RecommendedWatcher,
    files: FileSet,
    filter: Filter,
    armed: HashSet<PathBuf>,
}

impl FilesWatch {
    async fn run<F, Fut>(
        mut self,
        mut rx: RawEvents,
        debounce: Duration,
        mut armed_signal: impl FnMut(),
        mut on_batch: F,
    ) where
        F: FnMut(HashSet<PathBuf>) -> Fut,
        Fut: Future<Output = ()>,
    {
        let wanted = self.rearm();
        if self.armed.is_empty() {
            if wanted == 0 {
                log::info!("watch: nothing to watch; hot-reload idle");
            } else {
                log::error!("watch: no watch could be registered; hot-reload disabled");
            }
            armed_signal();
            return;
        }
        armed_signal();
        loop {
            let first = match tokio::time::timeout(REARM, rx.recv()).await {
                Ok(Some(Ok(event))) => event,
                Ok(Some(Err(e))) => {
                    log::error!("watch error: {e:?}");
                    return;
                }
                Ok(None) => return,
                Err(_) => {
                    self.rearm();
                    continue;
                }
            };
            let mut touched = HashSet::new();
            collect_modified(&first, &mut touched);
            if !drain(&mut rx, debounce, |e| collect_modified(e, &mut touched)).await {
                return;
            }
            if !touched.is_empty() {
                on_batch(touched).await;
            }
            // Re-arm after every batch, reacted to or not: the
            // reaction can change what there is to watch, and the
            // events left out may have come from the replaced file
            // the watch was following.
            self.rearm();
        }
    }

    /// Point the watcher at exactly the files worth watching now.
    /// Returns how many were wanted, so a caller can tell "nothing to
    /// watch" from "every watch failed to register".
    fn rearm(&mut self) -> usize {
        let want: HashSet<PathBuf> = (self.files)()
            .into_iter()
            .map(|p| p.canonicalize().unwrap_or(p))
            .filter(|p| p.exists() && (self.filter)(p))
            .collect();
        for gone in self.armed.difference(&want) {
            let _ = self.watcher.unwatch(gone);
        }
        // Every wanted path is armed again, the armed ones included:
        // an atomic save replaces the file, leaving the old watch on
        // an inode nothing writes to any more.
        let mut next = HashSet::new();
        for path in &want {
            match self.watcher.watch(path, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    next.insert(path.clone());
                }
                Err(e) => log::warn!("watch {}: {}", path.display(), e),
            }
        }
        self.armed = next;
        want.len()
    }
}

/// Add the paths of a Modify event to `touched`, and say whether it
/// was one. Anything else (a create, a remove, an access) is not an
/// edit of a watched file.
fn collect_modified(event: &notify::Event, touched: &mut HashSet<PathBuf>) -> bool {
    let modified = matches!(event.kind, notify::EventKind::Modify(_));
    if modified {
        touched.extend(event.paths.iter().cloned());
    }
    modified
}

struct TreeWatch {
    watcher: RecommendedWatcher,
    filter: Filter,
    relevant: Filter,
    watched: BTreeSet<PathBuf>,
}

impl TreeWatch {
    async fn run<F, Fut>(
        mut self,
        root: PathBuf,
        mut rx: RawEvents,
        debounce: Duration,
        mut armed_signal: impl FnMut(),
        mut on_batch: F,
    ) where
        F: FnMut(HashSet<PathBuf>) -> Fut,
        Fut: Future<Output = ()>,
    {
        let root = root.canonicalize().unwrap_or(root);
        self.add_tree(&root, true);
        armed_signal();
        if self.watched.is_empty() {
            log::error!("watch: {} could not be watched", root.display());
            return;
        }
        loop {
            let first = match rx.recv().await {
                Some(Ok(event)) => event,
                Some(Err(e)) => {
                    log::error!("watch error: {e:?}");
                    return;
                }
                None => return,
            };
            let mut touched = HashSet::new();
            if !self.note_event(&first, &mut touched) {
                continue;
            }
            if !drain(&mut rx, debounce, |e| self.note_event(e, &mut touched)).await {
                return;
            }
            on_batch(touched).await;
        }
    }

    /// Whether `event` is a create, modify or remove that names a
    /// relevant path; those paths go into `touched`. Only such events
    /// start or extend a batch, a repeat of a path already there
    /// included. Whether or not it counts, the event goes through
    /// [`Self::follow`] first.
    fn note_event(&mut self, event: &notify::Event, touched: &mut HashSet<PathBuf>) -> bool {
        self.follow(event);
        if !counts(&event.kind) {
            return false;
        }
        let relevant: Vec<PathBuf> = event
            .paths
            .iter()
            .filter(|p| (self.relevant)(p))
            .cloned()
            .collect();
        let any = !relevant.is_empty();
        touched.extend(relevant);
        any
    }

    /// Watch a directory created or moved in, and forget one removed.
    fn follow(&mut self, event: &notify::Event) {
        use notify::EventKind::{Create, Modify, Remove};
        use notify::event::ModifyKind::Name;
        match event.kind {
            Create(_) | Modify(Name(_)) => {
                for path in &event.paths {
                    let watched_parent = path.parent().is_some_and(|p| self.watched.contains(p));
                    // A symlinked directory is not followed, as at
                    // the start: it may lead anywhere.
                    let is_dir = std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir());
                    if watched_parent && is_dir && (self.filter)(path) {
                        self.add_tree(path, false);
                    }
                }
            }
            _ => {}
        }
        if matches!(event.kind, Remove(_) | Modify(Name(_))) {
            for path in &event.paths {
                if !path.exists() {
                    let gone: Vec<PathBuf> = self
                        .watched
                        .iter()
                        .filter(|w| w.starts_with(path))
                        .cloned()
                        .collect();
                    for w in gone {
                        let _ = self.watcher.unwatch(&w);
                        self.watched.remove(&w);
                    }
                }
            }
        }
    }

    /// Watch `dir` and every directory below it that passes the
    /// filter (`dir` itself is asked too unless it is the root). At
    /// the system's limit, warn naming what is left unwatched and
    /// keep the watches there are.
    fn add_tree(&mut self, dir: &Path, is_root: bool) {
        let mut unwatched = Vec::new();
        let start = dir.to_path_buf();
        let mut stack = vec![start.clone()];
        while let Some(dir) = stack.pop() {
            if !(is_root && dir == start) && !(self.filter)(&dir) {
                continue;
            }
            if self.watched.contains(&dir) {
                continue;
            }
            match self.watcher.watch(&dir, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    self.watched.insert(dir.clone());
                }
                Err(e) => {
                    unwatched.push((dir, e));
                    continue;
                }
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    stack.push(entry.path());
                }
            }
        }
        if unwatched.is_empty() {
            return;
        }
        let at_limit = unwatched
            .iter()
            .any(|(_, e)| matches!(e.kind, notify::ErrorKind::MaxFilesWatch));
        let names: Vec<String> = unwatched
            .iter()
            .map(|(d, _)| d.display().to_string())
            .collect();
        if at_limit {
            log::warn!(
                "watch: the system's limit on watches is reached (fs.inotify.max_user_watches); \
                 not watching {}",
                names.join(", ")
            );
        } else {
            for (dir, e) in &unwatched {
                log::warn!("watch {}: {e}", dir.display());
            }
        }
    }
}

/// Whether an event in a watched tree is a change: a create, a modify
/// (a rename included) or a remove.
fn counts(kind: &notify::EventKind) -> bool {
    use notify::EventKind::{Create, Modify, Remove};
    matches!(kind, Create(_) | Modify(_) | Remove(_))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    type Batches = Arc<Mutex<Vec<HashSet<PathBuf>>>>;

    /// Start a tree watch on `root` that skips every directory named
    /// "skip"; returns the batches it sees once it is armed.
    async fn tree(root: &Path) -> Batches {
        tree_of(root, Box::new(|_: &Path| true)).await
    }

    /// [`tree`] batching only the paths `relevant` accepts.
    async fn tree_of(root: &Path, relevant: Filter) -> Batches {
        let batches: Batches = Arc::default();
        let seen = batches.clone();
        let (ready_tx, ready_rx) = oneshot::channel();
        let watch = Watch {
            mode: Mode::Tree {
                root: root.to_path_buf(),
                relevant,
            },
            filter: Box::new(|p: &Path| p.file_name().is_none_or(|n| n != "skip")),
            debounce: Duration::from_millis(100),
        };
        tokio::spawn(watch.run(Some(ready_tx), move |touched| {
            seen.lock().unwrap().push(touched);
            async {}
        }));
        ready_rx.await.unwrap();
        batches
    }

    /// Wait until a batch names `path`.
    async fn wait_for(batches: &Batches, path: &Path) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if batches.lock().unwrap().iter().any(|b| b.contains(path)) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no batch named {} in {:?}",
                path.display(),
                batches.lock().unwrap()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn a_tree_sees_new_files_new_directories_and_atomic_saves() {
        let dir = tempfile::TempDir::with_prefix("mc-watch-").unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("skip")).unwrap();
        let batches = tree(&root).await;

        let created = root.join("plant.py");
        std::fs::write(&created, "a").unwrap();
        wait_for(&batches, &created).await;

        // A save by rename keeps being seen: the watch is on the
        // directory, not on the replaced inode.
        for round in 0..2 {
            let tmp = root.join(".plant.py.tmp");
            std::fs::write(&tmp, format!("b{round}")).unwrap();
            batches.lock().unwrap().clear();
            std::fs::rename(&tmp, &created).unwrap();
            wait_for(&batches, &created).await;
        }

        // A directory made after the start is watched too.
        let sub = root.join("lib");
        std::fs::create_dir(&sub).unwrap();
        wait_for(&batches, &sub).await;
        let helper = sub.join("helper.py");
        std::fs::write(&helper, "c").unwrap();
        wait_for(&batches, &helper).await;

        // A skipped directory has no watch.
        batches.lock().unwrap().clear();
        std::fs::write(root.join("skip").join("x.py"), "d").unwrap();
        let marker = root.join("marker.py");
        std::fs::write(&marker, "e").unwrap();
        wait_for(&batches, &marker).await;
        assert!(
            !batches
                .lock()
                .unwrap()
                .iter()
                .flatten()
                .any(|p| p.starts_with(root.join("skip"))),
            "{:?}",
            batches.lock().unwrap()
        );
    }

    /// A file the tree does not batch, written more often than the
    /// debounce window, does not hold back a save of one it does.
    #[tokio::test]
    async fn steady_writes_do_not_hold_a_batch_open() {
        let dir = tempfile::TempDir::with_prefix("mc-watch-").unwrap();
        let root = dir.path().canonicalize().unwrap();
        let python = |p: &Path| p.extension().is_some_and(|e| e == "py");
        let batches = tree_of(&root, Box::new(python)).await;
        let log = root.join("run.log");
        let writing = tokio::spawn(async move {
            for i in 0..150 {
                std::fs::write(&log, format!("{i}")).unwrap();
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let started = tokio::time::Instant::now();
        let saved = root.join("plant.py");
        std::fs::write(&saved, "a").unwrap();
        wait_for(&batches, &saved).await;
        let waited = started.elapsed();
        writing.abort();
        assert!(waited < Duration::from_millis(900), "{waited:?}");
        assert!(
            !batches.lock().unwrap().iter().flatten().any(|p| !python(p)),
            "{:?}",
            batches.lock().unwrap()
        );
    }

    /// A burst of new directories, each armed while the burst goes
    /// on, does not stall the watch: a save after it is seen.
    #[tokio::test]
    async fn a_burst_of_new_directories_does_not_stall_the_watch() {
        let dir = tempfile::TempDir::with_prefix("mc-watch-").unwrap();
        let root = dir.path().canonicalize().unwrap();
        // The watch runs on a thread and runtime of its own, so a
        // stalled watch fails the wait below instead of hanging it.
        let batches: Batches = Arc::default();
        let seen = batches.clone();
        let (ready_tx, ready_rx) = oneshot::channel();
        let watch = Watch {
            mode: Mode::Tree {
                root: root.clone(),
                relevant: Box::new(|_: &Path| true),
            },
            filter: Box::new(|_: &Path| true),
            debounce: Duration::from_millis(100),
        };
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let thread = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    tokio::select! {
                        () = watch.run(Some(ready_tx), move |touched| {
                            seen.lock().unwrap().push(touched);
                            async {}
                        }) => {}
                        _ = stop_rx => {}
                    }
                });
        });
        // Stops the watch and joins its thread when the test ends,
        // also when it fails.
        struct StopOnDrop(Option<(oneshot::Sender<()>, std::thread::JoinHandle<()>)>);
        impl Drop for StopOnDrop {
            fn drop(&mut self) {
                if let Some((tx, thread)) = self.0.take() {
                    let _ = tx.send(());
                    let _ = thread.join();
                }
            }
        }
        let _stop = StopOnDrop(Some((stop_tx, thread)));
        ready_rx.await.unwrap();
        let burst = root.clone();
        tokio::task::spawn_blocking(move || {
            for i in 0..300 {
                let d = burst.join(format!("d{i}"));
                std::fs::create_dir(&d).unwrap();
                std::fs::write(d.join("f.py"), "x = 1\n").unwrap();
            }
        })
        .await
        .unwrap();
        let late = root.join("late.py");
        std::fs::write(&late, "x = 2\n").unwrap();
        wait_for(&batches, &late).await;
    }

    /// A symlinked directory made after the start is not followed.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_directory_is_not_followed() {
        let dir = tempfile::TempDir::with_prefix("mc-watch-").unwrap();
        let elsewhere = tempfile::TempDir::with_prefix("mc-watch-out-").unwrap();
        let root = dir.path().canonicalize().unwrap();
        let batches = tree(&root).await;
        let link = root.join("link");
        std::os::unix::fs::symlink(elsewhere.path(), &link).unwrap();
        wait_for(&batches, &link).await;
        batches.lock().unwrap().clear();
        std::fs::write(elsewhere.path().join("x.py"), "a").unwrap();
        let marker = root.join("marker.py");
        std::fs::write(&marker, "b").unwrap();
        wait_for(&batches, &marker).await;
        assert!(
            !batches
                .lock()
                .unwrap()
                .iter()
                .flatten()
                .any(|p| p.starts_with(&link) || p.ends_with("x.py")),
            "{:?}",
            batches.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn files_count_only_modify_events_and_follow_a_replaced_file() {
        let dir = tempfile::TempDir::with_prefix("mc-watch-").unwrap();
        let root = dir.path().canonicalize().unwrap();
        let file = root.join("site.lisp");
        std::fs::write(&file, "a").unwrap();
        let batches: Batches = Arc::default();
        let seen = batches.clone();
        let (ready_tx, ready_rx) = oneshot::channel();
        let named = file.clone();
        let watch = Watch {
            mode: Mode::Files(Box::new(move || vec![named.clone()])),
            filter: Box::new(|_: &Path| true),
            debounce: Duration::from_millis(100),
        };
        tokio::spawn(watch.run(Some(ready_tx), move |touched| {
            seen.lock().unwrap().push(touched);
            async {}
        }));
        ready_rx.await.unwrap();
        std::fs::write(&file, "b").unwrap();
        wait_for(&batches, &file).await;
        // The batch re-armed the watch, so a write after an atomic
        // save is seen.
        let tmp = root.join("site.lisp.tmp");
        std::fs::write(&tmp, "c").unwrap();
        std::fs::rename(&tmp, &file).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        batches.lock().unwrap().clear();
        std::fs::write(&file, "d").unwrap();
        tokio::time::timeout(REARM * 2, wait_for(&batches, &file))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn nothing_to_watch_still_signals_ready() {
        let (ready_tx, ready_rx) = oneshot::channel();
        let watch = Watch {
            mode: Mode::Files(Box::new(Vec::new)),
            filter: Box::new(|_: &Path| true),
            debounce: Duration::from_millis(100),
        };
        watch.run(Some(ready_tx), |_| async {}).await;
        ready_rx.await.unwrap();
    }
}
