# pid-ctl

Many automation tasks are feedback loops: read a measurement from a sensor, a file, or a small script; compare it to a target; adjust an output you control (heater duty, fan speed, valve position, PWM); repeat on a schedule or whenever new data arrives. **pid-ctl** implements the middle step—the control law—so you can wire **measurement → decision → output** into scripts, `cron`, `systemd`, and shell pipelines instead of a separate GUI or proprietary runtime.

The project ships as a CLI and a small Rust library. You configure a target, where to read the current value, and where to send the next control value, using files, subprocesses, standard I/O, or an optional simulator for dry runs. A **PID** controller is used under the hood: it combines proportional, integral, and derivative terms to decide how strongly to push the output so the measurement tracks the target. Gains, limits, and safety-related options are exposed as flags; a terminal dashboard is available for live tuning on `loop`.

Below, **PV** (*process variable*) means the measured value you read, and **CV** (*control variable*) means the value you send to the actuator—standard names in process control. The repository is a Rust workspace with three crates (core math, CLI, simulator). Normative behavior—exit codes, edge cases, JSON schemas—is in [`pid-ctl_plan.md`](./pid-ctl_plan.md).

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

## Design & opinions

This project is built for **operators and automation** who already live in shells, systemd, and ad hoc scripts—not as a replacement for a full PLC/SCADA stack. The shape of the tool follows from that.

**Unix composition over embedded UI.** The primary interface is a CLI that fits pipes, `cron`, and one-shot scripts. Long-running control uses **`loop`**; streaming or externally paced workloads use **`pipe`**; idempotent “tick and exit” jobs use **`once`**. We split those deliberately: mixing them would blur who owns timing (you, the wall clock, or the controller).

**`pipe` vs `loop --pv-stdin`.** In v1, **`pipe`** is a **pure stream transformer**: stdin PV lines in, stdout CV lines out, no sleeps, and **no built-in CV sink**—the next stage in the shell pipeline owns actuation. **`loop`** owns a **fixed interval**, deadline-style scheduling (drift does not accumulate as a backlog of ticks), and optional **socket control**, **TUI tuning**, and richer operational flags. If the stream should drive the cadence, use `pipe`; if the controller should drive the cadence and you want daemon-style behavior, use `loop` (with `--pv-stdin` when PV arrives on stdin under the loop’s timing).

**Determinism where it matters.** `pid-ctl-core` does **no I/O**: only validated configuration and `step` math. That keeps the control law **testable**, reproducible in requirements tests, and safe to reason about. All filesystem, subprocess, and terminal concerns live in `pid-ctl`, behind small adapters.

**Control-law defaults.** The implementation uses **position form** PID with **derivative on measurement** to reduce setpoint kick, plus explicit anti-windup, optional slew limits, and setpoint ramping. Those choices match common process-control practice; if you need a different formulation, the core is the place to swap or extend—not hidden inside the CLI.

**Operability and automation.** **JSON state** on disk and a **Unix-domain socket** API exist so you can script setpoint and gains, query status, and integrate with supervision—without reserving a TTY. Structured **NDJSON** logs and a versioned state schema favor **auditability** and downstream tooling (including LLM-assisted ops). The interactive **`--tune`** dashboard is optional (feature-gated) so headless builds stay lean.

**Safety posture.** The plan emphasizes **reliability**: bounded command timeouts, policies for consecutive PV/CV/state failures, optional **safe CV** on fault paths, and file locking around state. These are **opinionated guardrails** for real hardware or irreversible actuators; they trade some flexibility for predictable failure modes.

**Normative specification.** Behavior is not “discover by trial”: [`pid-ctl_plan.md`](./pid-ctl_plan.md) is the **contract** (exit codes, flag interactions, JSON shapes). The README is the map; the plan is the source of truth.

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

## Examples (by mode)

The snippets below are practical starting points. Substitution tokens (`broker`, paths to sysfs, etc.) must match your machine. See [`pid-ctl_plan.md`](./pid-ctl_plan.md) for `{cv}` / `{cv:url}`, timeouts, and exit codes.

### `once` — single tick and exit

Use when **you** own the schedule (`cron`, `udev`, a wrapper script). Each invocation reads PV once, updates optional `--state`, writes CV, and exits.

**Literal PV, CV on stdout** (handy for capturing in a variable):

```bash
REFLUX=$(pid-ctl once \
  --pv 78.4 --setpoint 78.1 \
  --kp 1.5 --ki 0.3 --kd 0 \
  --out-min 0 --out-max 100 \
  --state /tmp/still.json \
  --cv-stdout --quiet)
echo "command heater $REFLUX"
```

**PV from a file, CV to a file** (e.g. sysfs-style values):

```bash
pid-ctl once \
  --pv-file /tmp/sensor_pv.txt \
  --cv-file /tmp/actuator_cv.txt \
  --setpoint 100 --kp 2 --ki 0.1 --kd 0 \
  --state /tmp/pid.json
```

**PV and CV via shell commands** (each `--pv-cmd` must print one numeric line; `--cv-cmd` supports `{cv}` substitution):

```bash
pid-ctl once \
  --pv-cmd "curl -fsS http://sensor.local/temp | jq -r .c" \
  --cv-cmd 'mosquitto_pub -h broker -t heater/duty -m "{cv}"' \
  --setpoint 21 --kp 1 --ki 0.05 --kd 0 \
  --out-min 0 --out-max 100 \
  --state /var/lib/pid-ctl/room.json
```

**Dry-run** (compute CV but do not require a CV sink — combine with `--format json` or `--log` to inspect):

```bash
pid-ctl once --pv 20 --setpoint 22 --kp 1 --ki 0 --kd 0 --dry-run --format json
```

### `pipe` — stream in, stream out

**External process owns timing**: one PID tick per non-empty stdin line; CV goes to stdout. First tick uses `--dt` (default 1s if omitted); later ticks use **elapsed wall time** between lines. **No** `--cv-cmd` / `--cv-file` on `pipe` — pipe the output to the next command.

**Minimal check:**

```bash
printf '10\n11\n12\n' | pid-ctl pipe --setpoint 20 --kp 0.5 --ki 0 --kd 0
```

**Tail a log, actuate in the next pipeline stage:**

```bash
tail -f /var/log/sensor.log \
  | awk '/temp/ {print $4}' \
  | pid-ctl pipe --setpoint 60 --kp 0.5 --ki 0.1 --kd 0 --out-min 0 --out-max 255 \
  | xargs -I{} sh -c 'echo {} > /path/to/actuator'
```

**Synthetic stream for testing:**

```bash
seq 0 100 | awk '{print 20 + $1 * 0.1}' \
  | pid-ctl pipe --setpoint 25 --kp 2 --ki 0.2 --kd 0.1 --out-min 0 --out-max 100
```

### `loop` — fixed interval, daemon-style

**`--interval`** drives the schedule (deadline-based: no backlog of missed ticks). **PV** comes from exactly one of `--pv-file`, `--pv-cmd`, or `--pv-stdin`.

**File PV** (something else writes the current PV each tick):

```bash
pid-ctl loop \
  --interval 2s \
  --pv-file /tmp/pv.txt \
  --cv-stdout \
  --setpoint 55 --kp 0.8 --ki 0.05 --kd 0.2 \
  --out-min 0 --out-max 100 \
  --state /tmp/fan.json --name fan-demo
```

**Command PV and command CV** (HTTP/MQTT/CLI tools):

```bash
pid-ctl loop \
  --interval 5s \
  --pv-cmd "mosquitto_sub -h 192.168.1.10 -t still/temp -C 1" \
  --cv-cmd 'mosquitto_pub -h 192.168.1.10 -t still/reflux -m "{cv}"' \
  --setpoint 78.3 --kp 1.5 --ki 0.2 --kd 0.8 \
  --out-min 0 --out-max 100 \
  --state /var/lib/pid-ctl/still.json
```

**Stdin PV (`--pv-stdin`)** — the **loop’s timer** decides when each tick runs; each tick waits up to `--pv-stdin-timeout` for one line (use for serial streams where you still want `loop`’s interval, `--state`, `--socket`, or `--tune`):

```bash
cat /dev/ttyUSB0 \
  | pid-ctl loop --pv-stdin --pv-stdin-timeout 2s \
      --interval 5s \
      --cv-file /sys/class/hwmon/hwmon0/pwm1 \
      --setpoint 65 --kp 1 --ki 0.1 --kd 0.2 \
      --out-min 0 --out-max 255 \
      --state /tmp/serial-fan.json
```

### `loop --tune` — interactive dashboard

Requires a TTY and the default `tui` feature. **Only** valid on `loop` (not `once` or `pipe`).

**Live hardware or MQTT** (same as a normal `loop`, plus `--tune`):

```bash
pid-ctl loop \
  --pv-cmd "mosquitto_sub -h broker -t proc/temp -C 1" \
  --cv-cmd 'mosquitto_pub -h broker -t proc/cv -m "{cv}"' \
  --interval 5s --setpoint 78 --kp 1.5 --ki 0.2 --kd 0.8 \
  --out-min 0 --out-max 100 \
  --state /var/lib/pid-ctl/proc.json --units °C \
  --tune
```

**Closed loop with `pid-ctl-sim`** (match `--interval` to `--dt` on `apply-cv`; use paths to your built binaries):

```bash
cargo build -p pid-ctl -p pid-ctl-sim

./target/debug/pid-ctl-sim init --state /tmp/plant.json --plant thermal

./target/debug/pid-ctl loop --tune \
  --pv-cmd "./target/debug/pid-ctl-sim print-pv --state /tmp/plant.json" \
  --cv-cmd "./target/debug/pid-ctl-sim apply-cv --state /tmp/plant.json --dt 0.5 --cv {cv}" \
  --interval 500ms --setpoint 22 --kp 0.5 --ki 0.02 --kd 0
```

Use **`--dry-run`** on the loop to suppress CV output so the simulated plant does not move; you can also toggle behavior inside the TUI.

### State file: `init`, `purge`, `status`

**Create or reset** a controller state file (fails if another process holds the lock):

```bash
pid-ctl init --state /tmp/pid.json
```

**Wipe runtime integrator/history fields** while keeping gains (see plan for exact fields):

```bash
pid-ctl purge --state /tmp/pid.json
```

**Inspect persisted snapshot** (offline):

```bash
pid-ctl status --state /tmp/pid.json
```

### Unix socket — live `loop` control

Start **`loop`** with **`--socket`** (Unix only). From other terminals you can query and command the running process without a TTY.

**Terminal A — controller:**

```bash
pid-ctl loop \
  --interval 2s \
  --pv-file /tmp/pv.txt \
  --cv-stdout \
  --setpoint 50 --kp 1 --ki 0.1 --kd 0 \
  --state /tmp/pid.json \
  --socket /tmp/pid.sock --socket-mode 0600
```

**Terminal B — operator / automation:**

```bash
pid-ctl status --socket /tmp/pid.sock
pid-ctl set --socket /tmp/pid.sock --param sp --value 75
pid-ctl hold --socket /tmp/pid.sock
pid-ctl resume --socket /tmp/pid.sock
pid-ctl reset --socket /tmp/pid.sock
pid-ctl save --socket /tmp/pid.sock
```

`set` accepts `--param` values such as `kp`, `ki`, `kd`, `sp`, and `interval` (see `--help`).

**Status with fallback** — try the socket first, then read `--state` if the loop is not running:

```bash
pid-ctl status --socket /tmp/pid.sock --state /tmp/pid.json
```

### Structured logs (`--log`)

NDJSON events go to stderr and/or a file; use for dashboards or post-processing:

```bash
pid-ctl loop \
  --pv-file /tmp/pv.txt --cv-stdout --interval 1s \
  --setpoint 0 --kp 1 --ki 0 --kd 0 \
  --log /tmp/run.ndjson
```

## Specification

Behavioral details—error codes, JSON schemas, edge cases for PV/CV commands, and locking—are **normative** in [`pid-ctl_plan.md`](./pid-ctl_plan.md).

## License

This project is licensed under the [MIT License](./LICENSE).
