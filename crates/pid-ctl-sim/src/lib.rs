//! Simulated plants for [`pid-ctl`](https://crates.io/crates/pid-ctl) closed-loop experiments.
//!
//! Use the **`pid-ctl-sim`** binary with `pid-ctl loop --pv-cmd` / `--cv-cmd`. Plant dynamics are
//! **pure** (explicit `dt`); there is no wall clock inside the model math.
//!
//! # Wiring for `loop` / `--tune`
//!
//! 1. `pid-ctl-sim init --state /path/plant.json --plant thermal` (or `first-order`, `fan`).
//! 2. Point **`--pv-cmd`** at `pid-ctl-sim print-pv --state /path/plant.json`.
//! 3. Point **`--cv-cmd`** at `pid-ctl-sim apply-cv --state /path/plant.json --dt SECONDS --cv {cv}`.
//!    Use the same `SECONDS` as `pid-ctl loop --interval` (there is no `{dt}` placeholder in
//!    `pid-ctl`’s CV command today).
//! 4. For a **closed-loop** plant, the actuator command must run: omit **`--dry-run`**, or in the
//!    tuning dashboard press **`d`** to turn dry-run off. With `--dry-run`, CV is not sent to
//!    `--cv-cmd`, so the plant state never updates.
//!
//! Example:
//!
//! ```text
//! pid-ctl-sim init --state /tmp/plant.json --plant thermal --param tau=90 --param k_heat=0.015
//! pid-ctl loop --tune \
//!   --pv-cmd "pid-ctl-sim print-pv --state /tmp/plant.json" \
//!   --cv-cmd "pid-ctl-sim apply-cv --state /tmp/plant.json --dt 0.5 --cv {cv}" \
//!   --interval 500ms --setpoint 23 --kp 0.8 --ki 0.03 --kd 0
//! ```
//!
//! # Discrete-time semantics vs `pid-ctl loop`
//!
//! Each controller tick: **read PV** → PID step → **write CV**. This harness splits into:
//!
//! - **`print-pv`:** current PV from disk (state after the *previous* tick’s `apply-cv`).
//! - **`apply-cv`:** integrate one step with the CV the controller just wrote.
//!
//! The first `print-pv` after `init` reflects the initial condition; each `apply-cv` advances the
//! plant for one tick.

#![forbid(unsafe_code)]

mod persist;
mod plant;
mod state;

pub use persist::{load as load_state, save as save_state};
pub use plant::{
    step_fan, step_first_order, step_thermal, FanParams, FirstOrderParams, ThermalParams,
};
pub use state::{Plant, SimState, SCHEMA_VERSION};
