//! The binary exits at boot: when a boot microgrid's gRPC port is taken, when
//! it is given 0.1's Lisp inputs, and when it is given a script.

use std::ffi::OsStr;
use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};
use std::path::Path;
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

/// Run the binary with `args` until it exits, failing the test after
/// 20 s; the exit status and everything it printed.
fn run_to_exit(dir: &Path, args: &[&OsStr]) -> (ExitStatus, String) {
    let log_path = dir.join("out.log");
    let out = std::fs::File::create(&log_path).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_macrocosim"))
        .args(args)
        .stdout(out.try_clone().unwrap())
        .stderr(out)
        .spawn()
        .unwrap();
    // A binary that stops exiting would serve forever: fail instead
    // of hanging the test run.
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            let log = std::fs::read_to_string(&log_path).unwrap_or_default();
            panic!("the binary did not exit:\n{log}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    (status, std::fs::read_to_string(&log_path).unwrap())
}

#[test]
fn a_boot_microgrid_on_a_held_port_exits_the_binary() {
    let holder = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
    let port = holder.local_addr().unwrap().port();
    let dir = tempfile::TempDir::with_prefix("mc-bootfail-").unwrap();
    let topology = dir.path().join("topology.json");
    std::fs::write(
        &topology,
        format!(
            r#"{{"schema_version": 1, "microgrids": [
                {{"id": 51, "grpc_port": {port}, "components": []}}]}}"#
        ),
    )
    .unwrap();
    let (status, log) = run_to_exit(
        dir.path(),
        &[
            "--ui-port".as_ref(),
            "0".as_ref(),
            "--assets-addr".as_ref(),
            "[::1]:0".as_ref(),
            "--dispatch-addr".as_ref(),
            "[::1]:0".as_ref(),
            "--state-dir".as_ref(),
            dir.path().as_os_str(),
            "--topology".as_ref(),
            topology.as_os_str(),
        ],
    );
    assert!(!status.success(), "the binary must exit with an error");
    assert!(
        log.contains("Microgrid #51") && log.contains(&port.to_string()),
        "{log}"
    );
    drop(holder);
}

/// Run the binary on a free UI port with `dir` as its state dir and `extra`
/// arguments, check that it exits with an error before anything binds, and
/// return what it printed.
fn run_refused(dir: &Path, extra: &[&OsStr]) -> String {
    let ui = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let ui_port = ui.local_addr().unwrap().port().to_string();
    drop(ui);
    let mut args: Vec<&OsStr> = vec![
        "--ui-port".as_ref(),
        ui_port.as_ref(),
        "--state-dir".as_ref(),
        dir.as_os_str(),
    ];
    args.extend_from_slice(extra);
    let (status, log) = run_to_exit(dir, &args);
    assert!(!status.success(), "the binary must exit with an error");
    assert!(!log.contains("listening"), "nothing may bind: {log}");
    log
}

/// A `.lisp` argument, or a state directory holding `enterprise.lisp`
/// or `microgrids/*.lisp`, exits the binary before anything binds,
/// naming the files found.
#[test]
fn lisp_inputs_exit_the_binary_before_anything_binds() {
    let run = |dir: &Path, extra: &[&OsStr]| {
        let log = run_refused(dir, extra);
        assert!(
            log.contains("macrocosim 0.2 no longer reads Lisp; support ended with 0.1."),
            "{log}"
        );
        log
    };

    let dir = tempfile::TempDir::with_prefix("mc-old-inputs-").unwrap();
    let log = run(dir.path(), &["site.lisp".as_ref()]);
    assert!(log.contains("Found: site.lisp."), "{log}");
    assert!(
        !log.contains("--state-dir"),
        "an argument is not the state dir's: {log}"
    );

    std::fs::create_dir(dir.path().join("microgrids")).unwrap();
    std::fs::write(dir.path().join("microgrids/7.lisp"), "").unwrap();
    std::fs::write(dir.path().join("enterprise.lisp"), "").unwrap();
    let log = run(dir.path(), &[]);
    let found = format!(
        "Found: {}, {}.",
        dir.path().join("enterprise.lisp").display(),
        dir.path().join("microgrids/7.lisp").display()
    );
    assert!(log.contains(&found), "{log}");
    assert!(log.contains("or pass --state-dir"), "{log}");
}

/// A `.py` argument exits the binary before anything binds, saying to run the
/// script with `python` against the simulator.
#[test]
fn a_script_argument_exits_the_binary_before_anything_binds() {
    let dir = tempfile::TempDir::with_prefix("mc-script-arg-").unwrap();
    let log = run_refused(dir.path(), &["plant.py".as_ref()]);
    assert!(
        log.contains(
            "macrocosim no longer runs scripts: start macrocosim, then run python plant.py"
        ),
        "{log}"
    );
}
