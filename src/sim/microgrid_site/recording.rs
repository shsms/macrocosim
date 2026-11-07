//! Recording lifecycle + reporting on a `MicrogridSite`.
//!
//! Three concerns live here:
//!
//! - The recording journal: start / stop / event / events_since /
//!   summary / report; a rebuild ends a recording and keeps its
//!   report for the stop.
//! - CSV sink open / close — paired with start / stop so the recorded
//!   files match the journal window.
//! - The report shapes and their SoC-stats helper.
//!
//! All persistent state lives in `MicrogridSiteInner`; this file only
//! adds methods.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::RwLockWriteGuard;
use serde::Serialize;

use crate::sim::component::SimulatedComponent;
use crate::sim::recording::{EndedBy, RecordingEvent, RecordingJournal};
use crate::sim::recording_csv::{CsvSink, CsvSinks};

use super::{Components, MicrogridSite};

/// Snapshot of `RecordingJournal` lifecycle state for
/// `/api/mg/{mg}/recording`. Excludes the events themselves — those
/// live behind a paginated `recording/events` endpoint with a
/// `since=` cursor.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RecordingStatus {
    pub name: Option<String>,
    #[serde(serialize_with = "crate::timefmt::serialize_opt_rfc3339")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(serialize_with = "crate::timefmt::serialize_opt_rfc3339")]
    pub ended_at: Option<DateTime<Utc>>,
    pub elapsed_s: f64,
    pub event_count: usize,
    /// One past the highest event id ever recorded. Stable cursor for
    /// `recording/events?since=N` — clients pass this back unchanged
    /// to mean "anything newer than what I last saw".
    pub next_event_id: u64,
    /// Lowest event id still retained in the ring. Clients compare
    /// their `since` cursor against this: if
    /// `since < earliest_event_id` they're polling into a window that
    /// has already been evicted, so some events were missed.
    pub earliest_event_id: u64,
}

/// The report of a recording, with what ended it.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RecordingReport {
    /// The recording the report belongs to (`None` before any start).
    pub name: Option<String>,
    /// Seconds from the start to the end, or to now while it runs.
    pub elapsed_s: f64,
    /// `stop` or `rebuild` once the recording ended; `None` while it
    /// runs.
    pub ended_by: Option<EndedBy>,
    /// Maximum active power seen on the loopback's `grid_power`
    /// formula stream since the recording started — the site's import
    /// peak, resampled at ~1 Hz.
    pub peak_grid_w: f64,
    /// Maximum |reactive power| seen on the `grid_reactive_power`
    /// stream since the recording started — the Q twin of
    /// `peak_grid_w`, tracked by magnitude rather than signed max (Q
    /// swings both ways).
    pub peak_grid_var: f64,
    /// Power factor `|P| / sqrt(P^2 + Q^2)` at the grid connection
    /// point at the instant `peak_grid_var` was recorded — paired
    /// against the last P sample, not an independently-peaked P.
    /// `None` before any pairable PQ sample, or when both P and Q
    /// were 0 at that instant.
    pub site_pf_at_reactive_peak: Option<f64>,
    pub total_battery_charged_wh: f64,
    pub total_battery_discharged_wh: f64,
    pub total_pv_produced_wh: f64,
    pub per_battery: Vec<BatteryEnergy>,
    pub per_pv: Vec<PvEnergy>,
    /// Stats over the *current* SoC of every registered battery.
    /// Computed on each report fetch — cheap O(N) over a handful of
    /// batteries. None when no batteries are registered.
    pub soc_stats: Option<SocStats>,
    /// Per-15-minute UTC-aligned window average of grid active power.
    /// Sorted oldest-first.
    pub grid_window_averages: Vec<WindowAverage>,
}

/// Why a recording call was refused.
#[derive(Debug)]
pub(crate) enum RecordingError {
    /// A recording runs already.
    Busy,
    /// No recording runs (and, for a stop, no report waits).
    NotRunning,
    /// A rebuild ended the recording, and its report waits for the
    /// stop.
    EndedByRebuild,
    /// The CSV directory or a file in it could not be made.
    Csv(std::io::Error),
}

/// Where a recording named `name` that starts at `now` writes its CSV
/// files: `<state_dir>/recordings/<mg>/<name>-<UTC timestamp>/`. The
/// name keeps ASCII letters, digits, `-` and `_`, every other
/// character becomes `_`, and it is cut to 64 characters; the
/// timestamp has no colons, so the name is valid on Windows too.
pub(crate) fn recording_dir(state_dir: &Path, mg: u64, name: &str, now: DateTime<Utc>) -> PathBuf {
    let name: String = name
        .chars()
        .take(64)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    state_dir
        .join("recordings")
        .join(mg.to_string())
        .join(format!("{name}-{}", now.format("%Y%m%dT%H%M%SZ")))
}

/// Make `dir`, whose parent may not exist yet, as a new directory;
/// when it exists, the first free `<dir>-2`, `<dir>-3`, … instead.
/// Returns the directory made.
fn new_dir(dir: &Path) -> std::io::Result<PathBuf> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut candidate = dir.to_path_buf();
    for n in 2.. {
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let mut name = dir.file_name().unwrap_or_default().to_os_string();
                name.push(format!("-{n}"));
                candidate = dir.with_file_name(name);
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!("the counter runs until a name is free")
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BatteryEnergy {
    pub id: u64,
    pub charge_wh: f64,
    pub discharge_wh: f64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PvEnergy {
    pub id: u64,
    pub produced_wh: f64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SocStats {
    /// Arithmetic mean of every battery's current SoC.
    pub mean_pct: f64,
    /// Median (lower of the two middle values for an even count).
    pub median_pct: f64,
    /// Mode bucketed to integer percent. If multiple buckets tie,
    /// returns the lowest. None for an empty set.
    pub mode_pct: Option<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WindowAverage {
    #[serde(serialize_with = "crate::timefmt::serialize_rfc3339")]
    pub window_start: DateTime<Utc>,
    pub avg_w: f64,
}

/// Compute mean / median / integer-bucketed mode over a battery SoC
/// sample set. Returns `None` for an empty input.
fn compute_soc_stats(socs: &[f32]) -> Option<SocStats> {
    if socs.is_empty() {
        return None;
    }
    let mean_pct = socs.iter().map(|v| *v as f64).sum::<f64>() / socs.len() as f64;
    let mut sorted: Vec<f32> = socs.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_pct = sorted[sorted.len() / 2 - usize::from(sorted.len().is_multiple_of(2))] as f64;
    // Mode: integer-bucketed, lowest-bucket on tie.
    let mut histogram = [0u32; 101];
    for v in socs {
        let bucket = v.clamp(0.0, 100.0).round() as usize;
        histogram[bucket] += 1;
    }
    // Pick the lowest bucket on a count tie. `max_by_key` keeps the
    // LAST max seen; iterate ascending and update only on strictly
    // greater so the lowest bucket wins.
    let mut mode_pct: u8 = 0;
    let mut best_count: u32 = 0;
    for (idx, count) in histogram.iter().enumerate() {
        if *count > best_count {
            best_count = *count;
            mode_pct = idx as u8;
        }
    }
    Some(SocStats {
        mean_pct,
        median_pct,
        mode_pct: Some(mode_pct),
    })
}

/// The CSV sinks of a recording, opened and not yet installed.
#[derive(Default)]
struct OpenedCsv {
    dir: PathBuf,
    telemetry: CsvSinks,
    setpoints: CsvSinks,
    bounds: CsvSinks,
    reactive_bounds: CsvSinks,
}

impl MicrogridSite {
    /// Open fresh CSV sinks per registered component under `dir`: a
    /// telemetry file for every component, plus a received-setpoints
    /// and an effective-bounds file for each component that reports
    /// an active-power envelope (the ones a control app commands),
    /// plus an effective-reactive-bounds file for each component that
    /// reports a Q envelope (`bounds_of(id, Reactive)` is `Some` — a
    /// different set: an inverter has one, the battery behind it
    /// doesn't). Nothing is installed: see [`Self::recording_start`].
    ///
    /// `dir` is made new: when it exists already (a recording of the
    /// same name started in the same second), `-2`, `-3`, … is added
    /// to its name. The files are opened for the components installed
    /// now, which `components` returns; a failure removes the
    /// directory again.
    fn recording_open_csv(&self, dir: &Path) -> std::io::Result<(OpenedCsv, Components)> {
        let dir = new_dir(dir)?;
        let components = self.inner.components.read().clone();
        match self.recording_open_csv_in(&dir, &components) {
            Ok(opened) => Ok((opened, components)),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                Err(e)
            }
        }
    }

    fn recording_open_csv_in(
        &self,
        dir: &Path,
        components: &[Arc<dyn SimulatedComponent>],
    ) -> std::io::Result<OpenedCsv> {
        let mut opened = OpenedCsv {
            dir: dir.to_path_buf(),
            ..OpenedCsv::default()
        };
        for c in components.iter() {
            opened
                .telemetry
                .insert(c.id(), CsvSink::open(dir, c.id(), c.category())?);
            if self
                .bounds_of(c.id(), crate::timeout_tracker::SetpointAxis::Active)
                .is_some()
            {
                opened
                    .setpoints
                    .insert(c.id(), CsvSink::open_setpoints(dir, c.id())?);
                opened
                    .bounds
                    .insert(c.id(), CsvSink::open_bounds(dir, c.id())?);
            }
            if self
                .bounds_of(c.id(), crate::timeout_tracker::SetpointAxis::Reactive)
                .is_some()
            {
                opened
                    .reactive_bounds
                    .insert(c.id(), CsvSink::open_reactive_bounds(dir, c.id())?);
            }
        }
        Ok(opened)
    }

    /// Replace the CSV sinks with `opened`, dropping (closing) the
    /// prior recording's.
    fn recording_install_csv(&self, opened: OpenedCsv) {
        *self.inner.recording_csv.write() = opened.telemetry;
        *self.inner.recording_setpoints_csv.write() = opened.setpoints;
        *self.inner.recording_bounds_csv.write() = opened.bounds;
        *self.inner.recording_reactive_bounds_csv.write() = opened.reactive_bounds;
        *self.inner.recording_csv_dir.write() = Some(opened.dir);
    }

    /// The directory the active/most-recent recording wrote to, plus
    /// the `.csv` file names in it (sorted). `None` if nothing has
    /// been recorded. Used by the UI to offer the CSVs for download.
    pub(crate) fn recording_csv_listing(&self) -> Option<(std::path::PathBuf, Vec<String>)> {
        let dir = self.inner.recording_csv_dir.read().clone()?;
        let mut files: Vec<String> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.ends_with(".csv").then_some(name)
            })
            .collect();
        files.sort();
        Some((dir, files))
    }

    /// Drop every active CSV sink (telemetry, setpoints, bounds).
    /// Each underlying `BufWriter` flushes on drop. Returns the total
    /// file count closed.
    pub(crate) fn recording_close_csv(&self) -> usize {
        let mut count = 0;
        for sinks in [
            &self.inner.recording_csv,
            &self.inner.recording_setpoints_csv,
            &self.inner.recording_bounds_csv,
            &self.inner.recording_reactive_bounds_csv,
        ] {
            let mut g = sinks.write();
            count += g.len();
            g.clear();
        }
        count
    }

    /// Does a recording run right now?
    #[cfg(test)]
    pub(crate) fn recording_is_running(&self) -> bool {
        self.inner.recording.read().is_running()
    }

    /// Snapshot of the recording's lifecycle for `recording`.
    pub(crate) fn recording_status(&self, now: DateTime<Utc>) -> RecordingStatus {
        let g = self.inner.recording.read();
        RecordingStatus {
            name: g.name.clone(),
            started_at: g.started_at,
            ended_at: g.ended_at,
            elapsed_s: g.elapsed_s(now),
            event_count: g.event_count(),
            next_event_id: g.next_event_id(),
            earliest_event_id: g.earliest_event_id(),
        }
    }

    /// Pull events with id >= `since` (callers pass the cursor from
    /// `next_event_id`), capped at `limit`. Used by
    /// `recording/events`.
    pub(crate) fn recording_events_since(&self, since: u64, limit: usize) -> Vec<RecordingEvent> {
        self.inner.recording.read().events_since(since, limit)
    }

    /// Aggregate metrics for `recording/report`: the running
    /// recording's so far, or the last one's once a stop or a rebuild
    /// has ended it.
    pub(crate) fn recording_report(&self, now: DateTime<Utc>) -> RecordingReport {
        let soc_stats = self.soc_stats();
        journal_report(&self.inner.recording.read(), now, soc_stats)
    }

    /// Stats over the current SoC of every registered battery. Out of
    /// band of the journal because it is current state, not an
    /// accumulator.
    fn soc_stats(&self) -> Option<SocStats> {
        use crate::sim::Category;
        let mut socs: Vec<f32> = Vec::new();
        for c in self.inner.components.read().iter() {
            if c.category() == Category::Battery
                && let Some(s) = c.telemetry(self).soc_pct
            {
                socs.push(s);
            }
        }
        compute_soc_stats(&socs)
    }

    /// Start a recording named `name` at `now`, with CSV files in
    /// `csv_dir` when one is given. Refused while a recording runs.
    /// Drops a report a rebuild kept for an earlier one.
    ///
    /// The CSV files are opened before the journal's write guard is
    /// taken, so the sampler and the loopback that read the journal
    /// do not wait on the disk. Under the guard the start checks
    /// again that no recording runs, then installs the files and
    /// starts the journal, so two starts, or a start and a stop,
    /// cannot interleave. When a rebuild replaced the components
    /// between the open and the guard, the files are opened again
    /// under the guard. The journal lock is the outer one here:
    /// nothing that holds a component, gateway or CSV lock takes the
    /// journal's. Opening files blocks, so an async caller runs this
    /// off its runtime's workers.
    pub(crate) fn recording_start(
        &self,
        name: String,
        csv_dir: Option<&Path>,
        now: DateTime<Utc>,
    ) -> Result<(), RecordingError> {
        if self.inner.recording.read().is_running() {
            return Err(RecordingError::Busy);
        }
        let opened = csv_dir
            .map(|dir| self.recording_open_csv(dir))
            .transpose()
            .map_err(RecordingError::Csv)?;
        self.recording_start_with(name, csv_dir, opened, now)
    }

    /// The second half of [`Self::recording_start`]: start the
    /// journal with the CSV files `opened` before the guard was
    /// taken, opened again if the components changed since.
    fn recording_start_with(
        &self,
        name: String,
        csv_dir: Option<&Path>,
        mut opened: Option<(OpenedCsv, Components)>,
        now: DateTime<Utc>,
    ) -> Result<(), RecordingError> {
        let mut journal = self.inner.recording.write();
        if journal.is_running() {
            if let Some((opened, _)) = opened {
                let _ = std::fs::remove_dir_all(&opened.dir);
            }
            return Err(RecordingError::Busy);
        }
        // A rebuild between the open and the guard installed other
        // components: open their files instead. A rebuild holds the
        // guard while it installs, so the list holds still now.
        if let (Some((first, components)), Some(dir)) = (&opened, csv_dir)
            && !self.is_current(components)
        {
            let _ = std::fs::remove_dir_all(&first.dir);
            opened = Some(self.recording_open_csv(dir).map_err(RecordingError::Csv)?);
        }
        if let Some((opened, _)) = opened {
            self.recording_install_csv(opened);
        }
        journal.start(name, now);
        Ok(())
    }

    /// Stop the running recording at `now` and close its CSV files;
    /// its report says `ended_by: stop`. When a rebuild ended the
    /// recording, answer the report it kept instead, once.
    pub(crate) fn recording_stop(
        &self,
        now: DateTime<Utc>,
    ) -> Result<RecordingReport, RecordingError> {
        let soc_stats = self.soc_stats();
        let mut journal = self.inner.recording.write();
        if let Some(report) = journal.rebuild_report.take() {
            return Ok(report);
        }
        if !journal.is_running() {
            return Err(RecordingError::NotRunning);
        }
        journal.stop(now);
        journal.ended_by = Some(EndedBy::Stop);
        self.recording_close_csv();
        Ok(journal_report(&journal, now, soc_stats))
    }

    /// Stop a running recording here, because the microgrid is being
    /// rebuilt: close its CSV files and build its report (`ended_by:
    /// rebuild`) off the components before they are replaced. The
    /// report is kept until the script's next stop call, which
    /// answers it. Returns the journal's write guard, which the
    /// rebuild holds until the new components are in, so no recording
    /// starts in between. Stops nothing when no recording runs.
    pub(crate) fn recording_end_for_rebuild(
        &self,
        now: DateTime<Utc>,
    ) -> RwLockWriteGuard<'_, RecordingJournal> {
        let soc_stats = self.soc_stats();
        let mut journal = self.inner.recording.write();
        if journal.is_running() {
            journal.stop(now);
            journal.ended_by = Some(EndedBy::Rebuild);
            self.recording_close_csv();
            journal.rebuild_report = Some(journal_report(&journal, now, soc_stats));
        }
        journal
    }

    /// Add an event to the running recording. Returns its id.
    /// Refused with [`RecordingError::EndedByRebuild`] when a rebuild
    /// ended the recording and its stop has not come yet.
    pub(crate) fn recording_event(
        &self,
        kind: String,
        payload: String,
        now: DateTime<Utc>,
    ) -> Result<u64, RecordingError> {
        let mut journal = self.inner.recording.write();
        if !journal.is_running() {
            return Err(if journal.rebuild_report.is_some() {
                RecordingError::EndedByRebuild
            } else {
                RecordingError::NotRunning
            });
        }
        Ok(journal.push_event(kind, payload, now))
    }
}

/// The report of `g` at `now`, with `soc_stats` read beside it. Every
/// journal figure comes from the one guard.
fn journal_report(
    g: &RecordingJournal,
    now: DateTime<Utc>,
    soc_stats: Option<SocStats>,
) -> RecordingReport {
    let mut total_charged = 0.0;
    let mut total_discharged = 0.0;
    let per_battery: Vec<BatteryEnergy> = g
        .per_battery()
        .iter()
        .map(|(id, b)| {
            total_charged += b.charge_wh;
            total_discharged += b.discharge_wh;
            BatteryEnergy {
                id: *id,
                charge_wh: b.charge_wh,
                discharge_wh: b.discharge_wh,
            }
        })
        .collect();
    let mut total_pv = 0.0;
    let per_pv: Vec<PvEnergy> = g
        .per_pv()
        .iter()
        .map(|(id, p)| {
            total_pv += p.produced_wh;
            PvEnergy {
                id: *id,
                produced_wh: p.produced_wh,
            }
        })
        .collect();
    let grid_window_averages: Vec<WindowAverage> = g
        .window_avgs()
        .iter()
        .map(|(secs, (sum, count))| WindowAverage {
            window_start: DateTime::<Utc>::from_timestamp(*secs, 0).unwrap_or_else(Utc::now),
            avg_w: if *count > 0 {
                *sum / (*count as f64)
            } else {
                0.0
            },
        })
        .collect();
    // The peak-|Q| pair also carries the P alongside it, so PF is
    // derived from ONE pairing rather than independently-peaked P and
    // Q (which could pair values from different instants).
    let peak_pq = g.peak_grid_pq();
    let peak_grid_var = peak_pq.map(|(_, q)| q.abs()).unwrap_or(0.0);
    let site_pf_at_reactive_peak = peak_pq.and_then(|(p, q)| {
        let apparent = (p * p + q * q).sqrt();
        (apparent != 0.0).then(|| p.abs() / apparent)
    });
    RecordingReport {
        name: g.name.clone(),
        elapsed_s: g.elapsed_s(now),
        ended_by: g.ended_by,
        peak_grid_w: g.peak_grid_active_w(),
        peak_grid_var,
        site_pf_at_reactive_peak,
        total_battery_charged_wh: total_charged,
        total_battery_discharged_wh: total_discharged,
        total_pv_produced_wh: total_pv,
        per_battery,
        per_pv,
        soc_stats,
        grid_window_averages,
    }
}

#[cfg(test)]
mod tests {
    use super::{RecordingError, compute_soc_stats, recording_dir};
    use crate::sim::microgrid_site::MicrogridSite;
    use crate::sim::recording::EndedBy;
    use chrono::Utc;

    /// A second recording of one name in the same second writes to a
    /// directory of its own, and a refused start leaves no directory.
    #[test]
    fn a_recording_never_reuses_a_directory() {
        let state = tempfile::tempdir().unwrap();
        let w = MicrogridSite::new();
        let now = Utc::now();
        let dir = recording_dir(state.path(), 7, "run", now);
        w.recording_start("run".into(), Some(&dir), now).unwrap();
        assert!(matches!(
            w.recording_start("run".into(), Some(&dir), now),
            Err(RecordingError::Busy)
        ));
        w.recording_stop(now).unwrap();
        w.recording_start("run".into(), Some(&dir), now).unwrap();
        let mut made: Vec<String> = std::fs::read_dir(dir.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        made.sort();
        let first = dir.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(made, vec![first.clone(), format!("{first}-2")]);
        let (listed, _) = w.recording_csv_listing().unwrap();
        assert_eq!(listed, dir.with_file_name(format!("{first}-2")));
    }

    /// A start that loses the journal to another start once its files
    /// are open removes its directory; the other one keeps its own.
    #[test]
    fn a_start_that_loses_the_race_removes_its_directory() {
        let state = tempfile::tempdir().unwrap();
        let w = MicrogridSite::new();
        let now = Utc::now();
        let dir = recording_dir(state.path(), 7, "run", now);
        let opened = w.recording_open_csv(&dir).unwrap();
        let lost = opened.0.dir.clone();
        w.recording_start("other".into(), Some(&dir), now).unwrap();
        assert!(matches!(
            w.recording_start_with("run".into(), Some(&dir), Some(opened), now),
            Err(RecordingError::Busy)
        ));
        assert!(!lost.exists(), "the loser's directory is gone");
        let (kept, _) = w.recording_csv_listing().unwrap();
        assert!(kept.exists() && kept != lost);
    }

    /// A rebuild between the open and the start: the files are opened
    /// again, for the components installed now, and the stale ones
    /// go.
    #[test]
    fn a_start_across_a_rebuild_opens_the_new_components_files() {
        let state = tempfile::tempdir().unwrap();
        let w = MicrogridSite::new();
        w.swap_topology(built(2), &Default::default());
        let now = Utc::now();
        let dir = recording_dir(state.path(), 7, "run", now);
        let opened = w.recording_open_csv(&dir).unwrap();
        w.swap_topology(built(3), &Default::default());
        w.recording_start_with("run".into(), Some(&dir), Some(opened), now)
            .unwrap();
        let (_, files) = w.recording_csv_listing().unwrap();
        assert!(files.iter().any(|f| f == "3-meter.csv"), "{files:?}");
        assert!(!files.iter().any(|f| f == "2-meter.csv"), "{files:?}");
    }

    /// A grid connection point with one meter, `meter`, below it.
    fn built(meter: u64) -> crate::spec::BuiltTopology {
        let doc = format!(
            r#"{{"schema_version": 1, "microgrids": [{{"components": [
                {{"type": "grid_connection_point", "id": 1}},
                {{"type": "meter", "id": {meter}}}], "connections": [[1, {meter}]]}}]}}"#
        );
        let spec = crate::spec::TopologyDoc::from_json(&doc)
            .unwrap()
            .microgrids
            .remove(0);
        crate::spec::build_topology(&spec, &crate::spec::Defaults::default()).unwrap()
    }

    /// A start is refused while a recording runs, and stop and event
    /// need a running recording.
    #[test]
    fn a_recording_needs_the_journal_to_itself() {
        let w = MicrogridSite::new();
        let now = Utc::now();
        assert!(matches!(
            w.recording_stop(now),
            Err(RecordingError::NotRunning)
        ));
        assert!(matches!(
            w.recording_event("k".into(), "p".into(), now),
            Err(RecordingError::NotRunning)
        ));

        w.recording_start("r".into(), None, now).unwrap();
        assert!(matches!(
            w.recording_start("again".into(), None, now),
            Err(RecordingError::Busy)
        ));
        assert_eq!(w.recording_event("k".into(), "p".into(), now).unwrap(), 0);
        w.recording_stop(now).unwrap();
        assert!(matches!(
            w.recording_stop(now),
            Err(RecordingError::NotRunning)
        ));
    }

    /// A rebuild ends the recording, and the next stop collects the
    /// report it kept, once; until then an event is refused as late.
    #[test]
    fn a_rebuild_keeps_the_report_for_one_stop() {
        let w = MicrogridSite::new();
        let now = Utc::now();
        w.recording_start("r".into(), None, now).unwrap();
        drop(w.recording_end_for_rebuild(now));
        assert!(!w.recording_is_running());
        assert!(matches!(
            w.recording_event("k".into(), "p".into(), now),
            Err(RecordingError::EndedByRebuild)
        ));

        let report = w.recording_stop(now).unwrap();
        assert_eq!(report.name.as_deref(), Some("r"));
        assert_eq!(report.ended_by, Some(EndedBy::Rebuild));
        assert!(matches!(
            w.recording_stop(now),
            Err(RecordingError::NotRunning)
        ));

        assert!(matches!(
            w.recording_event("k".into(), "p".into(), now),
            Err(RecordingError::NotRunning)
        ));

        // A new recording drops a report nobody collected.
        w.recording_start("r2".into(), None, now).unwrap();
        drop(w.recording_end_for_rebuild(now));
        w.recording_start("r3".into(), None, now).unwrap();
        let report = w.recording_stop(now).unwrap();
        assert_eq!(report.name.as_deref(), Some("r3"));
        assert_eq!(report.ended_by, Some(EndedBy::Stop));
    }

    #[test]
    fn a_recording_dir_has_a_safe_name_and_a_utc_timestamp() {
        let now = chrono::DateTime::from_timestamp(1_791_288_000, 0).unwrap();
        let dir = recording_dir(std::path::Path::new("/s"), 7, "../x y", now);
        assert_eq!(
            dir,
            std::path::Path::new("/s/recordings/7/___x_y-20261006T120000Z")
        );
        let long = recording_dir(std::path::Path::new("/s"), 7, &"é".repeat(80), now);
        let name = long.file_name().unwrap().to_str().unwrap();
        assert_eq!(name, format!("{}-20261006T120000Z", "_".repeat(64)));
    }

    #[test]
    fn soc_stats_compute_on_typical_set() {
        let s = compute_soc_stats(&[20.0, 40.0, 60.0, 80.0]).unwrap();
        // Mean of 20, 40, 60, 80 = 50.
        assert!((s.mean_pct - 50.0).abs() < 1e-6);
        // Median (lower of middle two on even count) = 40.
        assert!((s.median_pct - 40.0).abs() < 1e-6);
        // No clear mode — all equal counts at distinct buckets;
        // returns the lowest tied bucket (20).
        assert_eq!(s.mode_pct, Some(20));
    }

    #[test]
    fn soc_stats_mode_picks_repeated_bucket() {
        let s = compute_soc_stats(&[50.0, 50.4, 50.6, 25.0, 80.0]).unwrap();
        // Three SoCs round to 50 (50, 50, 51 — actually 50.6 rounds
        // to 51, so mode is 50 with 2 buckets, vs 51, 25, 80 each at
        // 1).
        assert_eq!(s.mode_pct, Some(50));
    }

    #[test]
    fn soc_stats_empty_returns_none() {
        assert!(compute_soc_stats(&[]).is_none());
    }
}
