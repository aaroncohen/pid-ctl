//! `pid-ctl-sim` — simulated plants for `pid-ctl loop --pv-cmd` / `--cv-cmd`.
//!
//! # Closed loop with `pid-ctl loop --tune`
//!
//! Use **`--pv-cmd`** and **`--cv-cmd`** pointing at this binary. Pass the **same** `--state` path
//! to both subcommands. Set `--dt` on `apply-cv` to match your controller `--interval` (seconds).
//!
//! **`--dry-run` disables CV output** in the tuning TUI — the plant never sees the actuator. For a
//! live simulated plant, omit `--dry-run` or press `d` in the dashboard to turn dry-run off.
//!
//! Example (run from workspace root after `cargo build -p pid-ctl -p pid-ctl-sim`):
//!
//! ```text
//! ./target/debug/pid-ctl-sim init --state /tmp/plant.json --plant thermal
//! ./target/debug/pid-ctl loop --tune \
//!   --pv-cmd "./target/debug/pid-ctl-sim print-pv --state /tmp/plant.json" \
//!   --cv-cmd "./target/debug/pid-ctl-sim apply-cv --state /tmp/plant.json --dt 0.5 --cv {cv}" \
//!   --interval 500ms --setpoint 22 --kp 0.5 --ki 0.02 --kd 0
//! ```
//!
//! Use **`./target/debug/pid-ctl-sim`** (or an absolute path): `./pid-ctl-sim` only works if your
//! shell cwd is the directory that **contains** that binary (e.g. `target/debug`), not the repo root.
//!
//! # Tick ordering
//!
//! `pid-ctl` reads PV, computes PID, then writes CV. Here, **`print-pv`** returns the measurement
//! **before** this tick’s `apply-cv`; **`apply-cv`** runs after the controller emits CV and
//! advances the plant for one step using that CV and `--dt`.

#![forbid(unsafe_code)]

use clap::{Parser, Subcommand, ValueEnum};
use pid_ctl_sim::{
    FanParams, FirstOrderParams, Plant, SCHEMA_VERSION, SimError, SimState, ThermalParams,
};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::exit;

#[derive(Parser)]
#[command(
    name = "pid-ctl-sim",
    version,
    about = "Simulated plants for pid-ctl loop / tuning"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create or overwrite a `--state` JSON file with defaults (optional `--param` overrides).
    Init {
        #[arg(long, value_name = "PATH")]
        state: PathBuf,
        #[arg(long, value_enum)]
        plant: PlantKind,
        /// Override defaults, e.g. `tau=120`, `k_heat=0.02`
        #[arg(long = "param", value_name = "KEY=VALUE")]
        params: Vec<String>,
    },
    /// Print the current PV (one line, parseable as f64) for `--pv-cmd`.
    PrintPv {
        #[arg(long, value_name = "PATH")]
        state: PathBuf,
    },
    /// Advance the plant one step using controller CV (for `--cv-cmd` with `{cv}`).
    ApplyCv {
        #[arg(long, value_name = "PATH")]
        state: PathBuf,
        /// Seconds — should match `pid-ctl` `--interval` when tuning (no `{dt}` in cv-cmd yet).
        #[arg(long)]
        dt: f64,
        /// Controller output (may be negative — must not be parsed as a new flag after `--cv`).
        #[arg(long, allow_hyphen_values = true)]
        cv: f64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum PlantKind {
    FirstOrder,
    Thermal,
    Fan,
}

fn main() {
    if let Err(e) = run(Cli::parse()) {
        let _ = writeln!(io::stderr(), "{e}");
        exit(1);
    }
}

fn run(cli: Cli) -> Result<(), SimError> {
    match cli.command {
        Command::Init {
            state,
            plant,
            params,
        } => {
            let overrides = parse_param_overrides(&params)?;
            let p = build_plant(plant, &overrides)?;
            p.validate_params()?;
            let sim = SimState {
                schema_version: SCHEMA_VERSION,
                plant: p,
            };
            sim.validate()?;
            write_state(&state, &sim)?;
            Ok(())
        }
        Command::PrintPv { state } => {
            let sim = read_state(&state)?;
            sim.validate()?;
            let pv = sim.plant.pv();
            println!("{pv}");
            Ok(())
        }
        Command::ApplyCv { state, dt, cv } => {
            let mut sim = read_state(&state)?;
            sim.validate()?;
            sim.plant.apply_cv(cv, dt)?;
            sim.validate()?;
            write_state(&state, &sim)?;
            Ok(())
        }
    }
}

fn read_state(path: &std::path::Path) -> Result<SimState, SimError> {
    pid_ctl_sim::load_state(path).map_err(|e| SimError::Io {
        context: format!("read {}", path.display()),
        source: e,
    })
}

fn write_state(path: &std::path::Path, sim: &SimState) -> Result<(), SimError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| SimError::Io {
            context: String::from("create_dir_all"),
            source: e,
        })?;
    }
    pid_ctl_sim::save_state(path, sim).map_err(|e| SimError::Io {
        context: format!("write {}", path.display()),
        source: e,
    })
}

fn parse_param_overrides(raw: &[String]) -> Result<HashMap<String, f64>, SimError> {
    let mut m = HashMap::new();
    for s in raw {
        let (k, v) = s.split_once('=').ok_or_else(|| {
            SimError::Validation(format!("invalid --param {s:?}, expected KEY=VALUE"))
        })?;
        let key = k.trim().to_string();
        let val: f64 = v
            .trim()
            .parse()
            .map_err(|e| SimError::Validation(format!("parse {s:?}: {e}")))?;
        m.insert(key, val);
    }
    Ok(m)
}

fn build_plant(kind: PlantKind, o: &HashMap<String, f64>) -> Result<Plant, SimError> {
    let g = |k: &str| o.get(k).copied();

    match kind {
        PlantKind::FirstOrder => {
            let params = FirstOrderParams {
                tau: g("tau").unwrap_or(1.0),
                gain: g("gain").unwrap_or(1.0),
            };
            let x = g("x").unwrap_or(0.0);
            params.validate()?;
            Ok(Plant::FirstOrder { params, x })
        }
        PlantKind::Thermal => {
            let params = ThermalParams {
                tau: g("tau").unwrap_or(60.0),
                t_ambient: g("t_ambient").unwrap_or(20.0),
                k_heat: g("k_heat").unwrap_or(0.01),
            };
            let t = g("t").unwrap_or(params.t_ambient);
            params.validate()?;
            Ok(Plant::Thermal { params, t })
        }
        PlantKind::Fan => {
            let params = FanParams {
                tau: g("tau").unwrap_or(2.0),
                max_flow: g("max_flow").unwrap_or(100.0),
                cv_max: g("cv_max").unwrap_or(100.0),
                exponent: g("exponent").unwrap_or(1.5),
            };
            let speed = g("speed").unwrap_or(0.0);
            params.validate()?;
            Ok(Plant::Fan { params, speed })
        }
    }
}
