use crate::LoopArgs;
use pid_ctl::app;
use pid_ctl_core::PidConfig;
use std::collections::VecDeque;
use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::tune) enum GainFocus {
    Kp,
    Ki,
    Kd,
    Sp,
}

/// Sparkline gain-change annotation: merges within 3 ticks; `marker_tick` selects the `|` column.
#[derive(Clone, Debug)]
pub(in crate::tune) struct GainAnnotation {
    /// Tick column for the `|` marker (latest tick in the merge group).
    pub(in crate::tune) marker_tick: u64,
    pub(in crate::tune) kp: Option<(f64, f64)>,
    pub(in crate::tune) ki: Option<(f64, f64)>,
    pub(in crate::tune) kd: Option<(f64, f64)>,
    pub(in crate::tune) sp: Option<(f64, f64)>,
}

impl GainAnnotation {
    pub(in crate::tune) fn display_text(&self) -> String {
        let mut parts = Vec::new();
        if let Some((f, t)) = self.kp {
            parts.push(format!("Kp {f:.3}→{t:.3}"));
        }
        if let Some((f, t)) = self.ki {
            parts.push(format!("Ki {f:.3}→{t:.3}"));
        }
        if let Some((f, t)) = self.kd {
            parts.push(format!("Kd {f:.3}→{t:.3}"));
        }
        if let Some((f, t)) = self.sp {
            parts.push(format!("SP {f:.3}→{t:.3}"));
        }
        parts.join("  ")
    }
}

impl GainFocus {
    pub(in crate::tune) const fn next(self) -> Self {
        match self {
            Self::Kp => Self::Ki,
            Self::Ki => Self::Kd,
            Self::Kd => Self::Sp,
            Self::Sp => Self::Kp,
        }
    }

    pub(in crate::tune) const fn prev(self) -> Self {
        match self {
            Self::Kp => Self::Sp,
            Self::Ki => Self::Kp,
            Self::Kd => Self::Ki,
            Self::Sp => Self::Kd,
        }
    }

    pub(in crate::tune) const fn idx(self) -> usize {
        match self {
            Self::Kp => 0,
            Self::Ki => 1,
            Self::Kd => 2,
            Self::Sp => 3,
        }
    }
}

// The four bools are independent, orthogonal flags (command_mode, help_overlay, hold, quit/dry_run).
// An enum-per-flag state machine would add complexity with no clarity gain here.
#[allow(clippy::struct_excessive_bools)]
pub(in crate::tune) struct TuneUiState {
    pub(in crate::tune) focus: GainFocus,
    pub(in crate::tune) step: [f64; 4],
    pub(in crate::tune) command_mode: bool,
    pub(in crate::tune) command_buf: String,
    pub(in crate::tune) help_overlay: bool,
    pub(in crate::tune) hold: bool,
    pub(in crate::tune) dry_run: bool,
    pub(in crate::tune) last_record: Option<app::IterationRecord>,
    pub(in crate::tune) pv_history: VecDeque<f64>,
    pub(in crate::tune) cv_history: VecDeque<f64>,
    pub(in crate::tune) sp_history: VecDeque<f64>,
    /// Parallel to PV/CV/SP history — tick id for sparkline column mapping.
    pub(in crate::tune) serial_history: VecDeque<u64>,
    pub(in crate::tune) tick_serial: u64,
    pub(in crate::tune) annotations: VecDeque<GainAnnotation>,
    /// Last-known sparkline width (terminal columns). Updated each render so
    /// history is kept long enough to fill the screen even when `tune_history` is small.
    pub(in crate::tune) spark_w: usize,
    pub(in crate::tune) last_kp: f64,
    pub(in crate::tune) last_ki: f64,
    pub(in crate::tune) last_kd: f64,
    pub(in crate::tune) last_sp: f64,
    pub(in crate::tune) start: Instant,
    pub(in crate::tune) quit: bool,
    pub(in crate::tune) status_flash: Option<(String, Instant)>,
    pub(in crate::tune) export_overlay: Option<String>,
}

impl TuneUiState {
    pub(in crate::tune) fn new(args: &LoopArgs) -> Self {
        Self {
            focus: GainFocus::Kp,
            step: [
                args.tune_step_kp,
                args.tune_step_ki,
                args.tune_step_kd,
                args.tune_step_sp,
            ],
            command_mode: false,
            command_buf: String::new(),
            help_overlay: false,
            hold: false,
            dry_run: args.dry_run,
            last_record: None,
            pv_history: VecDeque::new(),
            cv_history: VecDeque::new(),
            sp_history: VecDeque::new(),
            serial_history: VecDeque::new(),
            tick_serial: 0,
            annotations: VecDeque::new(),
            spark_w: args.tune_history,
            last_kp: f64::NAN,
            last_ki: f64::NAN,
            last_kd: f64::NAN,
            last_sp: f64::NAN,
            start: Instant::now(),
            quit: false,
            status_flash: None,
            export_overlay: None,
        }
    }

    pub(in crate::tune) fn push_history(&mut self, args: &LoopArgs, pv: f64, cv: f64, sp: f64) {
        // Keep enough history to fill the terminal width even when tune_history < spark_w.
        let cap = args.tune_history.max(self.spark_w);
        while self.pv_history.len() >= cap {
            self.pv_history.pop_front();
            self.cv_history.pop_front();
            self.sp_history.pop_front();
            self.serial_history.pop_front();
        }
        self.tick_serial = self.tick_serial.saturating_add(1);
        self.pv_history.push_back(pv);
        self.cv_history.push_back(cv);
        self.sp_history.push_back(sp);
        self.serial_history.push_back(self.tick_serial);
        while self.annotations.len() > cap {
            self.annotations.pop_front();
        }
    }

    pub(in crate::tune) fn note_gain_change(&mut self, _args: &LoopArgs, cfg: &PidConfig) {
        fn gain_delta(prev: f64, curr: f64) -> Option<(f64, f64)> {
            (prev.is_finite() && (curr - prev).abs() > f64::EPSILON).then_some((prev, curr))
        }
        fn merge_delta(
            existing: Option<(f64, f64)>,
            incoming: Option<(f64, f64)>,
        ) -> Option<(f64, f64)> {
            match (existing, incoming) {
                (Some((from, _)), Some((_, to))) => Some((from, to)),
                (existing, None) => existing,
                (None, incoming) => incoming,
            }
        }

        let delta = GainAnnotation {
            marker_tick: self.tick_serial,
            kp: gain_delta(self.last_kp, cfg.kp),
            ki: gain_delta(self.last_ki, cfg.ki),
            kd: gain_delta(self.last_kd, cfg.kd),
            sp: gain_delta(self.last_sp, cfg.setpoint),
        };
        self.last_kp = cfg.kp;
        self.last_ki = cfg.ki;
        self.last_kd = cfg.kd;
        self.last_sp = cfg.setpoint;
        if delta.kp.is_none() && delta.ki.is_none() && delta.kd.is_none() && delta.sp.is_none() {
            return;
        }
        if let Some(ann) = self.annotations.back_mut()
            && self.tick_serial.saturating_sub(ann.marker_tick) <= 3
        {
            ann.kp = merge_delta(ann.kp, delta.kp);
            ann.ki = merge_delta(ann.ki, delta.ki);
            ann.kd = merge_delta(ann.kd, delta.kd);
            ann.sp = merge_delta(ann.sp, delta.sp);
            ann.marker_tick = self.tick_serial;
            return;
        }
        self.annotations.push_back(delta);
    }
}

/// Minimum time between full-frame redraws while waiting for the next PID tick.
///
/// Without this, the loop redraws on every `event::poll` wakeup (~20 Hz for a 50 ms cap), which
/// steals wall time from the subprocess + controller work and stretches measured `raw_dt` away
/// from `--interval` — undermining tuning on a production-like cadence.
pub(in crate::tune) const TUNE_IDLE_DRAW_MIN: std::time::Duration =
    std::time::Duration::from_millis(200);
/// Redraw at least this often when the next tick is near so the countdown stays legible.
pub(in crate::tune) const TUNE_IDLE_DRAW_DEADLINE_NEAR: std::time::Duration =
    std::time::Duration::from_millis(120);
/// Fixed row height of the gains section (header + separator + 4 gain rows).
pub(in crate::tune) const GAINS_H: u16 = 6;
/// Minimum rows reserved for process info before sparklines start collapsing.
pub(in crate::tune) const PROCESS_MIN: u16 = 5;
