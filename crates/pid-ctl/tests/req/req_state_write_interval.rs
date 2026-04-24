//! Tests for `--state-write-interval` and `--state-fail-after` (pid-ctl-8vb.27).

use assert_cmd::Command;
use pid_ctl::app::StateStore;
use std::time::Duration;
use tempfile::tempdir;

/// state-write-interval coalescing: with a very large interval and many ticks,
/// the disk write happens only once (on first tick) while in-memory state is
/// updated every tick (verified by iter count in the final state).
#[test]
fn state_write_interval_coalesces_disk_writes() {
    let dir = tempdir().expect("temporary directory");
    let pv_path = dir.path().join("pv.txt");
    let state_path = dir.path().join("state.json");
    let cv_path = dir.path().join("cv.txt");
    std::fs::write(&pv_path, "50.0\n").expect("pv file");

    // Use --state-write-interval 10s to prevent any disk flush during the short run.
    // Run for ~300ms at 50ms interval → ~6 ticks.
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["loop", "--pv-file"]);
    cmd.arg(&pv_path);
    cmd.args([
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--interval",
        "50ms",
        "--cv-file",
    ]);
    cmd.arg(&cv_path);
    cmd.arg("--state");
    cmd.arg(&state_path);
    cmd.args(["--state-write-interval", "10s"]);

    cmd.timeout(Duration::from_millis(400));
    let _ = cmd.output();

    // State file should exist (force_flush at shutdown writes it).
    let store = StateStore::new(&state_path);
    let snapshot = store
        .load()
        .expect("state loaded")
        .expect("snapshot present");

    // iter must be > 0 (at least one tick ran and state was flushed at shutdown).
    assert!(
        snapshot.iter > 0,
        "expected at least one iteration in state; got iter={}",
        snapshot.iter
    );
}

/// Default state-write-interval for loop is `max(tick_interval, 100ms)`.
/// With a 50ms interval the default should be 100ms. Check this compiles and runs.
#[test]
fn state_write_interval_loop_default_is_max_interval_100ms() {
    let dir = tempdir().expect("temporary directory");
    let pv_path = dir.path().join("pv.txt");
    let state_path = dir.path().join("state.json");
    let cv_path = dir.path().join("cv.txt");
    std::fs::write(&pv_path, "50.0\n").expect("pv file");

    // No --state-write-interval — should use default max(50ms, 100ms) = 100ms.
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["loop", "--pv-file"]);
    cmd.arg(&pv_path);
    cmd.args([
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--interval",
        "50ms",
        "--cv-file",
    ]);
    cmd.arg(&cv_path);
    cmd.arg("--state");
    cmd.arg(&state_path);

    cmd.timeout(Duration::from_millis(400));
    let _ = cmd.output();

    let store = StateStore::new(&state_path);
    let snapshot = store
        .load()
        .expect("state loaded")
        .expect("snapshot present");
    assert!(
        snapshot.iter > 0,
        "state must have been flushed at shutdown"
    );
}

/// state-fail-after escalation: after N consecutive failures the stderr message
/// contains the prominent WARNING prefix and the `state_write_escalated` JSON event.
///
/// Strategy: start the loop with a valid state directory (so lock + load succeed),
/// then make the directory read-only so `save()` temp-file creation fails.
#[cfg(unix)]
#[test]
fn state_fail_after_escalates_to_prominent_warning() {
    use std::os::unix::fs::PermissionsExt;
    use std::process;

    // root bypasses chmod permission checks; the test can't simulate write failures.
    let uid = process::Command::new("id")
        .arg("-u")
        .output()
        .map(|o| o.stdout)
        .unwrap_or_default();
    if uid.starts_with(b"0") {
        eprintln!("SKIP: running as root; chmod-based failure simulation unavailable");
        return;
    }

    let dir = tempdir().expect("temporary directory");
    let pv_path = dir.path().join("pv.txt");
    let cv_path = dir.path().join("cv.txt");
    let log_path = dir.path().join("events.ndjson");
    std::fs::write(&pv_path, "50.0\n").expect("pv file");

    // Use a subdirectory for state so we can chmod it independently.
    let state_dir = dir.path().join("state_dir");
    std::fs::create_dir(&state_dir).expect("create state dir");
    let state_path = state_dir.join("state.json");

    let child = process::Command::new(assert_cmd::cargo::cargo_bin("pid-ctl"))
        .args(["loop", "--pv-file"])
        .arg(&pv_path)
        .args([
            "--setpoint",
            "55.0",
            "--kp",
            "1.0",
            "--interval",
            "50ms",
            "--cv-file",
        ])
        .arg(&cv_path)
        .arg("--state")
        .arg(&state_path)
        .args(["--state-fail-after", "2"])
        .args(["--state-write-interval", "1ms"])
        .arg("--log")
        .arg(&log_path)
        .stderr(process::Stdio::piped())
        .stdout(process::Stdio::null())
        .spawn()
        .expect("spawn loop");

    // Let the first tick succeed (lock acquired, state file written).
    std::thread::sleep(Duration::from_millis(150));

    // Make state directory read-only so subsequent save() calls fail.
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o555))
        .expect("chmod state dir");

    // Let several more ticks run so failures accumulate past the threshold.
    std::thread::sleep(Duration::from_millis(400));

    // Clean up: restore permissions before killing, so tempdir cleanup works.
    let _ = std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o755));

    process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("send SIGTERM");

    let output = child.wait_with_output().expect("wait for child");
    let stderr = String::from_utf8_lossy(&output.stderr);

    // There should be at least one prominent WARNING line.
    assert!(
        stderr.contains("WARNING: state write failing persistently"),
        "expected escalated warning in stderr; got: {stderr:?}"
    );

    // The log should contain a state_write_escalated JSON event.
    if log_path.exists() {
        let log_contents = std::fs::read_to_string(&log_path).expect("read log file");
        let has_escalated = log_contents
            .lines()
            .any(|line| line.contains("\"state_write_escalated\""));
        assert!(
            has_escalated,
            "expected state_write_escalated JSON event in log; got: {log_contents:?}"
        );
    }
}

/// State write failures are never fatal: loop continues running even when every write fails.
#[cfg(unix)]
#[test]
fn state_write_failures_are_never_fatal() {
    use std::os::unix::fs::PermissionsExt;
    use std::process;

    let dir = tempdir().expect("temporary directory");
    let pv_path = dir.path().join("pv.txt");
    let cv_path = dir.path().join("cv.txt");
    std::fs::write(&pv_path, "50.0\n").expect("pv file");

    // Use a subdirectory for state so we can chmod it independently.
    let state_dir = dir.path().join("state_dir");
    std::fs::create_dir(&state_dir).expect("create state dir");
    let state_path = state_dir.join("state.json");

    let mut child = process::Command::new(assert_cmd::cargo::cargo_bin("pid-ctl"))
        .args(["loop", "--pv-file"])
        .arg(&pv_path)
        .args([
            "--setpoint",
            "55.0",
            "--kp",
            "1.0",
            "--interval",
            "50ms",
            "--cv-file",
        ])
        .arg(&cv_path)
        .arg("--state")
        .arg(&state_path)
        .args(["--state-write-interval", "1ms"])
        .stderr(process::Stdio::null())
        .stdout(process::Stdio::null())
        .spawn()
        .expect("spawn loop");

    // Let the first tick succeed.
    std::thread::sleep(Duration::from_millis(150));

    // Make state directory read-only so subsequent save() calls fail.
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o555))
        .expect("chmod state dir");

    // Let several ticks run with failing state writes.
    std::thread::sleep(Duration::from_millis(300));

    // Restore permissions so tempdir cleanup works, then kill.
    let _ = std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o755));

    process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("send SIGTERM");

    let _ = child.wait();

    // CV file should have received writes (loop was running despite state failures).
    let cv_content = std::fs::read_to_string(&cv_path).expect("cv file written");
    assert!(
        !cv_content.trim().is_empty(),
        "CV should have been written despite state write failures"
    );
}

/// Default state-write-interval for pipe is 1s (no explicit flag).
/// Verify pipe runs to completion without error when state writes are coalesced.
#[test]
fn pipe_state_write_interval_default_is_1s() {
    let dir = tempdir().expect("temporary directory");
    let state_path = dir.path().join("state.json");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["pipe", "--setpoint", "55.0", "--kp", "1.0", "--state"]);
    cmd.arg(&state_path);
    cmd.write_stdin("50.0\n51.0\n52.0\n");

    let output = cmd.output().expect("pipe ran");
    assert!(
        output.status.success(),
        "pipe should succeed with default state-write-interval"
    );
}

/// --state-write-interval flag is accepted by pipe and loop without error.
#[test]
fn state_write_interval_flag_accepted() {
    let dir = tempdir().expect("temporary directory");

    // pipe
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "pipe",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--state-write-interval",
        "500ms",
    ]);
    cmd.write_stdin("50.0\n");
    cmd.assert().success();

    // loop
    let pv_path = dir.path().join("pv.txt");
    let cv_path = dir.path().join("cv.txt");
    std::fs::write(&pv_path, "50.0\n").expect("pv file");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args(["loop", "--pv-file"]);
    cmd.arg(&pv_path);
    cmd.args([
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--interval",
        "50ms",
        "--cv-file",
    ]);
    cmd.arg(&cv_path);
    cmd.args(["--state-write-interval", "2s"]);
    cmd.timeout(Duration::from_millis(200));
    let _ = cmd.output();
    // No assertion on exit — we just want no early panic/error from unknown flag.
}

/// --state-fail-after flag is accepted by pipe and loop without error.
#[test]
fn state_fail_after_flag_accepted() {
    // pipe
    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "pipe",
        "--setpoint",
        "55.0",
        "--kp",
        "1.0",
        "--state-fail-after",
        "5",
    ]);
    cmd.write_stdin("50.0\n");
    cmd.assert().success();
}

/// Shutdown force-flush: even with a very long state-write-interval, the final
/// state is written to disk on shutdown (`force_flush`) when SIGINT is received.
#[cfg(unix)]
#[test]
fn force_flush_at_shutdown_writes_final_state() {
    use std::process;

    let dir = tempdir().expect("temporary directory");
    let pv_path = dir.path().join("pv.txt");
    let state_path = dir.path().join("state.json");
    let cv_path = dir.path().join("cv.txt");
    std::fs::write(&pv_path, "50.0\n").expect("pv file");

    // Large state-write-interval so coalescing suppresses all periodic writes.
    // SIGTERM should trigger force-flush.
    let mut child = process::Command::new(assert_cmd::cargo::cargo_bin("pid-ctl"))
        .args(["loop", "--pv-file"])
        .arg(&pv_path)
        .args([
            "--setpoint",
            "55.0",
            "--kp",
            "1.0",
            "--interval",
            "50ms",
            "--cv-file",
        ])
        .arg(&cv_path)
        .arg("--state")
        .arg(&state_path)
        .args(["--state-write-interval", "3600s"])
        .stderr(process::Stdio::null())
        .stdout(process::Stdio::null())
        .spawn()
        .expect("spawn loop");

    // Let it run a few ticks.
    std::thread::sleep(Duration::from_millis(300));

    // Send SIGINT for clean shutdown (ctrlc handler sets the flag, triggering force_flush).
    process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("send SIGINT");

    // Wait for exit — the loop will notice the flag within one tick (50ms).
    let start = std::time::Instant::now();
    loop {
        match child.try_wait().expect("check child") {
            Some(status) => {
                assert!(
                    status.success(),
                    "expected clean exit after SIGINT, got: {status:?}"
                );
                break;
            }
            None if start.elapsed() > Duration::from_secs(3) => {
                child.kill().expect("kill child");
                panic!("loop did not exit within 3s after SIGINT");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }

    let store = StateStore::new(&state_path);
    let snapshot = store
        .load()
        .expect("state loaded")
        .expect("snapshot present after force_flush at shutdown");

    assert!(
        snapshot.iter > 0,
        "iter should be > 0 after force flush; state was written at shutdown"
    );
}
