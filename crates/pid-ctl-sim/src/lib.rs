//! Simulated plants for [`pid-ctl`](https://crates.io/crates/pid-ctl) closed-loop experiments.
//!
//! Use the **`pid-ctl-sim`** binary with `pid-ctl loop --pv-cmd` / `--cv-cmd`. Plant dynamics are
//! **pure** (explicit `dt`); there is no wall clock inside the model math.
//!
//! # Wiring for `loop` / `--tune`
//!
//! 1. `pid-ctl-sim init --state /path/plant.json --plant thermal` (or `first-order`, `fan`).
//! 2. Point **`--pv-cmd`** at `… print-pv --state /path/plant.json` (same binary for both commands).
//! 3. Point **`--cv-cmd`** at `… apply-cv --state /path/plant.json --dt SECONDS --cv {cv}` (negative
//!    CV values like `-0.03` are supported; `pid-ctl-sim` enables clap’s hyphen-value parsing on `--cv`).
//!    Use the same `SECONDS` as `pid-ctl loop --interval` (there is no `{dt}` placeholder in
//!    `pid-ctl`’s CV command today).
//!
//! **Paths:** `--pv-cmd` / `--cv-cmd` are run by `sh -c`; **`./foo` is relative to the directory
//! you started `pid-ctl` from**, not the repo layout. From the workspace root after `cargo build`,
//! use e.g. **`./target/debug/pid-ctl-sim`**, an absolute path, or put `pid-ctl-sim` on **`PATH`**
//! and invoke `pid-ctl-sim` with no `./` (using only `./pid-ctl-sim` from the repo root fails:
//! the binary lives under `target/debug/`, not next to `pid-ctl`).
//! 4. For a **closed-loop** plant, the actuator command must run: omit **`--dry-run`**, or in the
//!    tuning dashboard press **`d`** to turn dry-run off. With `--dry-run`, CV is not sent to
//!    `--cv-cmd`, so the plant state never updates.
//!
//! Example:
//!
//! ```text
//! ./target/debug/pid-ctl-sim init --state /tmp/plant.json --plant thermal --param tau=90 --param k_heat=0.015
//! ./target/debug/pid-ctl loop --tune \
//!   --pv-cmd "./target/debug/pid-ctl-sim print-pv --state /tmp/plant.json" \
//!   --cv-cmd "./target/debug/pid-ctl-sim apply-cv --state /tmp/plant.json --dt 0.5 --cv {cv}" \
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
