//! Named defaults for loop timing parameters.
//!
//! These values were previously duplicated as bare literals across `parse.rs`,
//! `loop_runtime.rs`, `main.rs`, and `socket.rs`. Consolidating them here makes
//! the policy auditable in one place and eliminates drift between the parse-time
//! default and the runtime re-derivation inside `apply_runtime_interval`.
//!
//! Corresponds to the defaults described in `pid-ctl_plan.md` (Reliability &
//! Operational Safety, §dt handling and §state flush coalescing).

use std::time::Duration;

/// Minimum accepted measured `dt` in seconds. Samples below this are skipped
/// (or clamped under `--dt-clamp`) to avoid derivative blow-up.
pub const MIN_DT_DEFAULT: f64 = 0.01;

/// Maximum accepted measured `dt` in seconds. Samples above this are skipped
/// (or clamped under `--dt-clamp`) to avoid integrator runaway.
pub const MAX_DT_DEFAULT: f64 = 60.0;

/// Default `max_dt` is this multiple of the configured `--interval`, capped at
/// `MAX_DT_DEFAULT` and floored at `MIN_DT_DEFAULT`.
pub const MAX_DT_INTERVAL_MULTIPLIER: f64 = 3.0;

/// Minimum state-flush interval. Always honoured even when `--interval` is
/// shorter, so disk I/O does not saturate on sub-100 ms tick rates.
pub const MIN_STATE_FLUSH: Duration = Duration::from_millis(100);

/// Upper bound for the default `--cmd-timeout` (it is `min(interval, this)`).
pub const DEFAULT_CMD_TIMEOUT_CAP: Duration = Duration::from_secs(30);

/// Per-request I/O timeout on the operator socket. Short enough to not stall
/// the tick, long enough to absorb typical fork/exec latency.
pub const SOCKET_IO_TIMEOUT: Duration = Duration::from_millis(500);

/// Chunk size used when the tick sleep is serviced alongside socket polls.
pub const SOCKET_SLEEP_CHUNK: Duration = Duration::from_millis(50);

/// Default tracking-time upper bound for anti-windup, expressed as a multiple
/// of the loop interval (so faster loops tolerate shorter `Tt`).
pub const ANTI_WINDUP_TT_INTERVAL_MULTIPLIER: f64 = 100.0;
