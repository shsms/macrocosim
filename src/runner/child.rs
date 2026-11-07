//! One script child process, which runs the script with
//! `PYTHON -P -m macrocosim._runner SCRIPT`. The engine holds its
//! stdin and never writes to it; closing it is the only stop signal.
//! Its stdout and stderr go to the log, line by line, each line led
//! by the script's name, and the last [`OUTPUT_LINES`] are kept for
//! the status.
//!
//! On Unix the child leads a process group of its own, so a Ctrl-C in
//! the terminal reaches only the engine. Once the child has ended,
//! the group is ended too, before the exit is reported: a process the
//! script started stops with it, unless it moved to a process group
//! or session of its own.

use std::collections::VecDeque;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::{oneshot, watch};

/// How many lines of output a child keeps.
pub const OUTPUT_LINES: usize = 50;

/// How long [`Child::stop`] waits for the child to exit after closing
/// its stdin, before it kills it.
pub const STOP_GRACE: Duration = Duration::from_secs(3);

/// The longest line of output kept; the rest of a longer line is
/// dropped.
pub const MAX_LINE_BYTES: usize = 4096;

/// How long the processes left in a child's group get between the
/// SIGTERM and the SIGKILL once the child has ended.
#[cfg(unix)]
const GROUP_GRACE: Duration = Duration::from_secs(1);

/// How a child ended: its exit code, `None` when a signal ended it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exit {
    pub code: Option<i32>,
}

/// The last lines a child wrote, stdout and stderr interleaved.
#[derive(Default)]
pub struct Output(Mutex<VecDeque<String>>);

impl Output {
    pub fn push(&self, line: String) {
        let mut lines = self.0.lock();
        if lines.len() == OUTPUT_LINES {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    pub fn lines(&self) -> Vec<String> {
        self.0.lock().iter().cloned().collect()
    }
}

/// A running (or ended) script child.
pub struct Child {
    pid: Option<u32>,
    stdin: Mutex<Option<ChildStdin>>,
    kill: Mutex<Option<oneshot::Sender<()>>>,
    exit: watch::Receiver<Option<Exit>>,
    output: Arc<Output>,
}

impl Child {
    /// Start `python -P -m macrocosim._runner script`, with the
    /// engine's address and the child's token in its environment. Its
    /// output is logged under the target `script:{name}`, each line
    /// led by `name`.
    pub fn spawn(
        python: &Path,
        script: &Path,
        url: &str,
        token: &str,
        name: String,
    ) -> std::io::Result<Self> {
        let mut command = tokio::process::Command::new(python);
        command
            .args(["-P", "-m", "macrocosim._runner"])
            .arg(script)
            .env("MACROCOSIM_URL", url)
            .env("MACROCOSIM_RUNNER_TOKEN", token)
            // UTF-8 on the pipes whatever the locale, for the child
            // and the processes it starts.
            .env("PYTHONUTF8", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // The child leads a process group of its own, which the
        // terminal's Ctrl-C does not reach.
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn()?;
        let pid = child.id();
        let stdin = child.stdin.take();
        let output = Arc::new(Output::default());
        let label = Arc::new((format!("script:{name}"), name));
        let readers = [
            child
                .stdout
                .take()
                .map(|s| read_lines(s, false, &output, &label)),
            child
                .stderr
                .take()
                .map(|s| read_lines(s, true, &output, &label)),
        ];
        let (kill_tx, kill_rx) = oneshot::channel();
        let (exit_tx, exit_rx) = watch::channel(None);
        tokio::spawn(async move {
            let status = tokio::select! {
                status = child.wait() => status,
                // A kill, or the `Child` dropped.
                _ = kill_rx => {
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            #[cfg(unix)]
            if let Some(pid) = pid {
                end_group(pid).await;
            }
            // The last lines come before the exit is reported, so a
            // reader of the exit sees them. A grandchild holding the
            // pipes open must not hold the exit back for long.
            for reader in readers.into_iter().flatten() {
                let _ = tokio::time::timeout(Duration::from_secs(1), reader).await;
            }
            let code = status.ok().and_then(|s| s.code());
            let _ = exit_tx.send(Some(Exit { code }));
        });
        Ok(Self {
            pid,
            stdin: Mutex::new(stdin),
            kill: Mutex::new(Some(kill_tx)),
            exit: exit_rx,
            output,
        })
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// The last lines the child wrote.
    pub fn output(&self) -> Vec<String> {
        self.output.lines()
    }

    /// How the child ended; `None` while it runs.
    pub fn try_exit_status(&self) -> Option<Exit> {
        *self.exit.borrow()
    }

    pub fn alive(&self) -> bool {
        self.try_exit_status().is_none()
    }

    /// Wait until the child has ended.
    pub async fn wait_exit(&self) -> Exit {
        let mut exit = self.exit.clone();
        match exit.wait_for(Option::is_some).await {
            Ok(e) => e.expect("waited for Some"),
            // The waiter task is gone; it reported nothing.
            Err(_) => Exit { code: None },
        }
    }

    /// Kill the child and wait until it has ended.
    pub async fn kill(&self) {
        if let Some(kill) = self.kill.lock().take() {
            let _ = kill.send(());
        }
        self.wait_exit().await;
    }

    /// Close the child's stdin, wait up to [`STOP_GRACE`] for it to
    /// exit, then kill it and wait.
    pub async fn stop(&self) {
        drop(self.stdin.lock().take());
        if tokio::time::timeout(STOP_GRACE, self.wait_exit())
            .await
            .is_err()
        {
            self.kill().await;
        }
    }
}

/// End what is left of the OS process group `pid` led: SIGTERM, then
/// SIGKILL if a process of it is still there after [`GROUP_GRACE`]. A
/// group that is already empty is left alone. The SIGKILL is sent
/// only while the group is still seen; a group number freed and
/// reused between two polls cannot be told apart.
#[cfg(unix)]
async fn end_group(pid: u32) {
    let Ok(pgid) = libc::pid_t::try_from(pid) else {
        return;
    };
    // SAFETY: killpg only sends a signal (signal 0 sends none and
    // only asks whether the group exists); ESRCH, no such group, is
    // the usual answer and ends the wait.
    let signal = |sig| unsafe { libc::killpg(pgid, sig) } == 0;
    if !signal(libc::SIGTERM) {
        return;
    }
    let deadline = tokio::time::Instant::now() + GROUP_GRACE;
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
        if !signal(0) {
            return;
        }
    }
    signal(libc::SIGKILL);
}

/// Read `stream` line by line into the log and `output` until it
/// ends. A line that is not UTF-8 is decoded with replacement
/// characters, and one longer than [`MAX_LINE_BYTES`] bytes is cut
/// there.
fn read_lines(
    stream: impl AsyncRead + Unpin + Send + 'static,
    stderr: bool,
    output: &Arc<Output>,
    label: &Arc<(String, String)>,
) -> tokio::task::JoinHandle<()> {
    let output = output.clone();
    let label = label.clone();
    tokio::spawn(async move {
        let (target, name) = &*label;
        let mut reader = BufReader::new(stream);
        while let Some(line) = next_line(&mut reader).await {
            if stderr {
                log::warn!(target: target.as_str(), "{name}: {line}");
            } else {
                log::info!(target: target.as_str(), "{name}: {line}");
            }
            output.push(line);
        }
    })
}

/// The next line of `reader` without its line ending, at most
/// [`MAX_LINE_BYTES`] bytes of it; `None` at the end or on a read
/// error.
async fn next_line(reader: &mut (impl AsyncBufRead + Unpin)) -> Option<String> {
    let mut line = Vec::new();
    let mut read_any = false;
    loop {
        let buf = reader.fill_buf().await.ok()?;
        if buf.is_empty() {
            return read_any.then(|| decode_lossy(&line));
        }
        read_any = true;
        let (chunk, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(at) => (&buf[..at], Some(at + 1)),
            None => (buf, None),
        };
        let room = MAX_LINE_BYTES.saturating_sub(line.len());
        line.extend_from_slice(&chunk[..chunk.len().min(room)]);
        let used = done.unwrap_or(buf.len());
        reader.consume(used);
        if done.is_some() {
            return Some(decode_lossy(&line));
        }
    }
}

fn decode_lossy(line: &[u8]) -> String {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    String::from_utf8_lossy(line).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn lines_of(bytes: &[u8]) -> Vec<String> {
        let mut reader = BufReader::new(bytes);
        let mut lines = Vec::new();
        while let Some(line) = next_line(&mut reader).await {
            lines.push(line);
        }
        lines
    }

    #[tokio::test]
    async fn a_line_that_is_not_utf8_is_read_and_the_rest_follows() {
        let lines = lines_of(b"temp 21 \xb0C\nnext\r\nlast").await;
        assert_eq!(lines, ["temp 21 \u{fffd}C", "next", "last"]);
    }

    /// A child of a stand-in interpreter that runs `body` (sh).
    #[cfg(unix)]
    fn child_running(dir: &Path, body: &str) -> Child {
        use std::os::unix::fs::PermissionsExt;
        let python = dir.join("python");
        std::fs::write(&python, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
        let script = dir.join("plant.py");
        // Another test's fork may still hold the file open for
        // writing for a moment (ETXTBSY); try again.
        for _ in 0..50 {
            match Child::spawn(
                &python,
                &script,
                "http://127.0.0.1:1",
                "t",
                "plant.py".into(),
            ) {
                Ok(child) => return child,
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(e) => panic!("spawn: {e}"),
            }
        }
        panic!("the stand-in interpreter stayed busy");
    }

    /// A child that exits when its stdin closes is stopped at once.
    #[cfg(unix)]
    #[tokio::test]
    async fn stop_closes_stdin_and_the_child_exits() {
        let dir = tempfile::TempDir::with_prefix("mc-child-").unwrap();
        let child = child_running(dir.path(), "exec cat >/dev/null");
        let started = std::time::Instant::now();
        child.stop().await;
        assert!(started.elapsed() < STOP_GRACE, "{:?}", started.elapsed());
        assert_eq!(child.try_exit_status(), Some(Exit { code: Some(0) }));
    }

    /// A child that ignores its stdin closing is killed after the
    /// grace.
    #[cfg(unix)]
    #[tokio::test]
    async fn stop_kills_a_child_that_outlives_the_grace() {
        let dir = tempfile::TempDir::with_prefix("mc-child-").unwrap();
        let child = child_running(dir.path(), "exec sleep 60");
        let started = std::time::Instant::now();
        child.stop().await;
        let took = started.elapsed();
        assert!(took >= STOP_GRACE, "{took:?}");
        assert!(took < STOP_GRACE + Duration::from_secs(2), "{took:?}");
        assert!(!child.alive());
        assert_eq!(
            child.try_exit_status(),
            Some(Exit { code: None }),
            "a signal ended it"
        );
    }

    /// The output keeps the last [`OUTPUT_LINES`] lines.
    #[test]
    fn the_output_keeps_the_last_lines() {
        let output = Output::default();
        for i in 0..OUTPUT_LINES + 10 {
            output.push(format!("line {i}"));
        }
        let lines = output.lines();
        assert_eq!(lines.len(), OUTPUT_LINES);
        assert_eq!(lines[0], "line 10");
        assert_eq!(
            lines[OUTPUT_LINES - 1],
            format!("line {}", OUTPUT_LINES + 9)
        );
    }

    #[tokio::test]
    async fn a_long_line_is_cut() {
        let mut bytes = vec![b'x'; MAX_LINE_BYTES * 3];
        bytes.extend_from_slice(b"\nshort\n");
        let lines = lines_of(&bytes).await;
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), MAX_LINE_BYTES);
        assert_eq!(lines[1], "short");
    }
}
