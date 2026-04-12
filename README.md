# pid-ctl

**pid-ctl** is a Rust workspace that provides a deterministic PID controller library, a command-line control loop for Unix-style automation, and optional simulated plants for tuning experiments. Use it to close the loop between a **process variable (PV)** you measure and a **control variable (CV)** you actuate—whether that is a shell command, a file, a pipe, or a simulator.

Full behavioral contracts, flag precedence, and I/O semantics are specified in [`pid-ctl_plan.md`](./pid-ctl_plan.md).

## Features

### Control core (`pid-ctl-core`)

- **Position-form PID** with **derivative on measurement** (reduces setpoint kick).
- **Output limits**, optional **CV slew-rate limiting**, and **setpoint ramping**.
- **PV low-pass filter** (configurable α), **error deadband**, and **anti-windup** (`back-calc`, `clamp`, or `none`) with optional back-calculation tracking time.
- **Pure, testable math**—no I/O inside the core; configuration is validated before stepping.

### CLI (`pid-ctl`)

- **`once`** — Run a **single** PID tick for scripting and tests (literal PV, file, or command-sourced PV; CV to stdout, file, or command).
- **`pipe`** — Read PV lines from **stdin**, write CV lines to **stdout** (composable in shell pipelines). Timing uses wall-clock spacing between lines (first tick uses `--dt`).
- **`loop`** — Periodic **control loop** with a wall-clock **interval**, PV from **file**, **command**, or **stdin** (one line per tick, with optional timeout).
- **`status`** — Inspect persisted controller state and/or query a running **`loop`** over a **Unix domain socket** (see below).
- **State file** — Optional JSON **persistence** (`--state`) with `init` / `purge`, configurable flush interval, and failure policies for disk writes.
- **Structured logging** — **NDJSON** events on stderr and/or `--log` (ISO8601 timestamps, schema version). Human-readable **text** or **JSON** summaries where applicable.
- **`--tune` (TUI)** — Interactive tuning dashboard (requires the default `tui` feature and a TTY): adjust gains, setpoint, and related options while the loop runs; sparkline-style history.
- **Unix-only: control socket** — While `loop` runs with `--socket`, use **`set`**, **`hold`**, **`resume`**, **`reset`**, and **`save`** to change parameters or coordinate shutdown without restarting the process.

### Simulator (`pid-ctl-sim`)

- **First-order**, **thermal**, and **fan** plant models with JSON state on disk.
- **`print-pv`** / **`apply-cv`** commands align with `pid-ctl loop --pv-cmd` / `--cv-cmd` for **closed-loop** experiments and **`loop --tune`**.

## Requirements

- **Rust** toolchain matching the workspace (`rust-version` in the root `Cargo.toml`; currently **1.85+**).
- **Unix** for socket-based remote commands and the default TUI tuning mode (terminal handling).

## Building

From the repository root:

```bash
cargo build --release -p pid-ctl -p pid-ctl-sim
```

Binaries are written to `target/release/pid-ctl` and `target/release/pid-ctl-sim`.

To build the CLI **without** the Ratatui-based tuning UI (for minimal or headless targets):

```bash
cargo build --release -p pid-ctl --no-default-features
```

Run `pid-ctl --help` and `pid-ctl <subcommand> --help` for the full flag list.

## Workspace layout

| Crate | Role |
|--------|------|
| [`pid-ctl-core`](./crates/pid-ctl-core) | Deterministic PID math and configuration (`PidController`, `PidConfig`, `step`). |
| [`pid-ctl`](./crates/pid-ctl) | CLI, adapters (files, commands, stdin/stdout), state store, JSON events, optional TUI. |
| [`pid-ctl-sim`](./crates/pid-ctl-sim) | Simulated plants and helper binary for loop/tune demos. |

## CLI overview

| Command | Purpose |
|---------|---------|
| `once` | One control tick; requires a PV source and (unless `--dry-run`) a CV sink. |
| `pipe` | Stream: stdin PV lines → stdout CV lines; optional `--state` / `--log`. |
| `loop` | Fixed-interval loop; PV from `--pv-file`, `--pv-cmd`, or `--pv-stdin`; CV via `--cv-stdout`, `--cv-file`, or `--cv-cmd`. |
| `status` | Show snapshot from `--state` and/or `--socket`. |
| `init` | Create or reset controller state JSON at `--state`. |
| `purge` | Delete the state file at `--state`. |
| `set` | *(Unix)* Send `kp`, `ki`, `kd`, `sp`, or `interval` to a running loop via `--socket`. |
| `hold` / `resume` / `reset` / `save` | *(Unix)* Socket commands to the running `loop`. |

Common PID-related flags (where supported) include `--setpoint`, `--kp`, `--ki`, `--kd`, `--out-min`, `--out-max`, `--deadband`, `--setpoint-ramp`, `--slew-rate`, `--pv-filter`, `--anti-windup`, `--anti-windup-tt`, `--dt`, `--min-dt`, `--max-dt`, `--dt-clamp`, `--scale`, `--cv-precision`, and `--reset-accumulator`. Durations accept forms like `500ms`, `2s`, or bare seconds.

## Examples

### One-shot tick (`once`)

Compute CV for a single measurement of `42.0`, send CV to stdout, default gains:

```bash
pid-ctl once --pv 42 --cv-stdout --setpoint 40 --kp 1 --ki 0.1 --kd 0
```

Dry-run (no CV sink): compute only, useful with `--format json` or `--log` for inspection.

### Pipeline (`pipe`)

Each non-empty stdin line is parsed as a PV sample; CV is written to stdout. Elapsed time between lines becomes `dt` after the first sample (the first tick uses `--dt`, default `1` second if unset):

```bash
printf '10\n11\n12\n' | pid-ctl pipe --setpoint 20 --kp 0.5 --ki 0 --kd 0
```

### Periodic loop with a file PV

Re-read a file each tick (application writes the current PV into the file):

```bash
pid-ctl loop \
  --interval 500ms \
  --pv-file /tmp/pv.txt \
  --cv-stdout \
  --setpoint 100 --kp 2 --ki 0.05 --kd 0.01 \
  --state /tmp/pid.json
```

### Loop with commands for PV and CV

Use `--pv-cmd` to run a command that prints one numeric line, and `--cv-cmd` to deliver the CV (exact substitution semantics are documented in `pid-ctl_plan.md`):

```bash
pid-ctl loop \
  --interval 1s \
  --pv-cmd "./read-sensor.sh" \
  --cv-cmd "./write-actuator.sh {cv}" \
  --setpoint 0 --kp 1 --ki 0 --kd 0
```

### Interactive tuning with the simulator

Initialize plant state, then run the controller in tuning mode with PV/CV wired to `pid-ctl-sim`. Use the **same** `--state` path for the plant JSON as in the upstream docs; match `--interval` on the loop with `--dt` on `apply-cv`.

```bash
cargo build -p pid-ctl -p pid-ctl-sim

./target/debug/pid-ctl-sim init --state /tmp/plant.json --plant thermal

./target/debug/pid-ctl loop --tune \
  --pv-cmd "./target/debug/pid-ctl-sim print-pv --state /tmp/plant.json" \
  --cv-cmd "./target/debug/pid-ctl-sim apply-cv --state /tmp/plant.json --dt 0.5 --cv {cv}" \
  --interval 500ms --setpoint 22 --kp 0.5 --ki 0.02 --kd 0
```

`--dry-run` on the loop suppresses CV output (the plant does not see the actuator). In the TUI you can also toggle behavior interactively; see `--help` for tune-related step sizes and `--tune-history`.

### Structured logs

Append NDJSON records to a file for post-processing:

```bash
pid-ctl loop --pv-file /tmp/pv.txt --cv-stdout --interval 1s --log /tmp/run.ndjson \
  --setpoint 0 --kp 1 --ki 0 --kd 0
```

### Unix socket (live loop)

Start the loop with a socket path, then from another terminal:

```bash
pid-ctl set --socket /tmp/pid.sock --param sp --value 75
pid-ctl status --socket /tmp/pid.sock
```

## Specification

Behavioral details—error codes, JSON schemas, edge cases for PV/CV commands, and locking—are **normative** in [`pid-ctl_plan.md`](./pid-ctl_plan.md).

## License

This project is licensed under the [MIT License](./LICENSE).
