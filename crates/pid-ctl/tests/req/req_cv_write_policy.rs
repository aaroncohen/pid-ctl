//! CV write failure exit codes — `pid-ctl_plan.md` (Exit Codes, Reliability item 17).

use assert_cmd::Command;
use pid_ctl::app::{StateSnapshot, StateStore};
use predicates::str::contains;
use tempfile::tempdir;

#[test]
fn once_exits_5_on_cv_write_failure() {
    let tempdir = tempdir().expect("temporary directory");
    let state_path = tempdir.path().join("fan.json");
    let bad_cv_path = tempdir.path().join("missing").join("cv.txt");
    let store = StateStore::new(&state_path);

    store
        .save(&StateSnapshot {
            last_cv: Some(41.0),
            ..StateSnapshot::default()
        })
        .expect("seed state");

    let mut cmd = Command::cargo_bin("pid-ctl").expect("pid-ctl binary");
    cmd.args([
        "once",
        "--pv",
        "60.0",
        "--setpoint",
        "55.0",
        "--kp",
        "2.0",
        "--out-min",
        "0.0",
        "--out-max",
        "100.0",
        "--cv-file",
    ]);
    cmd.arg(&bad_cv_path);
    cmd.arg("--state");
    cmd.arg(&state_path);

    cmd.assert().code(5).stderr(contains("CV write failed"));

    let persisted = store
        .load()
        .expect("state reload")
        .expect("snapshot persisted");
    assert_eq!(persisted.last_cv, Some(41.0));
    assert_eq!(persisted.out_min, Some(0.0));
    assert_eq!(persisted.out_max, Some(100.0));
    assert_eq!(persisted.iter, 1);
}
