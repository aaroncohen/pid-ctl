//! PTY smoke tests for `loop --tune` — exercises the dashboard past CLI validation (`pid-ctl_plan.md`, bead pid-ctl-8vb.9.8).

use assert_cmd::cargo::cargo_bin;
use portable_pty::{Child, CommandBuilder, ExitStatus, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::tempdir;

fn tune_pty_bin_and_args(dir: &std::path::Path) -> (PathBuf, Vec<String>) {
    let pv = dir.join("pv.txt");
    std::fs::write(&pv, "50.0\n").expect("pv file");
    let bin = cargo_bin("pid-ctl");
    let args = vec![
        "loop".into(),
        "--pv-file".into(),
        pv.to_string_lossy().into_owned(),
        "--setpoint".into(),
        "55.0".into(),
        "--kp".into(),
        "1.0".into(),
        "--ki".into(),
        "0.0".into(),
        "--kd".into(),
        "0.0".into(),
        "--interval".into(),
        "1s".into(),
        "--dry-run".into(),
        "--tune".into(),
    ];
    (bin, args)
}

fn drain_master(mut reader: Box<dyn Read + Send>) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut buf = [0u8; 16384];
        let mut acc = Vec::new();
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => acc.extend_from_slice(&buf[..n]),
            }
        }
        String::from_utf8_lossy(&acc).into_owned()
    })
}

fn wait_child_timeout(child: &mut Box<dyn Child + Send + Sync>, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) => {}
            Err(e) => panic!("try_wait: {e}"),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child did not exit within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn loop_tune_pty_quit_prints_export_and_exits_zero() {
    let dir = tempdir().expect("tempdir");
    let (bin, args) = tune_pty_bin_and_args(dir.path());

    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize::default()).expect("openpty");

    let mut cmd = CommandBuilder::new(bin);
    for a in &args {
        cmd.arg(a);
    }

    let mut child = pair.slave.spawn_command(cmd).expect("spawn_command");

    drop(pair.slave);

    let reader = pair.master.try_clone_reader().expect("reader");
    let drain = drain_master(reader);

    let mut writer = pair.master.take_writer().expect("writer");
    // Let the TUI initialize (raw mode + alternate screen) before input.
    thread::sleep(Duration::from_millis(250));
    writer.write_all(b"q").expect("write q");
    writer.flush().ok();
    drop(writer);

    let status = wait_child_timeout(&mut child, Duration::from_secs(20));
    let combined = drain.join().expect("drain join");

    assert!(status.success(), "unexpected exit: {status:?}");
    assert_eq!(status.exit_code(), 0);
    assert!(
        combined.contains("Tuned gains only:"),
        "expected stderr export sentinel in PTY capture; got len {}",
        combined.len()
    );
}

/// Ctrl+C / SIGINT should take the same clean shutdown path as `q` (`ctrlc` handler + export).
#[cfg(unix)]
#[test]
fn loop_tune_pty_sigint_prints_export_and_exits_zero() {
    let dir = tempdir().expect("tempdir");
    let (bin, args) = tune_pty_bin_and_args(dir.path());

    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize::default()).expect("openpty");

    let mut cmd = CommandBuilder::new(bin);
    for a in &args {
        cmd.arg(a);
    }

    let mut child = pair.slave.spawn_command(cmd).expect("spawn_command");

    drop(pair.slave);

    let reader = pair.master.try_clone_reader().expect("reader");
    let drain = drain_master(reader);

    thread::sleep(Duration::from_millis(250));

    let pid = child.process_id().expect("child pid");
    let st = Command::new("kill")
        .args(["-INT", &pid.to_string()])
        .status()
        .expect("kill");
    assert!(st.success(), "kill -INT failed: {st:?}");

    let status = wait_child_timeout(&mut child, Duration::from_secs(20));
    let combined = drain.join().expect("drain join");

    assert!(status.success(), "unexpected exit: {status:?}");
    assert_eq!(status.exit_code(), 0);
    assert!(
        combined.contains("Tuned gains only:"),
        "expected stderr export sentinel in PTY capture after SIGINT; got len {}",
        combined.len()
    );
}
