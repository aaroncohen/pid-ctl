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

/// Launch the TUI at a specific terminal size, wait for it to render, send 'q', and return
/// the combined PTY output. Text strings appear literally in the byte stream (ANSI codes only
/// wrap attributes/colors, not the text itself), so `contains("GAINS")` etc. work directly.
fn run_tune_pty_at_size(rows: u16, cols: u16) -> String {
    let dir = tempdir().expect("tempdir");
    let (bin, args) = tune_pty_bin_and_args(dir.path());

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");

    let mut cmd = CommandBuilder::new(bin);
    for a in &args {
        cmd.arg(a);
    }

    let mut child = pair.slave.spawn_command(cmd).expect("spawn_command");
    drop(pair.slave);

    let reader = pair.master.try_clone_reader().expect("reader");
    let drain = drain_master(reader);

    let mut writer = pair.master.take_writer().expect("writer");
    thread::sleep(Duration::from_millis(500));
    writer.write_all(b"q").expect("write q");
    writer.flush().ok();
    drop(writer);

    let status = wait_child_timeout(&mut child, Duration::from_secs(20));
    let combined = drain.join().expect("drain join");

    assert!(
        status.success(),
        "unexpected exit at {cols}×{rows}: {status:?}"
    );
    combined
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

/// At 80×15 the sparklines should collapse but gains must still be rendered.
#[test]
fn loop_tune_pty_tiny_terminal_shows_gains_not_history() {
    let combined = run_tune_pty_at_size(15, 80);
    assert!(
        combined.contains("GAINS"),
        "GAINS missing in 80×15 PTY output (len {})",
        combined.len()
    );
    assert!(
        !combined.contains("HISTORY"),
        "HISTORY unexpectedly present in 80×15 PTY output (len {})",
        combined.len()
    );
}

/// At a standard 80×24 terminal all three sections should be visible.
#[test]
fn loop_tune_pty_standard_terminal_shows_all_sections() {
    let combined = run_tune_pty_at_size(24, 80);
    assert!(
        combined.contains("GAINS"),
        "GAINS missing in 80×24 PTY output (len {})",
        combined.len()
    );
    assert!(
        combined.contains("HISTORY"),
        "HISTORY missing in 80×24 PTY output (len {})",
        combined.len()
    );
    assert!(
        combined.contains("PROCESS"),
        "PROCESS missing in 80×24 PTY output (len {})",
        combined.len()
    );
}

/// A wide 200×30 terminal should render cleanly and exit zero.
#[test]
fn loop_tune_pty_wide_terminal_renders_ok() {
    let combined = run_tune_pty_at_size(30, 200);
    assert!(
        combined.contains("GAINS"),
        "GAINS missing in 200×30 PTY output (len {})",
        combined.len()
    );
    assert!(
        combined.contains("HISTORY"),
        "HISTORY missing in 200×30 PTY output (len {})",
        combined.len()
    );
}
