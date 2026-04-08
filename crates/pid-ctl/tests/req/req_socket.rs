//! Integration tests for the Unix domain socket control API — bead pid-ctl-19f.

use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;
use tempfile::tempdir;

/// RAII guard that kills a child process on drop, ensuring cleanup even on test failure.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

/// Send a raw JSON request to the socket and return the raw JSON response.
fn socket_request(path: &Path, request: &str) -> String {
    let mut stream = UnixStream::connect(path).expect("connect to socket");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

/// Poll until the socket file appears and is connectable.
fn wait_for_socket(path: &Path, timeout: Duration) {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if path.exists() && UnixStream::connect(path).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "socket did not appear within {timeout:?} at {}",
        path.display()
    );
}

/// Spawn a `pid-ctl loop` with common flags and a socket.
/// Returns the child (wrapped in a guard) and the paths used.
fn spawn_loop(
    pv_path: &Path,
    cv_path: &Path,
    state_path: Option<&Path>,
    socket_path: &Path,
    extra_args: &[&str],
) -> ChildGuard {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_pid-ctl"));
    cmd.args(["loop", "--pv-file"])
        .arg(pv_path)
        .args(["--setpoint", "60", "--kp", "1.0", "--ki", "0.1", "--kd", "0.0"])
        .args(["--out-min", "0", "--out-max", "100"])
        .arg("--cv-file")
        .arg(cv_path)
        .args(["--interval", "100ms"]);

    if let Some(sp) = state_path {
        cmd.arg("--state").arg(sp);
    }

    cmd.arg("--socket").arg(socket_path);

    for arg in extra_args {
        cmd.arg(arg);
    }

    cmd.stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());

    let child = cmd.spawn().expect("spawn pid-ctl loop");
    ChildGuard(child)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn test_socket_status() {
    let dir = tempdir().expect("temp dir");
    let pv_path = dir.path().join("pv.txt");
    let cv_path = dir.path().join("cv.txt");
    let socket_path = dir.path().join("ctrl.sock");

    std::fs::write(&pv_path, "50.0\n").expect("write pv");

    let _guard = spawn_loop(&pv_path, &cv_path, None, &socket_path, &[]);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    // Let at least one tick complete.
    std::thread::sleep(Duration::from_millis(200));

    let raw = socket_request(&socket_path, r#"{"cmd":"status"}"#);
    let v: serde_json::Value = serde_json::from_str(raw.trim()).expect("parse response JSON");

    assert_eq!(v["ok"], true);
    assert!(
        v["iter"].as_u64().unwrap() >= 1,
        "expected iter >= 1, got {}",
        v["iter"]
    );
    assert_eq!(v["kp"], 1.0);
    assert_eq!(v["ki"], 0.1);
    assert_eq!(v["kd"], 0.0);
    assert_eq!(v["sp"], 60.0);
}

#[test]
fn test_socket_set_kp() {
    let dir = tempdir().expect("temp dir");
    let pv_path = dir.path().join("pv.txt");
    let cv_path = dir.path().join("cv.txt");
    let socket_path = dir.path().join("ctrl.sock");

    std::fs::write(&pv_path, "50.0\n").expect("write pv");

    let _guard = spawn_loop(&pv_path, &cv_path, None, &socket_path, &[]);
    wait_for_socket(&socket_path, Duration::from_secs(5));
    std::thread::sleep(Duration::from_millis(200));

    let raw = socket_request(&socket_path, r#"{"cmd":"set","param":"kp","value":2.5}"#);
    let v: serde_json::Value = serde_json::from_str(raw.trim()).expect("parse set response");

    assert_eq!(v["ok"], true);
    assert_eq!(v["param"], "kp");
    assert_eq!(v["old"], 1.0);
    assert_eq!(v["new"], 2.5);

    // Wait a tick then verify via status.
    std::thread::sleep(Duration::from_millis(150));

    let raw = socket_request(&socket_path, r#"{"cmd":"status"}"#);
    let v: serde_json::Value = serde_json::from_str(raw.trim()).expect("parse status response");
    assert_eq!(v["kp"], 2.5);
}

#[test]
fn test_socket_set_sp() {
    let dir = tempdir().expect("temp dir");
    let pv_path = dir.path().join("pv.txt");
    let cv_path = dir.path().join("cv.txt");
    let socket_path = dir.path().join("ctrl.sock");

    std::fs::write(&pv_path, "50.0\n").expect("write pv");

    let _guard = spawn_loop(&pv_path, &cv_path, None, &socket_path, &[]);
    wait_for_socket(&socket_path, Duration::from_secs(5));
    std::thread::sleep(Duration::from_millis(200));

    let raw = socket_request(&socket_path, r#"{"cmd":"set","param":"sp","value":75.0}"#);
    let v: serde_json::Value = serde_json::from_str(raw.trim()).expect("parse set response");

    assert_eq!(v["ok"], true);
    assert_eq!(v["param"], "sp");
    assert_eq!(v["old"], 60.0);
    assert_eq!(v["new"], 75.0);

    std::thread::sleep(Duration::from_millis(150));

    let raw = socket_request(&socket_path, r#"{"cmd":"status"}"#);
    let v: serde_json::Value = serde_json::from_str(raw.trim()).expect("parse status response");
    assert_eq!(v["sp"], 75.0);
}

#[test]
fn test_socket_reset() {
    let dir = tempdir().expect("temp dir");
    let pv_path = dir.path().join("pv.txt");
    let cv_path = dir.path().join("cv.txt");
    let socket_path = dir.path().join("ctrl.sock");

    // PV != SP so error is nonzero, i_acc will accumulate.
    std::fs::write(&pv_path, "50.0\n").expect("write pv");

    let _guard = spawn_loop(&pv_path, &cv_path, None, &socket_path, &[]);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    // Let several ticks run so i_acc accumulates (ki=0.1, error=10).
    std::thread::sleep(Duration::from_millis(500));

    let raw = socket_request(&socket_path, r#"{"cmd":"reset"}"#);
    let v: serde_json::Value = serde_json::from_str(raw.trim()).expect("parse reset response");

    assert_eq!(v["ok"], true);
    let i_acc_before = v["i_acc_before"].as_f64().expect("i_acc_before should be a number");
    assert!(
        i_acc_before.abs() > 0.001,
        "expected nonzero i_acc_before after several ticks, got {i_acc_before}"
    );

    // Wait a tick, check that i_acc is now near zero (one tick may have added a small amount).
    std::thread::sleep(Duration::from_millis(150));

    let raw = socket_request(&socket_path, r#"{"cmd":"status"}"#);
    let v: serde_json::Value = serde_json::from_str(raw.trim()).expect("parse status response");
    let i_acc = v["i_acc"].as_f64().expect("i_acc should be a number");
    // After reset, at most one tick's worth of integral (error * dt ~ 10 * 0.1 = 1.0).
    assert!(
        i_acc.abs() < i_acc_before.abs(),
        "i_acc after reset ({i_acc}) should be much smaller than before ({i_acc_before})"
    );
}

#[test]
fn test_socket_hold_resume() {
    let dir = tempdir().expect("temp dir");
    let pv_path = dir.path().join("pv.txt");
    let cv_path = dir.path().join("cv.txt");
    let socket_path = dir.path().join("ctrl.sock");

    std::fs::write(&pv_path, "50.0\n").expect("write pv");

    let _guard = spawn_loop(&pv_path, &cv_path, None, &socket_path, &[]);
    wait_for_socket(&socket_path, Duration::from_secs(5));
    std::thread::sleep(Duration::from_millis(200));

    // Send hold.
    let raw = socket_request(&socket_path, r#"{"cmd":"hold"}"#);
    let v: serde_json::Value = serde_json::from_str(raw.trim()).expect("parse hold response");
    assert_eq!(v["ok"], true);

    // While held, status should still work.
    std::thread::sleep(Duration::from_millis(150));
    let raw = socket_request(&socket_path, r#"{"cmd":"status"}"#);
    let v: serde_json::Value = serde_json::from_str(raw.trim()).expect("parse status while held");
    assert_eq!(v["ok"], true);

    // Resume.
    let raw = socket_request(&socket_path, r#"{"cmd":"resume"}"#);
    let v: serde_json::Value = serde_json::from_str(raw.trim()).expect("parse resume response");
    assert_eq!(v["ok"], true);
}

#[test]
fn test_socket_save() {
    let dir = tempdir().expect("temp dir");
    let pv_path = dir.path().join("pv.txt");
    let cv_path = dir.path().join("cv.txt");
    let state_path = dir.path().join("ctrl.json");
    let socket_path = dir.path().join("ctrl.sock");

    std::fs::write(&pv_path, "50.0\n").expect("write pv");

    let _guard = spawn_loop(&pv_path, &cv_path, Some(&state_path), &socket_path, &[]);
    wait_for_socket(&socket_path, Duration::from_secs(5));
    std::thread::sleep(Duration::from_millis(200));

    // Send save.
    let raw = socket_request(&socket_path, r#"{"cmd":"save"}"#);
    let v: serde_json::Value = serde_json::from_str(raw.trim()).expect("parse save response");
    assert_eq!(v["ok"], true);

    // State file should exist and contain valid JSON.
    assert!(state_path.exists(), "state file should exist after save");
    let content = std::fs::read_to_string(&state_path).expect("read state file");
    let state: serde_json::Value =
        serde_json::from_str(&content).expect("state file should be valid JSON");
    // Should have the kp we started with.
    assert_eq!(state["kp"], 1.0);
}

#[test]
fn test_socket_unknown_command() {
    let dir = tempdir().expect("temp dir");
    let pv_path = dir.path().join("pv.txt");
    let cv_path = dir.path().join("cv.txt");
    let socket_path = dir.path().join("ctrl.sock");

    std::fs::write(&pv_path, "50.0\n").expect("write pv");

    let _guard = spawn_loop(&pv_path, &cv_path, None, &socket_path, &[]);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let raw = socket_request(&socket_path, r#"{"cmd":"foobar"}"#);
    let v: serde_json::Value = serde_json::from_str(raw.trim()).expect("parse error response");

    assert_eq!(v["ok"], false);
    assert!(
        v["error"].as_str().is_some(),
        "error field should be present"
    );
    assert!(
        v["available"].is_array(),
        "available field should list commands"
    );
}

#[test]
fn test_socket_unknown_param() {
    let dir = tempdir().expect("temp dir");
    let pv_path = dir.path().join("pv.txt");
    let cv_path = dir.path().join("cv.txt");
    let socket_path = dir.path().join("ctrl.sock");

    std::fs::write(&pv_path, "50.0\n").expect("write pv");

    let _guard = spawn_loop(&pv_path, &cv_path, None, &socket_path, &[]);
    wait_for_socket(&socket_path, Duration::from_secs(5));
    std::thread::sleep(Duration::from_millis(200));

    let raw = socket_request(
        &socket_path,
        r#"{"cmd":"set","param":"foobar","value":1.0}"#,
    );
    let v: serde_json::Value = serde_json::from_str(raw.trim()).expect("parse error response");

    assert_eq!(v["ok"], false);
    assert!(
        v["error"].as_str().is_some(),
        "error field should be present"
    );
    assert!(
        v["settable"].is_array(),
        "settable field should list params"
    );
    let settable: Vec<&str> = v["settable"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(settable.contains(&"kp"));
    assert!(settable.contains(&"ki"));
    assert!(settable.contains(&"kd"));
    assert!(settable.contains(&"sp"));
    assert!(settable.contains(&"interval"));
}

#[test]
fn test_status_via_socket() {
    let dir = tempdir().expect("temp dir");
    let pv_path = dir.path().join("pv.txt");
    let cv_path = dir.path().join("cv.txt");
    let socket_path = dir.path().join("ctrl.sock");

    std::fs::write(&pv_path, "50.0\n").expect("write pv");

    let _guard = spawn_loop(&pv_path, &cv_path, None, &socket_path, &[]);
    wait_for_socket(&socket_path, Duration::from_secs(5));
    std::thread::sleep(Duration::from_millis(200));

    // Run `pid-ctl status --socket <path>` as a subprocess.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_pid-ctl"))
        .args(["status", "--socket"])
        .arg(&socket_path)
        .output()
        .expect("run status command");

    assert!(
        output.status.success(),
        "status --socket should exit 0, got {:?}; stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout should be valid JSON");
    assert_eq!(v["ok"], true);
}

#[test]
fn test_status_socket_fallback() {
    let dir = tempdir().expect("temp dir");
    let state_path = dir.path().join("ctrl.json");
    let nonexistent_socket = dir.path().join("nonexistent.sock");

    // Write a state file that `pid-ctl status --state` can read.
    let pv_path = dir.path().join("pv.txt");
    let cv_path = dir.path().join("cv.txt");
    let socket_path = dir.path().join("tmp.sock");

    std::fs::write(&pv_path, "50.0\n").expect("write pv");

    // Start a loop briefly to produce a state file, then kill it.
    {
        let mut guard = spawn_loop(&pv_path, &cv_path, Some(&state_path), &socket_path, &[]);
        wait_for_socket(&socket_path, Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(300));
        // Force a save before killing.
        let _ = socket_request(&socket_path, r#"{"cmd":"save"}"#);
        std::thread::sleep(Duration::from_millis(100));
        guard.0.kill().ok();
        guard.0.wait().ok();
    }

    assert!(
        state_path.exists(),
        "state file should exist after loop ran"
    );

    // Create a stale socket file at `nonexistent_socket` so that `connect()` returns
    // ConnectionRefused (not NotFound). The fallback only triggers on ConnectionRefused.
    {
        let _listener = std::os::unix::net::UnixListener::bind(&nonexistent_socket)
            .expect("bind stale socket for fallback test");
    }
    assert!(nonexistent_socket.exists());

    // Now run status with a stale socket and a valid state file.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_pid-ctl"))
        .args(["status", "--socket"])
        .arg(&nonexistent_socket)
        .arg("--state")
        .arg(&state_path)
        .output()
        .expect("run status command");

    assert!(
        output.status.success(),
        "status should fall back to state file, got {:?}; stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // State file format uses different field names than socket response — just verify valid JSON.
    let _v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout should be valid JSON");
}

#[test]
fn test_socket_already_running() {
    let dir = tempdir().expect("temp dir");
    let pv_path = dir.path().join("pv.txt");
    let cv_path = dir.path().join("cv.txt");
    let socket_path = dir.path().join("ctrl.sock");

    std::fs::write(&pv_path, "50.0\n").expect("write pv");

    let _guard1 = spawn_loop(&pv_path, &cv_path, None, &socket_path, &[]);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    // Try to start a second loop with the same socket.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_pid-ctl"))
        .args(["loop", "--pv-file"])
        .arg(&pv_path)
        .args(["--setpoint", "60", "--kp", "1.0", "--ki", "0.1", "--kd", "0.0"])
        .args(["--out-min", "0", "--out-max", "100"])
        .arg("--cv-file")
        .arg(&cv_path)
        .args(["--interval", "100ms"])
        .arg("--socket")
        .arg(&socket_path)
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn second loop");

    assert_eq!(
        output.status.code(),
        Some(3),
        "second instance should exit with code 3; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_socket_stale_cleanup() {
    let dir = tempdir().expect("temp dir");
    let pv_path = dir.path().join("pv.txt");
    let cv_path = dir.path().join("cv.txt");
    let socket_path = dir.path().join("ctrl.sock");

    std::fs::write(&pv_path, "50.0\n").expect("write pv");

    // Create a stale socket by binding and immediately dropping a listener.
    {
        let _listener =
            std::os::unix::net::UnixListener::bind(&socket_path).expect("bind stale socket");
    }
    assert!(socket_path.exists(), "stale socket file should exist");

    // Starting a loop should succeed — stale socket gets cleaned up.
    let _guard = spawn_loop(&pv_path, &cv_path, None, &socket_path, &[]);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    // Verify the socket is functional.
    let raw = socket_request(&socket_path, r#"{"cmd":"status"}"#);
    let v: serde_json::Value = serde_json::from_str(raw.trim()).expect("parse response");
    assert_eq!(v["ok"], true);
}

#[test]
fn test_socket_mode_permissions() {
    let dir = tempdir().expect("temp dir");
    let pv_path = dir.path().join("pv.txt");
    let cv_path = dir.path().join("cv.txt");
    let socket_path = dir.path().join("ctrl.sock");

    std::fs::write(&pv_path, "50.0\n").expect("write pv");

    let _guard = spawn_loop(
        &pv_path,
        &cv_path,
        None,
        &socket_path,
        &["--socket-mode", "0660"],
    );
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let meta = std::fs::symlink_metadata(&socket_path).expect("socket metadata");
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o660,
        "socket mode should be 0660, got {mode:#o}"
    );
}
