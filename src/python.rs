//! Finding the Python interpreter the `.py` export runs on, checking it, and
//! running the export ([`write_module`]). The check: Python 3.11 or later, with
//! the `macrocosim` package of the binary's own version. A passed check is kept
//! for the life of the `Interpreter`; a failed one runs again on the next use,
//! so installing the package fixes the export without a restart.
//!
//! The first match wins: `--python`, then `$MACROCOSIM_PYTHON`, then
//! a `python` or `python3` next to the running binary (the wheel's
//! layout), then `python3` and `python` on `PATH`. A candidate that
//! fails the check is reported; the lookup does not fall through to
//! the next one, which would hide a misconfigured setup.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::AsyncWriteExt;

use crate::version::VERSION;

/// The oldest Python the export may run on.
pub const MIN_PYTHON: (u32, u32) = (3, 11);

/// How long the check may take before it counts as failed.
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// How long `python -m macrocosim.export` may take.
pub const WRITE_MODULE_TIMEOUT: Duration = Duration::from_secs(10);

/// The Python source the check runs. It prints the interpreter's
/// version on the first line, then the `macrocosim` package's version
/// on the second. Without `-P`, which Python 3.10 and older refuse,
/// so an old interpreter still reports its version; the current
/// directory `-c` puts first on `sys.path` is dropped instead, so no
/// stray file there is imported. Under `PYTHONSAFEPATH` no directory
/// is put first, and nothing is dropped.
const CHECK_SCRIPT: &str = "import sys\n\
                     if not getattr(sys.flags, 'safe_path', False): del sys.path[0]\n\
                     print(*sys.version_info[:2])\n\
                     import importlib.metadata as m; print(m.version('macrocosim'))";

/// The interpreter an engine uses, found and checked on use until a
/// check passes.
#[derive(Default)]
pub struct Interpreter {
    /// `--python`.
    override_path: Mutex<Option<PathBuf>>,
    /// The interpreter that passed the check, kept for the life of
    /// the engine. Held across a check, so two uses never check at
    /// once.
    checked: tokio::sync::Mutex<Option<PathBuf>>,
}

impl Interpreter {
    /// Use `path` (`--python`) instead of looking for one. Only takes
    /// effect until a [`Self::resolve`] passes.
    pub fn set_override(&self, path: PathBuf) {
        *self.override_path.lock() = Some(path);
    }

    /// The interpreter to run. Returns the one a check already
    /// passed, without checking again. Until a check has passed, each
    /// call finds an interpreter and checks it now. Errs with the
    /// install message when the check fails.
    pub async fn resolve(&self) -> Result<PathBuf, String> {
        let mut checked = self.checked.lock().await;
        if let Some(path) = &*checked {
            return Ok(path.clone());
        }
        let override_path = self.override_path.lock().clone();
        let path = locate(find(override_path)?, std::env::var_os("PATH"));
        check(&path)
            .await
            .map_err(|what| message(&path.display().to_string(), &what))?;
        *checked = Some(path.clone());
        Ok(path)
    }
}

/// The Python module `python -m macrocosim.export` on `python` writes for
/// `input`, a topology document, or why that failed or took longer than
/// [`WRITE_MODULE_TIMEOUT`].
pub async fn write_module(python: &Path, input: &impl serde::Serialize) -> Result<String, String> {
    let input = serde_json::to_vec(input).map_err(|e| e.to_string())?;
    let mut child = tokio::process::Command::new(python)
        .args(["-P", "-m", "macrocosim.export"])
        // Its error message in UTF-8 too, whatever the locale.
        .env("PYTHONUTF8", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("{} does not run: {e}", python.display()))?;
    let mut stdin = child.stdin.take().expect("piped above");
    let run = async move {
        // A write error means the child exited early; its stderr says why.
        let _ = stdin.write_all(&input).await;
        drop(stdin);
        child.wait_with_output().await
    };
    let out = match tokio::time::timeout(WRITE_MODULE_TIMEOUT, run).await {
        Err(_) => return Err(format!("no answer within {WRITE_MODULE_TIMEOUT:?}")),
        Ok(Err(e)) => return Err(e.to_string()),
        Ok(Ok(out)) => out,
    };
    if !out.status.success() {
        return Err(format!(
            "{}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

/// The message an export answers with when there is no usable
/// interpreter.
pub fn message(found: &str, what: &str) -> String {
    format!(
        "macrocosim needs Python {}.{} or later with the macrocosim package, version {}. \
         Found: {found} ({what}). Install it, or pass --python.",
        MIN_PYTHON.0, MIN_PYTHON.1, *VERSION
    )
}

/// The first candidate, in the lookup order of the module docs.
fn find(flag: Option<PathBuf>) -> Result<PathBuf, String> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    find_in(
        flag,
        std::env::var_os("MACROCOSIM_PYTHON"),
        exe_dir,
        std::env::var_os("PATH"),
    )
}

/// [`find`] over the given flag, `$MACROCOSIM_PYTHON`, the binary's
/// directory and `PATH`.
fn find_in(
    flag: Option<PathBuf>,
    env: Option<std::ffi::OsString>,
    exe_dir: Option<PathBuf>,
    path_var: Option<std::ffi::OsString>,
) -> Result<PathBuf, String> {
    if let Some(path) = flag {
        return Ok(path);
    }
    if let Some(path) = env.filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let names: &[&str] = if cfg!(windows) {
        &["python.exe", "python3.exe"]
    } else {
        &["python", "python3"]
    };
    if let Some(dir) = exe_dir {
        for name in names {
            let path = dir.join(name);
            if path.is_file() {
                return Ok(path);
            }
        }
    }
    let on_path: &[&str] = if cfg!(windows) {
        &["python3.exe", "python.exe"]
    } else {
        &["python3", "python"]
    };
    if let Some(path) = on_path_var(on_path, path_var) {
        return Ok(path);
    }
    Err(message(
        "nothing",
        "no --python, MACROCOSIM_PYTHON, python next to the binary, or python3 or python \
         on PATH",
    ))
}

/// `path` as the children run it, absolute since they run in other
/// directories: a bare name (`python3`) is looked up on `PATH`, any
/// other relative path is taken from the current directory. A bare
/// name `PATH` does not have is left as it is, for the check to
/// report.
fn locate(path: PathBuf, path_var: Option<std::ffi::OsString>) -> PathBuf {
    if path.components().count() == 1 && !path.is_absolute() {
        return on_path_var(&[&path], path_var).unwrap_or(path);
    }
    std::path::absolute(&path).unwrap_or(path)
}

/// The first file named one of `names` in the directories of
/// `path_var` (a `PATH` value), each directory asked for every name
/// before the next one.
fn on_path_var(
    names: &[impl AsRef<Path>],
    path_var: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    let paths = path_var?;
    std::env::split_paths(&paths)
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|p| p.is_file())
}

/// Run the check on `python`; what failed, if anything.
async fn check(python: &Path) -> Result<(), String> {
    let run = tokio::process::Command::new(python)
        .arg("-c")
        .arg(CHECK_SCRIPT)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = match tokio::time::timeout(CHECK_TIMEOUT, run).await {
        Err(_) => return Err(format!("it did not answer within {CHECK_TIMEOUT:?}")),
        Ok(Err(e)) => return Err(format!("it does not run: {e}")),
        Ok(Ok(out)) => out,
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut lines = stdout.lines();
    let version = lines.next().and_then(|line| {
        let mut parts = line.split_whitespace().map(str::parse::<u32>);
        match (parts.next(), parts.next()) {
            (Some(Ok(major)), Some(Ok(minor))) => Some((major, minor)),
            _ => None,
        }
    });
    let Some((major, minor)) = version else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let last = stderr.lines().last().unwrap_or("").trim();
        return Err(format!("it did not report its version: {last}"));
    };
    if (major, minor) < MIN_PYTHON {
        return Err(format!("Python {major}.{minor}"));
    }
    match lines.next().map(str::trim) {
        Some(found) if found == VERSION.as_str() => Ok(()),
        Some(found) if !found.is_empty() => Err(format!("macrocosim {found}")),
        _ => Err("no macrocosim package".to_string()),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Held while a test writes and runs a fake interpreter, so no
    /// other test here forks while a fake is open for writing (which
    /// makes running it fail with ETXTBSY).
    static SPAWNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// A shell script standing in for Python, printing `stdout`.
    fn fake(dir: &Path, stdout: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("python");
        std::fs::write(&path, format!("#!/bin/sh\nprintf '{stdout}'\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    async fn checked(stdout: &str) -> Result<PathBuf, String> {
        let dir = tempfile::TempDir::with_prefix("mc-python-").unwrap();
        let interpreter = Interpreter::default();
        interpreter.set_override(fake(dir.path(), stdout));
        interpreter.resolve().await
    }

    #[tokio::test]
    async fn the_check_names_what_failed() {
        let _spawns = SPAWNS.lock().await;
        let v = VERSION.as_str();
        assert!(checked(&format!("3 11\\n{v}\\n")).await.is_ok());
        assert!(checked(&format!("4 0\\n{v}\\n")).await.is_ok());
        let err = checked(&format!("3 10\\n{v}\\n")).await.unwrap_err();
        assert!(err.contains("(Python 3.10)"), "{err}");
        let err = checked("3 12\\n0.1.0\\n").await.unwrap_err();
        assert!(err.contains("(macrocosim 0.1.0)"), "{err}");
        assert!(err.contains(&format!("version {v}. Found: ")), "{err}");
        assert!(err.ends_with("Install it, or pass --python."), "{err}");
        let err = checked("3 12\\n").await.unwrap_err();
        assert!(err.contains("(no macrocosim package)"), "{err}");
        let err = checked("").await.unwrap_err();
        assert!(err.contains("did not report its version"), "{err}");
    }

    /// The first match wins: the flag, then a non-empty
    /// `$MACROCOSIM_PYTHON`, then `python` and `python3` beside the
    /// binary, then `python3` and `python` on `PATH`.
    #[cfg(unix)]
    #[test]
    fn the_interpreter_is_looked_for_in_order() {
        let root = tempfile::TempDir::with_prefix("mc-find-").unwrap();
        let (exe, a, b) = (
            root.path().join("exe"),
            root.path().join("a"),
            root.path().join("b"),
        );
        for dir in [&exe, &a, &b] {
            std::fs::create_dir(dir).unwrap();
        }
        let touch = |p: PathBuf| {
            std::fs::write(&p, "").unwrap();
            p
        };
        let path_var = Some(std::env::join_paths([&a, &b]).unwrap());
        let find = |flag: Option<&str>, env: Option<&str>| {
            find_in(
                flag.map(PathBuf::from),
                env.map(Into::into),
                Some(exe.clone()),
                path_var.clone(),
            )
        };
        assert!(find(None, None).is_err(), "nothing anywhere");
        let in_b = touch(b.join("python3"));
        assert_eq!(find(None, None).unwrap(), in_b);
        let in_a = touch(a.join("python"));
        assert_eq!(
            find(None, None).unwrap(),
            in_a,
            "an earlier PATH entry wins"
        );
        let a3 = touch(a.join("python3"));
        assert_eq!(
            find(None, None).unwrap(),
            a3,
            "python3 before python on PATH"
        );
        touch(exe.join("python3"));
        assert_eq!(
            find(None, None).unwrap(),
            exe.join("python3"),
            "beside the binary"
        );
        let beside = touch(exe.join("python"));
        assert_eq!(
            find(None, None).unwrap(),
            beside,
            "python before python3 beside it"
        );
        assert_eq!(
            find(None, Some("")).unwrap(),
            beside,
            "an empty variable is ignored"
        );
        assert_eq!(
            find(None, Some("/env/python")).unwrap(),
            PathBuf::from("/env/python")
        );
        assert_eq!(
            find(Some("/flag/python"), Some("/env/python")).unwrap(),
            PathBuf::from("/flag/python")
        );
    }

    #[test]
    fn a_bare_name_is_looked_up_on_the_path() {
        let found = locate(PathBuf::from("sh"), std::env::var_os("PATH"));
        assert!(
            found.is_absolute() && found.is_file(),
            "{}",
            found.display()
        );
        assert_eq!(
            locate(
                PathBuf::from("no-such-python-here"),
                std::env::var_os("PATH")
            ),
            PathBuf::from("no-such-python-here")
        );
        let relative = locate(PathBuf::from("venv/bin/python"), std::env::var_os("PATH"));
        assert_eq!(
            relative,
            std::env::current_dir().unwrap().join("venv/bin/python")
        );
    }

    /// Under `PYTHONSAFEPATH` the check reads the interpreter's and
    /// the package's versions from the project's own Python and
    /// leaves `sys.path` whole (skipped without `python/.venv`,
    /// failed instead under `MACROCOSIM_REQUIRE_PYTHON=1`).
    #[tokio::test]
    async fn the_check_keeps_sys_path_under_safe_path() {
        let python = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/python/.venv/bin/python"
        ));
        if !python.is_file() {
            assert!(
                std::env::var_os("MACROCOSIM_REQUIRE_PYTHON").is_none_or(|v| v != "1"),
                "MACROCOSIM_REQUIRE_PYTHON=1 but {} is missing",
                python.display()
            );
            eprintln!("skipped: {} is missing", python.display());
            return;
        }
        let run = |script: String| {
            let python = python.clone();
            async move {
                let out = tokio::process::Command::new(&python)
                    .arg("-c")
                    .arg(script)
                    .env("PYTHONSAFEPATH", "1")
                    .output()
                    .await
                    .unwrap();
                assert!(
                    out.status.success(),
                    "{}",
                    String::from_utf8_lossy(&out.stderr)
                );
                String::from_utf8_lossy(&out.stdout).into_owned()
            }
        };
        let checked = run(format!("{CHECK_SCRIPT}\nprint(len(sys.path))")).await;
        let lines: Vec<&str> = checked.lines().collect();
        assert_eq!(lines.len(), 3, "{checked}");
        assert_eq!(lines[1], VERSION.as_str());
        let plain = run("import sys; print(len(sys.path))".into()).await;
        assert_eq!(lines[2], plain.trim(), "the check dropped a path entry");
    }

    #[tokio::test]
    async fn a_missing_interpreter_is_reported() {
        let _spawns = SPAWNS.lock().await;
        let interpreter = Interpreter::default();
        interpreter.set_override(PathBuf::from("/nonexistent/python"));
        let err = interpreter.resolve().await.unwrap_err();
        assert!(
            err.contains("Found: /nonexistent/python (it does not run:"),
            "{err}"
        );
    }

    /// A failed check runs again on the next use, so installing the
    /// package needs no restart; a passed one is kept. The fake is
    /// written once, before anything runs, and reads its answer from
    /// a file the test changes.
    #[tokio::test]
    async fn only_a_passed_check_is_kept() {
        use std::os::unix::fs::PermissionsExt;
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::TempDir::with_prefix("mc-python-").unwrap();
        let answer = dir.path().join("answer");
        let python = dir.path().join("python");
        std::fs::write(&python, format!("#!/bin/sh\ncat '{}'\n", answer.display())).unwrap();
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
        let interpreter = Interpreter::default();
        interpreter.set_override(python.clone());

        std::fs::write(&answer, "3 12\n").unwrap();
        let err = interpreter.resolve().await.unwrap_err();
        assert!(err.contains("(no macrocosim package)"), "{err}");

        std::fs::write(&answer, format!("3 12\n{}\n", *VERSION)).unwrap();
        assert_eq!(
            interpreter.resolve().await.unwrap(),
            python,
            "checked again"
        );

        std::fs::write(&answer, "3 12\n").unwrap();
        assert_eq!(
            interpreter.resolve().await.unwrap(),
            python,
            "the pass is kept"
        );
    }
}
