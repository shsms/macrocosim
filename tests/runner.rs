//! `macrocosim plant.py`: the real binary runs a script copied from
//! `tests/fixtures/scripts/` in a child Python process, and every
//! edit is a rewrite of that copy. The tests read the runner's state
//! from `GET /api/runner`; its `pid` lets a test check that a stopped
//! child has exited (`/proc/<pid>`, so Linux only).
#![cfg(target_os = "linux")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/scripts");

/// A running binary on a copy of a fixture script, killed on drop.
struct Engine {
    child: Child,
    dir: tempfile::TempDir,
    ui: String,
    client: reqwest::Client,
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Copy `fixture` to `plant.py` (and `extra` beside it, under their
/// own names) in a new directory.
fn script_dir(fixture: &str, extra: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::TempDir::with_prefix("mc-runner-").unwrap();
    std::fs::create_dir(dir.path().join("s")).unwrap();
    std::fs::copy(
        Path::new(FIXTURES).join(fixture),
        dir.path().join("s/plant.py"),
    )
    .unwrap();
    for name in extra {
        std::fs::copy(
            Path::new(FIXTURES).join(name),
            dir.path().join("s").join(name),
        )
        .unwrap();
    }
    dir
}

fn spawn(dir: &Path, python: &Path) -> Child {
    let log = std::fs::File::create(dir.join("engine.log")).unwrap();
    Command::new(env!("CARGO_BIN_EXE_macrocosim"))
        .args(["--ephemeral-ports", "--state-dir"])
        .arg(dir.join("state"))
        .arg(format!(
            "--emit-endpoints={}",
            dir.join("endpoints.json").display()
        ))
        .arg("--python")
        .arg(python)
        .arg(dir.join("s/plant.py"))
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap()
}

impl Engine {
    /// The binary on `fixture` (with `extra` files beside it), once
    /// its endpoints file is written; `None` without the venv.
    fn start(fixture: &str, extra: &[&str]) -> Option<Self> {
        let python = common::venv_python()?;
        let dir = script_dir(fixture, extra);
        std::fs::create_dir(dir.path().join("state")).unwrap();
        let child = spawn(dir.path(), &python);
        let endpoints = dir.path().join("endpoints.json");
        let deadline = Instant::now() + Duration::from_secs(60);
        while !endpoints.exists() {
            assert!(Instant::now() < deadline, "no endpoints file after 60 s");
            std::thread::sleep(Duration::from_millis(50));
        }
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&endpoints).unwrap()).unwrap();
        let ui = format!("http://{}", v["ui"].as_str().unwrap());
        Some(Self {
            child,
            dir,
            ui,
            client: reqwest::Client::new(),
        })
    }

    fn script(&self) -> PathBuf {
        self.dir.path().join("s/plant.py")
    }

    fn endpoints(&self) -> Value {
        let text = std::fs::read_to_string(self.dir.path().join("endpoints.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    /// Replace `from` with `to` in the script copy.
    fn edit(&self, from: &str, to: &str) {
        let text = std::fs::read_to_string(self.script()).unwrap();
        assert!(text.contains(from), "{from:?} is not in the script");
        std::fs::write(self.script(), text.replacen(from, to, 1)).unwrap();
    }

    /// Overwrite the script copy with the fixture `name`.
    fn replace_with(&self, name: &str) {
        std::fs::copy(Path::new(FIXTURES).join(name), self.script()).unwrap();
    }

    async fn get(&self, path: &str) -> Value {
        let r = self
            .client
            .get(format!("{}{path}", self.ui))
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "GET {path}: {}", r.status());
        r.json().await.unwrap()
    }

    async fn status(&self) -> Value {
        self.get("/api/runner").await
    }

    /// Poll the status until `done` holds, for up to `secs`; `what`
    /// names what is awaited.
    async fn wait_for(&self, secs: u64, what: &str, done: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let status = self.status().await;
            if done(&status) {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "no {what} within {secs} s: {status:#}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Wait for the first child to be running.
    async fn running(&self) -> Value {
        self.wait_for(30, "running child", |s| {
            s["child"]["state"] == "running" && s["last_reload"]["result"] == "ok"
        })
        .await
    }

    /// Wait until a reload after the one at `before` has ended.
    async fn reloaded(&self, before: &Value, secs: u64) -> Value {
        let at = before["last_reload"]["updated_at"].clone();
        self.wait_for(secs, "reload", |s| {
            s["last_reload"]["updated_at"] != at && s["last_reload"]["result"] != "pending"
        })
        .await
    }

    async fn microgrid_ids(&self) -> Vec<u64> {
        let list = self.get("/api/microgrids").await;
        list.as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_u64().unwrap())
            .collect()
    }

    async fn spec(&self, mg: u64) -> Value {
        self.get(&format!("/api/mg/{mg}/spec")).await
    }

    async fn set_soc(&self, mg: u64, battery: u64, pct: f64) {
        let r = self
            .client
            .post(format!("{}/api/mg/{mg}/component/{battery}/drive", self.ui))
            .json(&json!({"soc_pct": pct}))
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "{}", r.text().await.unwrap());
    }

    /// The SoC of a battery sampled at least `after_s` (epoch
    /// seconds); waits for such a sample.
    async fn soc_after(&self, mg: u64, battery: u64, after_s: f64) -> f64 {
        let path = format!("/api/mg/{mg}/component/{battery}/history?metric=soc_pct&window_s=60");
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let h = self.get(&path).await;
            if let Some(last) = h["samples"].as_array().unwrap().last()
                && last[0].as_f64().unwrap() > after_s
            {
                return last[1].as_f64().unwrap();
            }
            assert!(Instant::now() < deadline, "no SoC sample of {battery}: {h}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

fn epoch_s(rfc3339: &Value) -> f64 {
    let t = chrono::DateTime::parse_from_rfc3339(rfc3339.as_str().unwrap()).unwrap();
    t.timestamp_millis() as f64 / 1000.0
}

fn alive(pid: &Value) -> bool {
    Path::new(&format!("/proc/{}", pid.as_u64().unwrap())).exists()
}

/// Wait until process `pid` is gone; panics after `secs`.
async fn gone(pid: &Value, secs: f64) {
    let deadline = Instant::now() + Duration::from_secs_f64(secs);
    while alive(pid) {
        assert!(Instant::now() < deadline, "process {pid} still runs");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Send the engine `child` a Ctrl-C and wait for it to exit
/// cleanly; panics after `secs`.
async fn ctrl_c(child: &mut Child, secs: u64) {
    let killed = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(exit) = child.try_wait().unwrap() {
            assert!(exit.success(), "{exit}");
            return;
        }
        assert!(Instant::now() < deadline, "the engine still runs");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Every component id of a stored spec, sorted.
fn component_ids(spec: &Value) -> Vec<u64> {
    let mut ids: Vec<u64> = spec["components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_u64().unwrap())
        .collect();
    ids.sort_unstable();
    ids
}

/// Component `id` of a stored spec.
fn component(spec: &Value, id: u64) -> Value {
    spec["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("no component {id} in {spec}"))
}

#[tokio::test]
async fn a_cold_boot_registers_every_microgrid_of_the_script() {
    let Some(e) = Engine::start("two_microgrids.py", &[]) else {
        return;
    };
    let status = e.running().await;
    assert_eq!(status["microgrids"], json!([61, 62]));
    assert_eq!(
        status["script"],
        e.script().canonicalize().unwrap().display().to_string()
    );
    assert_eq!(e.microgrid_ids().await, vec![61, 62]);
    let north = component_ids(&e.spec(61).await);
    let south = component_ids(&e.spec(62).await);
    assert!(
        north.iter().all(|id| !south.contains(id)),
        "{north:?} {south:?}"
    );
    let listed: Vec<u64> = e.endpoints()["microgrids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_u64().unwrap())
        .collect();
    assert_eq!(
        listed,
        vec![61, 62],
        "the endpoints file waits for the hello"
    );
    let list = e.get("/api/microgrids").await;
    for m in list.as_array().unwrap() {
        assert_eq!(m["unsaved"], false, "{m}");
        assert!(m["source"].as_str().unwrap().ends_with("plant.py"), "{m}");
    }
}

#[tokio::test]
async fn a_run_only_edit_reloads_and_every_battery_keeps_its_soc() {
    let Some(e) = Engine::start("two_microgrids.py", &[]) else {
        return;
    };
    let before = e.running().await;
    e.set_soc(61, 12, 80.0).await;
    e.set_soc(62, 22, 80.0).await;
    e.edit("print(\"running\"", "print(\"running again\"");
    let after = e
        .wait_for(5, "running child again", |s| {
            s["child"]["pid"] != before["child"]["pid"]
                && s["child"]["state"] == "running"
                && s["last_reload"]["result"] == "ok"
        })
        .await;
    gone(&before["child"]["pid"], 1.0).await;
    let at = epoch_s(&after["last_reload"]["updated_at"]);
    assert_eq!(e.soc_after(61, 12, at).await, 80.0);
    assert_eq!(e.soc_after(62, 22, at).await, 80.0);
}

#[tokio::test]
async fn a_spec_edit_starts_the_changed_battery_fresh() {
    let Some(e) = Engine::start("two_microgrids.py", &[]) else {
        return;
    };
    let before = e.running().await;
    e.set_soc(61, 12, 80.0).await;
    e.set_soc(62, 22, 80.0).await;
    e.edit(
        "id=12, initial_soc=Percentage.from_percent(50)",
        "id=12, initial_soc=Percentage.from_percent(60)",
    );
    let after = e.reloaded(&before, 15).await;
    assert_eq!(after["last_reload"]["result"], "ok", "{after:#}");
    let at = epoch_s(&after["last_reload"]["updated_at"]);
    assert_eq!(
        e.soc_after(61, 12, at).await,
        60.0,
        "changed: from the spec"
    );
    assert_eq!(e.soc_after(62, 22, at).await, 80.0, "unchanged: carried");
}

#[tokio::test]
async fn a_syntax_error_fails_to_load_and_leaves_the_old_child_running() {
    let Some(e) = Engine::start("two_microgrids.py", &[]) else {
        return;
    };
    let before = e.running().await;
    let specs = (e.spec(61).await, e.spec(62).await);
    e.replace_with("syntax_error.py");
    let after = e.reloaded(&before, 15).await;
    assert_eq!(after["last_reload"]["result"], "failed_to_load");
    let output = after["last_reload"]["output"].to_string();
    assert!(output.contains("plant.py\\\", line 5"), "{output}");
    assert!(output.contains("SyntaxError"), "{output}");
    assert_eq!(after["child"]["pid"], before["child"]["pid"]);
    assert_eq!(after["child"]["state"], "running");
    assert!(alive(&before["child"]["pid"]));
    assert_eq!(after["microgrids"], json!([61, 62]));
    assert_eq!((e.spec(61).await, e.spec(62).await), specs);
}

/// The hello answers 403 for a token that is not the child's, 400 for
/// another package version and 422 for an invalid spec, and leaves
/// the world as it was. A first load that fails still writes the
/// endpoints file.
#[tokio::test]
async fn a_bad_hello_is_refused_and_changes_nothing() {
    let Some(e) = Engine::start("bad_hellos.py", &[]) else {
        return;
    };
    let answers = e.dir.path().join("answers.json");
    // The script writes the file once its last hello is answered,
    // which can be after the endpoints file appears.
    let read = async || -> Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(text) = std::fs::read_to_string(&answers)
                && let Ok(answers) = serde_json::from_str::<Value>(&text)
            {
                return answers;
            }
            assert!(Instant::now() < deadline, "no answers from the script");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    assert_eq!(read().await, json!({"wrong_token": 403, "bad": 400}));
    assert_eq!(
        e.endpoints()["microgrids"],
        json!([]),
        "written after a failed first load"
    );
    let status = e
        .wait_for(15, "failed load", |s| !s["last_reload"].is_null())
        .await;
    assert_eq!(status["last_reload"]["result"], "failed_to_load");
    assert!(e.microgrid_ids().await.is_empty());

    std::fs::remove_file(&answers).unwrap();
    e.edit(r#"KIND = "version""#, r#"KIND = "topology""#);
    assert_eq!(read().await, json!({"wrong_token": 403, "bad": 422}));
    assert!(e.microgrid_ids().await.is_empty());
}

#[tokio::test]
async fn an_edit_to_a_helper_module_reloads() {
    let Some(e) = Engine::start("helper_import.py", &["helper.py"]) else {
        return;
    };
    let before = e.running().await;
    assert_eq!(component(&e.spec(63).await, 31)["power_w"], 2000.0);
    std::fs::write(e.dir.path().join("s/helper.py"), "LOAD_KW = 3\n").unwrap();
    let after = e.reloaded(&before, 15).await;
    assert_eq!(after["last_reload"]["result"], "ok", "{after:#}");
    assert_eq!(component(&e.spec(63).await, 31)["power_w"], 3000.0);
}

/// The events socket sends a `script_status` event, with no
/// microgrid, when a reload changes the runner's status.
#[tokio::test]
async fn a_reload_reaches_the_events_socket() {
    let Some(e) = Engine::start("two_microgrids.py", &[]) else {
        return;
    };
    let before = e.running().await;
    let mut ws = common::WsClient::connect(&e.ui).await;
    e.replace_with("syntax_error.py");
    let event = ws
        .wait_for(
            Duration::from_secs(20),
            "script_status event of a failed reload",
            |ev| ev["kind"] == "script_status" && ev["last_reload"]["result"] == "failed_to_load",
        )
        .await;
    assert!(
        event.get("microgrid_id").is_none_or(Value::is_null),
        "{event}"
    );
    assert_eq!(event["child"]["pid"], before["child"]["pid"], "{event}");
}

#[tokio::test]
async fn a_crash_is_not_restarted_until_a_file_changes() {
    let Some(e) = Engine::start("crashes.py", &[]) else {
        return;
    };
    let crashed = e
        .wait_for(30, "crash", |s| s["child"]["state"] == "crashed")
        .await;
    assert_eq!(crashed["child"]["exit_code"], 1);
    assert_eq!(crashed["last_reload"]["result"], "ok");
    let output = crashed["output"].to_string();
    assert!(
        output.contains("RuntimeError: the script broke"),
        "{output}"
    );
    assert!(!output.contains("_runner.py"), "{output}");
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(e.status().await, crashed, "no new child");
    assert_eq!(e.microgrid_ids().await, vec![64], "the world stays");
    e.edit("the script broke", "the script broke again");
    let again = e.reloaded(&crashed, 30).await;
    assert_ne!(again["child"]["pid"], crashed["child"]["pid"]);
}

#[tokio::test]
async fn a_script_without_a_hello_is_killed_and_a_newer_save_kills_it_first() {
    let Some(e) = Engine::start("two_microgrids.py", &[]) else {
        return;
    };
    let before = e.running().await;
    let pid_file = e.dir.path().join("s/plant.pid");
    let pid_of = |old: Option<&str>| {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Ok(pid) = std::fs::read_to_string(&pid_file)
                && Some(pid.as_str()) != old
                && !pid.is_empty()
            {
                return pid;
            }
            assert!(Instant::now() < deadline, "the script wrote no pid");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    e.replace_with("never_hello.py");
    let first = pid_of(None);
    let pending = e.status().await;
    assert_eq!(pending["last_reload"]["result"], "pending");
    // A second save kills the pending child before starting another.
    let text = std::fs::read_to_string(e.script()).unwrap();
    std::fs::write(e.script(), format!("{text}# saved again\n")).unwrap();
    let second = pid_of(Some(&first));
    gone(&json!(first.parse::<u64>().unwrap()), 5.0).await;
    let failed = e
        .wait_for(45, "failed load at the deadline", |s| {
            s["last_reload"]["result"] == "failed_to_load"
        })
        .await;
    let output = failed["last_reload"]["output"].to_string();
    assert!(output.contains("sent no hello within 30 s"), "{output}");
    gone(&json!(second.parse::<u64>().unwrap()), 5.0).await;
    assert_eq!(failed["child"]["pid"], before["child"]["pid"]);
    assert_eq!(failed["child"]["state"], "running");
    assert!(alive(&before["child"]["pid"]));
}

#[tokio::test]
async fn an_automatic_id_added_first_renumbers_the_next_microgrid() {
    let Some(e) = Engine::start("two_microgrids.py", &[]) else {
        return;
    };
    let before = e.running().await;
    assert_eq!(component_ids(&e.spec(61).await), vec![11, 12, 1000, 1001]);
    let south = component_ids(&e.spec(62).await);
    assert!(south.contains(&1002), "{south:?}");
    e.edit(
        "mc.make_meter(power=Power.from_kilowatts(2)),",
        "mc.make_meter(power=Power.from_kilowatts(2)), mc.make_meter(),",
    );
    let after = e.reloaded(&before, 15).await;
    assert_eq!(after["last_reload"]["result"], "ok", "{after:#}");
    let north = component_ids(&e.spec(61).await);
    assert!(north.contains(&1002), "{north:?}");
    let south = component_ids(&e.spec(62).await);
    assert!(south.contains(&1003) && !south.contains(&1002), "{south:?}");
}

/// Edit the script so that its next run starts a process that ignores
/// SIGTERM, wait for the reload that edit causes, and return the
/// status that shows the process started, and the process's pid.
async fn start_stubborn_process(e: &Engine) -> (Value, Value) {
    let first = e.running().await;
    e.edit(
        "    print(\"running\", sorted(microgrids))",
        "    import subprocess\n    \
         sub = subprocess.Popen([\"sh\", \"-c\", \"trap '' TERM; sleep 20; :\"])\n    \
         print(\"sub\", sub.pid, flush=True)",
    );
    let started = e
        .wait_for(10, "started subprocess", |s| {
            s["child"]["pid"] != first["child"]["pid"]
                && s["output"].as_array().is_some_and(|o| {
                    o.iter()
                        .any(|l| l.as_str().is_some_and(|l| l.starts_with("sub ")))
                })
        })
        .await;
    let line = started["output"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|l| l.as_str()?.strip_prefix("sub "))
        .unwrap()
        .to_string();
    let sub = json!(line.trim().parse::<u64>().unwrap());
    assert!(alive(&sub));
    (started, sub)
}

/// A process the script started ends with the child that started it,
/// even one that ignores SIGTERM: the child leads a process group,
/// which is ended once it exits.
#[tokio::test]
async fn a_process_the_script_started_ends_with_a_reload() {
    let Some(e) = Engine::start("two_microgrids.py", &[]) else {
        return;
    };
    let (started, sub) = start_stubborn_process(&e).await;
    e.edit("sleep 20; :", "sleep 19; :");
    e.wait_for(10, "second reload", |s| {
        s["child"]["pid"] != started["child"]["pid"] && s["last_reload"]["result"] == "ok"
    })
    .await;
    gone(&sub, 3.0).await;
}

/// A Ctrl-C ends the script's process group before the engine exits.
#[tokio::test]
async fn ctrl_c_ends_a_process_the_script_started() {
    let Some(mut e) = Engine::start("two_microgrids.py", &[]) else {
        return;
    };
    let (_, sub) = start_stubborn_process(&e).await;
    ctrl_c(&mut e.child, 10).await;
    gone(&sub, 0.5).await;
}

#[tokio::test]
async fn ctrl_c_stops_the_child_and_the_engine() {
    let Some(mut e) = Engine::start("two_microgrids.py", &[]) else {
        return;
    };
    let status = e.running().await;
    ctrl_c(&mut e.child, 5).await;
    gone(&status["child"]["pid"], 1.0).await;
}

/// A Ctrl-C while the first script is still loading stops the child
/// and the engine at once, without waiting for the hello deadline.
#[tokio::test]
async fn ctrl_c_while_the_script_loads_stops_the_child_and_the_engine() {
    let Some(python) = common::venv_python() else {
        return;
    };
    let dir = script_dir("never_hello.py", &[]);
    std::fs::create_dir(dir.path().join("state")).unwrap();
    let mut child = spawn(dir.path(), &python);
    let pid_file = dir.path().join("s/plant.pid");
    let deadline = Instant::now() + Duration::from_secs(30);
    let pid = loop {
        if let Ok(pid) = std::fs::read_to_string(&pid_file)
            && let Ok(pid) = pid.trim().parse::<u64>()
        {
            break json!(pid);
        }
        assert!(Instant::now() < deadline, "the script wrote no pid");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    ctrl_c(&mut child, 5).await;
    gone(&pid, 1.0).await;
}

#[tokio::test]
async fn a_killed_engine_takes_its_child_with_it() {
    let Some(mut e) = Engine::start("two_microgrids.py", &[]) else {
        return;
    };
    let status = e.running().await;
    e.child.kill().unwrap();
    e.child.wait().unwrap();
    gone(&status["child"]["pid"], 1.0).await;
}

/// Run the binary on a script with `python`; its exit and stderr.
fn run_with_python(python: &Path) -> (std::process::ExitStatus, String) {
    let dir = script_dir("two_microgrids.py", &[]);
    let out = Command::new(env!("CARGO_BIN_EXE_macrocosim"))
        .args(["--ephemeral-ports", "--state-dir"])
        .arg(dir.path())
        .arg("--python")
        .arg(python)
        .arg(dir.path().join("s/plant.py"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (
        out.status,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_missing_interpreter_exits_with_the_install_message() {
    let (status, stderr) = run_with_python(Path::new("/nonexistent/python"));
    assert!(!status.success());
    assert!(
        stderr
            .contains("macrocosim needs Python 3.11 or later with the macrocosim package, version"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Found: /nonexistent/python (it does not run:"),
        "{stderr}"
    );
    assert!(stderr.contains("Install it, or pass --python."), "{stderr}");
}

#[test]
fn a_mismatched_package_exits_with_the_install_message() {
    let fake = Path::new(FIXTURES).join("fake-python.sh");
    let (status, stderr) = run_with_python(&fake);
    assert!(!status.success());
    assert!(
        stderr.contains(&format!("Found: {} (macrocosim 0.0.0)", fake.display())),
        "{stderr}"
    );
}

#[tokio::test]
async fn dropping_a_microgrid_from_the_script_unloads_it() {
    let Some(e) = Engine::start("two_microgrids.py", &[]) else {
        return;
    };
    let before = e.running().await;
    e.edit("MICROGRIDS = [NORTH, SOUTH]", "MICROGRIDS = [NORTH]");
    let after = e.reloaded(&before, 15).await;
    assert_eq!(after["last_reload"]["result"], "ok", "{after:#}");
    assert_eq!(after["microgrids"], json!([61]));
    assert_eq!(e.microgrid_ids().await, vec![61]);
}

#[tokio::test]
async fn a_script_may_not_claim_a_microgrid_made_elsewhere() {
    let Some(e) = Engine::start("two_microgrids.py", &[]) else {
        return;
    };
    let before = e.running().await;
    let r = e
        .client
        .post(format!("{}/api/microgrids", e.ui))
        .json(&json!({"name": "made in the UI", "id": 70}))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "{}", r.text().await.unwrap());
    e.replace_with("claims_ui_id.py");
    let after = e.reloaded(&before, 15).await;
    assert_eq!(after["last_reload"]["result"], "failed_to_load");
    let output = after["last_reload"]["output"].to_string();
    assert!(
        output.contains("microgrid 70 is not this script's"),
        "{output}"
    );
    assert_eq!(after["child"]["pid"], before["child"]["pid"]);
    assert!(alive(&before["child"]["pid"]));
    assert_eq!(e.microgrid_ids().await, vec![61, 62, 70]);
}

#[tokio::test]
async fn a_put_and_a_hello_together_both_complete() {
    let Some(e) = Engine::start("two_microgrids.py", &[]) else {
        return;
    };
    let before = e.running().await;
    let mut edited = e.spec(61).await;
    edited["name"] = json!("edited in the UI");
    e.edit("name=\"north\"", "name=\"north again\"");
    let put = e
        .client
        .put(format!("{}/api/mg/61/spec", e.ui))
        .json(&edited)
        .send();
    let (put, after) = tokio::join!(put, e.reloaded(&before, 15));
    let put = put.unwrap();
    assert!(put.status().is_success(), "{}", put.text().await.unwrap());
    assert_eq!(after["last_reload"]["result"], "ok", "{after:#}");
    // One applied after the other; the spec is the later one's, and
    // only the PUT leaves the microgrid unsaved.
    let list = e.get("/api/microgrids").await;
    let entry = list
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == 61)
        .unwrap()
        .clone();
    let name = e.spec(61).await["name"].clone();
    if entry["unsaved"] == true {
        assert_eq!(name, "edited in the UI");
    } else {
        assert_eq!(name, "north again");
    }
}
