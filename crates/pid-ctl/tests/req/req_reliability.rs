//! Reliability & Operational Safety — `pid-ctl_plan.md` (numbered principles).

#[test]
#[ignore = "no ControllerApp / tick pipeline yet"]
fn pv_read_failure_invokes_safe_cv_or_hold_last_per_plan() {
    todo!("principle 3: fail-safe on PV loss");
}

#[test]
#[ignore = "no tick pipeline yet"]
fn cv_write_failure_policy_matches_mode_once_loop_pipe() {
    todo!("item 17: exit codes, last_cv confirmed-applied semantics");
}
