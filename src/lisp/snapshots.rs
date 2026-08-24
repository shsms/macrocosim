//! Per-microgrid snapshots on `Config`.
//!
//! A snapshot is a frozen copy of one microgrid's own file, kept
//! under `snapshots/{mg_id}/{name}.lisp`. Saving copies the file;
//! loading writes the copy back over it and reloads just that file,
//! so the microgrid comes back with the topology it had when the
//! snapshot was taken. Loading `as_id` instead lands the snapshot as
//! a SECOND, new microgrid ([`Config::load_as`]) and leaves the
//! original alone.
//!
//! Only managed microgrids can be snapshotted: an unmanaged file is
//! the author's, and writing one back would clobber hand-written
//! text. Live physics state (mid-flight setpoints, current SoC,
//! ramps) is not captured — the site re-spins from baseline once the
//! snapshotted topology is back in place.

use std::fs;
use std::path::{Path, PathBuf};

use super::{Config, microgrid_file};

/// Why a snapshot call failed. Typed rather than stringly so the
/// HTTP layer can pick a status code without reading messages.
#[derive(Debug)]
pub enum SnapshotError {
    /// The name is not a single safe file-name component.
    InvalidName(String),
    /// No such microgrid, or no such snapshot.
    NotFound(String),
    /// The microgrid has no managed file to copy or restore.
    Unmanaged(String),
    /// Filesystem trouble, or a reload that failed afterwards.
    Failed(String),
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnapshotError::InvalidName(m)
            | SnapshotError::NotFound(m)
            | SnapshotError::Unmanaged(m)
            | SnapshotError::Failed(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for SnapshotError {}

impl Config {
    /// Directory holding microgrid `mg_id`'s snapshots:
    /// `snapshots/{mg_id}/` under the state dir. Created lazily by
    /// the first save.
    pub fn snapshots_dir_for(&self, mg_id: u64) -> PathBuf {
        self.state_dir.join("snapshots").join(mg_id.to_string())
    }

    /// Copy microgrid `mg_id`'s file to
    /// `snapshots/{mg_id}/{name}.lisp` and return the snapshot's
    /// path. Refuses an unmanaged microgrid — there is no
    /// switchyard-owned file to freeze.
    pub fn save_snapshot_for(&self, mg_id: u64, name: &str) -> Result<PathBuf, SnapshotError> {
        let source = self.managed_file_of(mg_id)?;
        let dir = self.snapshots_dir_for(mg_id);
        let dest = sanitise_snapshot_path(&dir, name)?;
        fs::create_dir_all(&dir)
            .map_err(|e| SnapshotError::Failed(format!("create {}: {e}", dir.display())))?;
        fs::copy(&source, &dest).map_err(|e| {
            SnapshotError::Failed(format!(
                "copy {} to {}: {e}",
                source.display(),
                dest.display()
            ))
        })?;
        Ok(dest)
    }

    /// Restore snapshot `name` of microgrid `mg_id`.
    ///
    /// With `as_id` unset the snapshot is written back over the
    /// microgrid's own file and that file alone is reloaded, so the
    /// microgrid returns to its snapshotted structure in place.
    ///
    /// With `as_id` set nothing existing is touched: the snapshot is
    /// loaded as a NEW microgrid under that id (a copy of the site
    /// next to the original). Returns the id that was loaded, if any.
    pub fn load_snapshot_for(
        &self,
        mg_id: u64,
        name: &str,
        as_id: Option<u64>,
    ) -> Result<Option<u64>, SnapshotError> {
        let dir = self.snapshots_dir_for(mg_id);
        let snapshot = sanitise_snapshot_path(&dir, name)?;
        if !snapshot.exists() {
            return Err(SnapshotError::NotFound(format!(
                "snapshot {name:?} not found for microgrid {mg_id}"
            )));
        }
        if let Some(new_id) = as_id {
            let id = self
                .load_as(&snapshot, new_id)
                .map_err(SnapshotError::Failed)?;
            return Ok(Some(id));
        }
        let dest = self.managed_file_of(mg_id)?;
        let text = fs::read_to_string(&snapshot).map_err(|e| {
            SnapshotError::Failed(format!("cannot read {}: {e}", snapshot.display()))
        })?;
        // Restoring replaces the whole file, generated block
        // included, so it is a structural edit like any other and
        // takes the interpreter lock for the same reason `persist`
        // does: nothing may write the file between the block we read
        // for the undo stack and the bytes we put in its place.
        let mut ctx = self.ctx.borrow_mut();
        let replaced = super::undo::read_generated_block(&dest);
        microgrid_file::write_atomic(&dest, &text)
            .map_err(|e| SnapshotError::Failed(format!("write {}: {e}", dest.display())))?;
        // Our own write: the watcher must not treat it as a human
        // edit and reload the file a second time.
        self.record_self_write(&dest, &text);
        // A restore is undoable: the block it displaced goes on the
        // stack, exactly as a structural eval's would, so Undo walks
        // back out of the restore instead of skipping over it.
        if let Some(replaced) = replaced {
            self.push_undo_block(mg_id, replaced);
        }
        // Per file, not the whole world — restoring one microgrid's
        // snapshot leaves every other microgrid running.
        self.reload_file_locked(&mut ctx, &dest)
            .map_err(SnapshotError::Failed)?;
        Ok(Some(mg_id))
    }

    /// Names of microgrid `mg_id`'s snapshots, sorted lexically.
    /// Empty when it has none.
    pub fn list_snapshots_for(&self, mg_id: u64) -> Vec<String> {
        list_snapshots_in(&self.snapshots_dir_for(mg_id))
    }

    /// The managed file backing microgrid `mg_id`, or the reason
    /// there isn't one.
    fn managed_file_of(&self, mg_id: u64) -> Result<PathBuf, SnapshotError> {
        let registry = self.microgrids.lock();
        let entry = registry
            .get(&mg_id)
            .ok_or_else(|| SnapshotError::NotFound(format!("microgrid {mg_id} not registered")))?;
        if !entry.managed {
            return Err(SnapshotError::Unmanaged(format!(
                "microgrid {mg_id} is not managed by switchyard; adopt it first"
            )));
        }
        entry.source.clone().ok_or_else(|| {
            SnapshotError::Unmanaged(format!("microgrid {mg_id} has no file to snapshot"))
        })
    }
}

fn sanitise_snapshot_path(dir: &Path, name: &str) -> Result<PathBuf, SnapshotError> {
    // Reject anything that could escape the snapshots dir via `..`,
    // an absolute path, or path separators. We only accept a single
    // file-name component.
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.starts_with('.')
    {
        return Err(SnapshotError::InvalidName(format!(
            "invalid snapshot name {name:?}"
        )));
    }
    Ok(dir.join(format!("{name}.lisp")))
}

fn list_snapshots_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("lisp") {
                return None;
            }
            p.file_stem().and_then(|s| s.to_str()).map(|s| s.to_owned())
        })
        .collect();
    out.sort();
    out
}
