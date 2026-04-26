//! Application orchestration for **pid-ctl**: tick pipeline, I/O adapters, persistence, locking.
//!
//! Implementation is intentionally not started here — behavior is driven by requirement tests and
//! `pid-ctl_plan.md` (Reliability & Operational Safety, Full CLI Reference, State File Schema).
//!
//! # No stable API contract
//!
//! Everything re-exported from this crate (`adapters`, `app`, `json_events`, `schedule`,
//! `socket`) is the binary's shared plumbing — not a library surface for third-party
//! consumers. Names, signatures, and module layout may change with any commit to serve
//! the `pid-ctl` CLI's needs. If you need a stable library to embed PID control, depend
//! on `pid-ctl-core` instead.

#![forbid(unsafe_code)]

pub mod adapters;
pub mod app;
pub mod autotune;
pub mod json_events;
pub mod schedule;
#[cfg(unix)]
pub mod socket;
