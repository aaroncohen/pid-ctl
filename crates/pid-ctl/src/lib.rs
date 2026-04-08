//! Application orchestration for **pid-ctl**: tick pipeline, I/O adapters, persistence, locking.
//!
//! Implementation is intentionally not started here — behavior is driven by requirement tests and
//! `pid-ctl_plan.md` (Reliability & Operational Safety, Full CLI Reference, State File Schema).

#![forbid(unsafe_code)]

pub mod adapters;
pub mod app;
pub mod json_events;
pub mod schedule;
pub mod socket;
