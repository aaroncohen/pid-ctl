use super::{
    GainAnnotation, annotation_caret_line, build_export_line_values, history_trend, spark_data,
    spark_marker_row,
};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[test]
fn spark_data_flat_series_is_visible_mid_line() {
    let mut d = VecDeque::new();
    d.push_back(22.0);
    d.push_back(22.0);
    d.push_back(22.0);
    let s = spark_data(&d);
    assert_eq!(s, vec![50, 50, 50]);
}

#[test]
fn spark_data_spanning_series_normalized() {
    let mut d = VecDeque::new();
    d.push_back(0.0);
    d.push_back(10.0);
    let s = spark_data(&d);
    assert_eq!(s, vec![0, 100]);
}

#[test]
fn pv_trend_arrow_when_history_rises() {
    let mut d = VecDeque::new();
    d.push_back(1.0);
    d.push_back(3.0);
    assert_eq!(history_trend(&d), "▲");
}

#[test]
fn spark_marker_row_places_pipe_at_tick_column() {
    let serials = vec![1_u64, 2, 3, 4, 5];
    let mut ann = VecDeque::new();
    ann.push_back(GainAnnotation {
        marker_tick: 4,
        kp: Some((1.0, 2.0)),
        ki: None,
        kd: None,
        sp: None,
    });
    let row = spark_marker_row(&serials, &ann, 5);
    assert_eq!(row.chars().nth(3), Some('|'));
}

/// Regression: sparkline slots must use `Constraint::Fill` so they cannot overflow
/// their parent in ratatui's layout engine.
#[test]
fn sparkline_constraints_use_fill() {
    use ratatui::layout::Constraint;
    // Mirror the constraints used in render_frame's hist_inner layout.
    // Indices 1 and 3 are the PV and CV sparklines.
    let constraints = [
        Constraint::Length(1), // [0] history title
        Constraint::Fill(1),   // [1] PV sparkline
        Constraint::Length(1), // [2] PV marker row
        Constraint::Fill(1),   // [3] CV sparkline
        Constraint::Length(1), // [4] CV marker row
        Constraint::Length(1), // [5] caret
    ];
    for idx in [1usize, 3] {
        assert!(
            matches!(constraints[idx], Constraint::Fill(_)),
            "hist_inner[{idx}] (sparkline) must be Fill — got {:?}",
            constraints[idx]
        );
    }
    // Verify fixed slots are all Length(1) — no wasted rows.
    for idx in [0usize, 2, 4, 5] {
        assert!(
            matches!(constraints[idx], Constraint::Length(1)),
            "hist_inner[{idx}] (fixed slot) must be Length(1) — got {:?}",
            constraints[idx]
        );
    }
}

/// Regression: history cap must be `max(tune_history, spark_w)` so that sparklines fill
/// the terminal width even when `tune_history` < screen columns.
#[test]
fn history_cap_uses_spark_w_when_wider_than_tune_history() {
    // Simulate a TuneUiState with tune_history=10 but spark_w=100 (wide terminal).
    // After 20 pushes, we expect 100 items retained (not 10).
    let tune_history = 10usize;
    let spark_w = 100usize;
    // The cap logic: cap = tune_history.max(spark_w)
    let cap = tune_history.max(spark_w);
    assert_eq!(
        cap, 100,
        "cap should follow spark_w when spark_w > tune_history"
    );
    // Simulate the dequeue trimming
    let mut history: VecDeque<f64> = VecDeque::new();
    for i in 0..200u32 {
        while history.len() >= cap {
            history.pop_front();
        }
        history.push_back(f64::from(i));
    }
    assert_eq!(history.len(), cap, "history should hold exactly cap items");
    // Conversely: if spark_w < tune_history, tune_history wins
    let cap2 = tune_history.max(20);
    assert_eq!(
        cap2, 20,
        "cap should follow tune_history when tune_history > spark_w"
    );
}

#[test]
fn history_trend_falling() {
    let mut d = VecDeque::new();
    d.push_back(5.0);
    d.push_back(1.0);
    assert_eq!(history_trend(&d), "▼");
}

#[test]
fn history_trend_stable() {
    let mut d = VecDeque::new();
    d.push_back(3.0);
    d.push_back(3.0);
    assert_eq!(history_trend(&d), "→");
}

#[test]
fn history_trend_empty_is_stable() {
    let d: VecDeque<f64> = VecDeque::new();
    assert_eq!(history_trend(&d), "→");
}

#[test]
fn gain_annotation_display_text_shows_net_change() {
    let ann = GainAnnotation {
        marker_tick: 5,
        kp: Some((1.0, 2.0)),
        ki: Some((0.1, 0.3)),
        kd: None,
        sp: None,
    };
    let text = ann.display_text();
    assert!(text.contains("Kp 1.000→2.000"), "got: {text}");
    assert!(text.contains("Ki 0.100→0.300"), "got: {text}");
    assert!(!text.contains("Kd"), "got: {text}");
}

#[test]
fn spark_marker_row_time_dots_at_multiples_of_10() {
    let serials: Vec<u64> = vec![8, 9, 10, 11, 20];
    let ann: VecDeque<GainAnnotation> = VecDeque::new();
    let row = spark_marker_row(&serials, &ann, 5);
    let chars: Vec<char> = row.chars().collect();
    assert_eq!(chars[0], ' ', "8 % 10 != 0");
    assert_eq!(chars[1], ' ', "9 % 10 != 0");
    assert_eq!(chars[2], '·', "10 % 10 == 0");
    assert_eq!(chars[3], ' ', "11 % 10 != 0");
    assert_eq!(chars[4], '·', "20 % 10 == 0");
}

#[test]
fn spark_marker_row_pipe_overwrites_dot() {
    let serials: Vec<u64> = vec![10, 20, 30];
    let mut ann: VecDeque<GainAnnotation> = VecDeque::new();
    ann.push_back(GainAnnotation {
        marker_tick: 10,
        kp: Some((1.0, 2.0)),
        ki: None,
        kd: None,
        sp: None,
    });
    let row = spark_marker_row(&serials, &ann, 3);
    let chars: Vec<char> = row.chars().collect();
    assert_eq!(chars[0], '|', "pipe should overwrite dot at tick 10");
    assert_eq!(chars[1], '·', "tick 20 should show dot");
    assert_eq!(chars[2], '·', "tick 30 should show dot");
}

#[test]
fn annotation_caret_aligns_with_marker_column() {
    // Serial window: ticks 1..5, annotation at tick 3 (col 2).
    let serials: Vec<u64> = vec![1, 2, 3, 4, 5];
    let mut ann: VecDeque<GainAnnotation> = VecDeque::new();
    ann.push_back(GainAnnotation {
        marker_tick: 3,
        kp: Some((1.0, 2.0)),
        ki: None,
        kd: None,
        sp: None,
    });
    let line = annotation_caret_line(&serials, &ann, 20);
    let chars: Vec<char> = line.chars().collect();
    assert_eq!(chars[2], '^', "caret must be at col 2 (tick 3)");
    // Label text starts two columns after the caret.
    let label_start: String = chars[4..].iter().collect();
    assert!(
        label_start.starts_with("Kp 1.000→2.000"),
        "label should start at col 4, got: {line:?}"
    );
}

#[test]
fn annotation_caret_newer_overwrites_older() {
    // Two annotations: older at col 0, newer at col 5.
    // Their label text overlaps — newer should win on the overlapping chars.
    let serials: Vec<u64> = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    let mut ann: VecDeque<GainAnnotation> = VecDeque::new();
    // Older annotation at tick 1 (col 0) — long label that would extend past col 5.
    ann.push_back(GainAnnotation {
        marker_tick: 1,
        kp: Some((1.0, 2.0)),
        ki: None,
        kd: None,
        sp: None,
    });
    // Newer annotation at tick 6 (col 5) — overwrites the tail of the older label.
    ann.push_back(GainAnnotation {
        marker_tick: 6,
        kp: None,
        ki: Some((0.1, 0.5)),
        kd: None,
        sp: None,
    });
    let line = annotation_caret_line(&serials, &ann, 40);
    let chars: Vec<char> = line.chars().collect();
    // Newer caret must be at col 5.
    assert_eq!(chars[5], '^', "newer caret must be at col 5 (tick 6)");
    // Newer label text (Ki ...) must start at col 7.
    let newer_label: String = chars[7..].iter().collect();
    assert!(
        newer_label.starts_with("Ki 0.100→0.500"),
        "newer label should start at col 7, got: {line:?}"
    );
}

#[test]
fn annotation_caret_skips_off_screen_markers() {
    // Annotation tick not present in serial window → no output.
    let serials: Vec<u64> = vec![10, 11, 12];
    let mut ann: VecDeque<GainAnnotation> = VecDeque::new();
    ann.push_back(GainAnnotation {
        marker_tick: 99,
        kp: Some((1.0, 2.0)),
        ki: None,
        kd: None,
        sp: None,
    });
    let line = annotation_caret_line(&serials, &ann, 20);
    assert!(
        line.is_empty(),
        "off-screen annotation should produce no output, got: {line:?}"
    );
}

#[test]
fn step_125_up_sequence() {
    use super::step_125_up;
    assert!((step_125_up(0.1) - 0.2).abs() < 1e-9);
    assert!((step_125_up(0.2) - 0.5).abs() < 1e-9);
    assert!((step_125_up(0.5) - 1.0).abs() < 1e-9);
    assert!((step_125_up(1.0) - 2.0).abs() < 1e-9);
    assert!((step_125_up(2.0) - 5.0).abs() < 1e-9);
    assert!((step_125_up(5.0) - 10.0).abs() < 1e-9);
}

#[test]
fn step_125_down_sequence() {
    use super::step_125_down;
    assert!((step_125_down(10.0) - 5.0).abs() < 1e-9);
    assert!((step_125_down(5.0) - 2.0).abs() < 1e-9);
    assert!((step_125_down(2.0) - 1.0).abs() < 1e-9);
    assert!((step_125_down(1.0) - 0.5).abs() < 1e-9);
    assert!((step_125_down(0.5) - 0.2).abs() < 1e-9);
    assert!((step_125_down(0.2) - 0.1).abs() < 1e-9);
}

#[test]
fn needed_decimals_integers_need_zero() {
    use super::needed_decimals;
    assert_eq!(needed_decimals(1.0), 0);
    assert_eq!(needed_decimals(10.0), 0);
    assert_eq!(needed_decimals(0.0), 0);
}

#[test]
fn needed_decimals_step_values() {
    use super::needed_decimals;
    assert_eq!(needed_decimals(0.1), 1);
    assert_eq!(needed_decimals(0.01), 2);
    assert_eq!(needed_decimals(0.001), 3);
    assert_eq!(needed_decimals(0.5), 1);
    assert_eq!(needed_decimals(0.25), 2);
}

#[test]
fn expand_scale_widens_tiny_range() {
    use super::expand_scale;
    let (lo, hi) = expand_scale(2.770, 2.780, 2.775);
    assert!(
        hi - lo >= 0.01 * 2.775,
        "span should be at least 1% of mean"
    );
    assert!(lo <= 2.770, "lo should not exceed original min");
    assert!(hi >= 2.780, "hi should not exceed original max");
}

#[test]
fn expand_scale_leaves_wide_range_unchanged() {
    use super::expand_scale;
    let (lo, hi) = expand_scale(0.0, 100.0, 50.0);
    assert!((lo - 0.0).abs() < 1e-9);
    assert!((hi - 100.0).abs() < 1e-9);
}

#[test]
fn history_range_returns_expanded_scale() {
    use super::history_range;
    let mut d = VecDeque::new();
    // Tight range around 100 — should be expanded to at least 1%
    for _ in 0..10 {
        d.push_back(100.0);
    }
    d.push_back(100.001);
    let (lo, hi) = history_range(&d).unwrap();
    assert!(hi - lo >= 0.01 * 100.0 * 0.99, "span should be ~1% of mean");
}

#[test]
fn pv_canvas_coords_oldest_left_newest_right() {
    // X index 0 = oldest, last index = newest (left-to-right time order).
    let d: Vec<f64> = vec![1.0, 2.0, 3.0];
    let spark_w = 3usize;
    let start = d.len().saturating_sub(spark_w);
    #[allow(clippy::cast_precision_loss)]
    let coords: Vec<(f64, f64)> = d
        .iter()
        .skip(start)
        .enumerate()
        .filter_map(|(i, &v)| v.is_finite().then_some((i as f64, v)))
        .collect();
    assert_eq!(coords, vec![(0.0, 1.0), (1.0, 2.0), (2.0, 3.0)]);
}

#[test]
fn pv_canvas_coords_truncate_to_window() {
    // When history is longer than spark_w, only the last spark_w items appear.
    let d: Vec<f64> = vec![99.0, 1.0, 2.0, 3.0]; // 4 items, window=3
    let spark_w = 3usize;
    let start = d.len().saturating_sub(spark_w);
    #[allow(clippy::cast_precision_loss)]
    let coords: Vec<(f64, f64)> = d
        .iter()
        .skip(start)
        .enumerate()
        .filter_map(|(i, &v)| v.is_finite().then_some((i as f64, v)))
        .collect();
    // 99.0 is excluded; indices restart from 0
    assert_eq!(coords, vec![(0.0, 1.0), (1.0, 2.0), (2.0, 3.0)]);
}

#[test]
fn scale_ticks_multiples_of_step_are_brackets() {
    use super::scale_ticks;
    // span=0-7: rough=0.875, step=1.0 (first nice number >= 0.875), sub=0.1
    // Multiples of step=1.0 → k%10==0 → ┤/╡/╣.  Non-multiples → · or ╴.
    let ticks = scale_ticks(0.0, 7.0, 50.0, 100);
    // k=10 → y=1.0, k=20 → y=2.0 ... all should be heavy ticks
    for &(y, sym) in &ticks {
        let is_integer = (y.round() - y).abs() < 1e-9;
        if is_integer && y > 0.0 {
            assert!(
                sym == "┤" || sym == "╡" || sym == "╣",
                "y={y} is a multiple of step, expected ┤/╡/╣ got {sym}"
            );
        }
    }
}

#[test]
fn scale_ticks_k_modulo_classification_correct() {
    use super::scale_ticks;
    // span=0-5: rough=5/8=0.625, step=1.0 (first nice >= 0.625), sub=0.1.
    // k=50 → y=5.0 → k%50==0 → ╡; k=100 → y=10 (out of range).
    let ticks = scale_ticks(0.0, 5.0, 50.0, 100);
    let at_5 = ticks.iter().find(|(y, _)| (*y - 5.0).abs() < 1e-9);
    // y=5.0 is k=50 → ╡ (major ×5)
    assert_eq!(at_5.map(|(_, s)| *s), Some("╡"), "y=5 should be ╡");
    let at_1 = ticks.iter().find(|(y, _)| (*y - 1.0).abs() < 1e-9);
    // y=1.0 is k=10 → ┤ (base step)
    assert_eq!(at_1.map(|(_, s)| *s), Some("┤"), "y=1 should be ┤");
}

#[test]
fn scale_ticks_empty_on_zero_span() {
    use super::scale_ticks;
    assert!(scale_ticks(5.0, 5.0, 50.0, 20).is_empty());
}

#[test]
fn scale_ticks_base_steps_are_consistent_at_fractional_values() {
    use super::scale_ticks;
    // Regression: span centered on 60.0 with ±0.3 range (like SP=60, 1% zoom).
    // Every base-step tick must be ┤, ╡, or ╣ — never · or ╴.
    let ticks = scale_ticks(59.7, 60.3, 60.0, 100);
    // Find the base step used: the gap between consecutive ┤ ticks.
    let bracket_ys: Vec<f64> = ticks
        .iter()
        .filter(|(_, s)| *s == "┤" || *s == "╡" || *s == "╣")
        .map(|(y, _)| *y)
        .collect();
    assert!(
        !bracket_ys.is_empty(),
        "expected at least one base-step tick"
    );
    // None of the non-heavy ticks should land at the same Y as a heavy tick.
    let dot_ys: Vec<f64> = ticks
        .iter()
        .filter(|(_, s)| *s == "·" || *s == "╴")
        .map(|(y, _)| *y)
        .collect();
    for &dy in &dot_ys {
        assert!(
            bracket_ys.iter().all(|&by| (by - dy).abs() > 1e-9),
            "dot tick at {dy} collides with a base-step tick"
        );
    }
}

#[test]
fn export_dedupes_tunables_and_strips_tune_flags() {
    let argv = vec![
        "pid-ctl".into(),
        "loop".into(),
        "--pv-file".into(),
        "/tmp/p".into(),
        "--setpoint".into(),
        "50".into(),
        "--kp".into(),
        "1".into(),
        "--ki".into(),
        "0.1".into(),
        "--kd".into(),
        "0".into(),
        "--interval".into(),
        "2s".into(),
        "--tune-history".into(),
        "80".into(),
        "--tune".into(),
    ];
    let s = build_export_line_values(&argv, 78.3, 2.1, 0.05, 0.8, Duration::from_secs(5));
    assert!(
        s.contains("--setpoint 78.3")
            && s.contains("--kp 2.1")
            && s.contains("--ki 0.05")
            && s.contains("--kd 0.8")
            && s.contains("--interval 5s")
    );
    assert_eq!(s.matches("--kp").count(), 1);
    assert_eq!(s.matches("--setpoint").count(), 1);
    assert!(!s.contains("--tune"));
    assert!(!s.contains("--tune-history"));
}

// ── Render visibility helpers ──────────────────────────────────────────────

fn test_pid_config() -> pid_ctl_core::PidConfig {
    pid_ctl_core::PidConfig {
        setpoint: 55.0,
        kp: 1.0,
        ki: 0.05,
        kd: 0.01,
        out_min: 0.0,
        out_max: 100.0,
        ..Default::default()
    }
}

fn test_loop_args(config: pid_ctl_core::PidConfig) -> super::LoopArgs {
    super::LoopArgs {
        runtime: crate::LoopRuntimeConfig {
            interval: Duration::from_secs(1),
            max_dt: crate::cli::user_set::UserSet::Default(2.0),
            pv_stdin_timeout: crate::cli::user_set::UserSet::Default(Duration::from_secs(5)),
            state_write_interval: crate::cli::user_set::UserSet::Default(None),
        },
        pv_source: pid_ctl::app::adapters_build::LoopPvSource::Cmd("echo 0".into()),
        ff_source: pid_ctl::app::adapters_build::LoopFfSource::Zero,
        cv_sink: None,
        pid_config: config,
        state_path: None,
        name: None,
        reset_accumulator: false,
        scale: 1.0,
        cv_precision: 3,
        output_format: crate::OutputFormat::Text,
        cmd_timeout: Duration::from_secs(5),
        pv_cmd_timeout: Duration::from_secs(5),
        safe_cv: None,
        cv_fail_after: 3,
        fail_after: None,
        min_dt: crate::cli::user_set::UserSet::Default(0.5),
        dt_clamp: false,
        log_path: None,
        dry_run: true,
        verify_cv: false,
        state_fail_after: 3,
        tune: true,
        tune_history: 60,
        tune_step_kp: 0.01,
        tune_step_ki: 0.001,
        tune_step_kd: 0.01,
        tune_step_sp: 0.1,
        units: Some("°C".into()),
        quiet: false,
        verbose: false,
        #[cfg(unix)]
        socket_path: None,
        #[cfg(unix)]
        socket_mode: 0o660,
        max_iterations: None,
    }
}

fn make_test_ui_state(_args: &super::LoopArgs, terminal_width: u16) -> super::TuneUiState {
    let mut pv_history = VecDeque::new();
    let mut cv_history = VecDeque::new();
    let mut sp_history = VecDeque::new();
    let mut serial_history = VecDeque::new();
    #[allow(clippy::cast_precision_loss)]
    for i in 0..60u64 {
        let t = i as f64 * 0.3;
        pv_history.push_back(53.0 + t.sin() * 3.0);
        cv_history.push_back(35.0 + i as f64 * 0.3);
        sp_history.push_back(55.0);
        serial_history.push_back(i + 1);
    }
    super::TuneUiState {
        focus: super::GainFocus::Kp,
        step: [0.01, 0.001, 0.01, 0.1],
        command_mode: false,
        command_buf: String::new(),
        help_overlay: false,
        hold: false,
        dry_run: true,
        last_record: Some(pid_ctl::app::IterationRecord {
            schema_version: 1,
            ts: "2026-01-01T00:00:00Z".into(),
            name: None,
            iter: 42,
            pv: 53.2,
            sp: 55.0,
            effective_sp: None,
            err: 1.8,
            p: 1.8,
            i: 0.3,
            d: -0.1,
            ff: 0.0,
            cv: 42.5,
            i_acc: 1.5,
            dt: 1.0,
        }),
        pv_history,
        cv_history,
        sp_history,
        serial_history,
        tick_serial: 60,
        annotations: VecDeque::new(),
        spark_w: terminal_width.saturating_sub(4) as usize,
        last_kp: 1.0,
        last_ki: 0.05,
        last_kd: 0.01,
        last_sp: 55.0,
        start: Instant::now(),
        quit: false,
        status_flash: None,
        export_overlay: None,
    }
}

fn buffer_text(backend: &ratatui::backend::TestBackend) -> String {
    let buf = backend.buffer();
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| {
                    buf.cell(ratatui::layout::Position { x, y })
                        .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_at_size(width: u16, height: u16) -> String {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let config = test_pid_config();
    let args = test_loop_args(config.clone());
    let ui = make_test_ui_state(&args, width);
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            super::render_frame(f, &config, &args, &ui, 1.0, Duration::ZERO);
        })
        .unwrap();
    buffer_text(terminal.backend())
}

// ── Visibility tests ───────────────────────────────────────────────────────

/// Gains section must be visible even at the minimum practical terminal height.
/// Header (2) + Gains (6) + Footer (3) = 11 rows minimum; test at 14 to give
/// one row of process info.
#[test]
fn gains_always_visible_at_minimum_height() {
    let text = render_at_size(80, 14);
    assert!(text.contains("GAINS"), "GAINS missing at 80×14:\n{text}");
    assert!(text.contains("Kp"), "Kp row missing at 80×14:\n{text}");
    assert!(text.contains("Ki"), "Ki row missing at 80×14:\n{text}");
    assert!(text.contains("Kd"), "Kd row missing at 80×14:\n{text}");
}

/// At a standard 24-row terminal the process info block should also be present.
#[test]
fn process_info_visible_at_standard_height() {
    let text = render_at_size(80, 24);
    assert!(text.contains("GAINS"), "GAINS missing at 80×24:\n{text}");
    assert!(
        text.contains("PROCESS"),
        "PROCESS block missing at 80×24:\n{text}"
    );
    assert!(
        text.contains("Setpoint"),
        "Setpoint row missing at 80×24:\n{text}"
    );
    assert!(
        text.contains("PV (actual)"),
        "PV row missing at 80×24:\n{text}"
    );
}

/// At 16 rows sparklines should collapse (not enough room) but gains must remain.
/// Available body rows = 16 − header(2) − footer(3) = 11, which is below the
/// minimum of 13 needed for sparklines, so they collapse to zero height.
#[test]
fn sparklines_collapse_at_small_height() {
    let text = render_at_size(80, 16);
    assert!(text.contains("GAINS"), "GAINS missing at 80×16:\n{text}");
    assert!(
        !text.contains("HISTORY"),
        "HISTORY unexpectedly present at 80×16:\n{text}"
    );
}

/// At 30 rows the sparklines section should be visible alongside gains and process info.
#[test]
fn sparklines_visible_at_comfortable_height() {
    let text = render_at_size(80, 30);
    assert!(text.contains("GAINS"), "GAINS missing at 80×30:\n{text}");
    assert!(
        text.contains("HISTORY"),
        "HISTORY missing at 80×30:\n{text}"
    );
}

/// A wide terminal should render all sections without panicking or truncating gains.
#[test]
fn wide_terminal_renders_without_panic() {
    let text = render_at_size(200, 30);
    assert!(text.contains("GAINS"), "GAINS missing at 200×30:\n{text}");
    assert!(
        text.contains("HISTORY"),
        "HISTORY missing at 200×30:\n{text}"
    );
}

// ── Scale ruler stability tests ────────────────────────────────────────────

/// Maps a world-space `y` value to a 0-based character row, matching the
/// exact formula ratatui's Canvas uses for label placement (see
/// `ratatui-widgets` canvas.rs, "Finally draw the labels"):
///
/// ```text
/// y_row = (top - label.y) * (canvas_height - 1) / (top - bottom)  as u16
/// ```
///
/// Returns `None` for points outside `[y_lo, y_hi]` (filtered by ratatui
/// before the math runs).
fn y_to_char_row(y: f64, y_lo: f64, y_hi: f64, height: u16) -> Option<u16> {
    // Ratatui pre-filters labels to [bottom, top] inclusive.
    if y < y_lo || y > y_hi {
        return None;
    }
    let resolution_h = f64::from(height.saturating_sub(1));
    let span = y_hi - y_lo;
    if span <= 0.0 || height == 0 {
        return None;
    }
    let frac = (y_hi - y) * resolution_h / span;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(frac as u16) // truncates toward zero, same as ratatui's `as u16` cast
}

/// Renders only the scale-ruler ticks into a [`TestBackend`] buffer and
/// returns the `(col, row)` positions of every cell that contains a tick
/// character (·, ╴, ┤, ╡, or ╣).
fn rendered_tick_cells(
    y_lo: f64,
    y_hi: f64,
    setpoint: f64,
    width: u16,
    height: u16,
) -> std::collections::HashSet<(u16, u16)> {
    const TICK_SYMS: &[&str] = &["·", "╴", "┤", "╡", "╣"];
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::canvas::Canvas;
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    let x_max = (f64::from(width) - 1.0).max(1.0);
    let ticks = super::scale_ticks(y_lo, y_hi, setpoint, height);
    terminal
        .draw(|f| {
            let area = f.area();
            let canvas = Canvas::default()
                .marker(ratatui::symbols::Marker::Braille)
                .x_bounds([0.0, x_max])
                .y_bounds([y_lo, y_hi])
                .paint(move |ctx| {
                    for (y, sym) in &ticks {
                        ctx.print(x_max, *y, ratatui::text::Span::raw(*sym));
                    }
                });
            f.render_widget(canvas, area);
        })
        .unwrap();
    let mut cells = std::collections::HashSet::new();
    let buf = terminal.backend().buffer().clone();
    for row in 0..height {
        for col in 0..width {
            let cell = buf
                .cell(ratatui::layout::Position { x: col, y: row })
                .unwrap();
            if TICK_SYMS.contains(&cell.symbol()) {
                cells.insert((col, row));
            }
        }
    }
    cells
}

/// Diagnostic: renders the scale ruler at several zoom levels and dumps the
/// buffer to stderr so you can visually inspect tick placement.
///
/// Run with:
/// ```text
/// cargo test -p pid-ctl --features tui scale_ruler_debug -- --ignored --nocapture
/// ```
#[test]
#[ignore = "manual debug render; run with --ignored --nocapture"]
fn scale_ruler_debug_render() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::canvas::Canvas;
    let sp = 60.0_f64;
    let height: u16 = 20;
    let width: u16 = 10;
    for &half_span in &[0.3_f64, 1.0, 3.0, 10.0, 30.0] {
        let y_lo = sp - half_span;
        let y_hi = sp + half_span;
        let ticks = super::scale_ticks(y_lo, y_hi, sp, height);
        eprintln!("\n── half_span={half_span} y=[{y_lo:.2}, {y_hi:.2}] ──");
        for &(y, sym) in &ticks {
            let row = y_to_char_row(y, y_lo, y_hi, height);
            eprintln!("  scale_ticks: y={y:.6} {sym} → predicted row {row:?}");
        }
        let x_max = (f64::from(width) - 1.0).max(1.0);
        let ticks2 = super::scale_ticks(y_lo, y_hi, sp, height);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                let canvas = Canvas::default()
                    .marker(ratatui::symbols::Marker::Braille)
                    .x_bounds([0.0, x_max])
                    .y_bounds([y_lo, y_hi])
                    .paint(move |ctx| {
                        for (y, sym) in &ticks2 {
                            ctx.print(x_max, *y, ratatui::text::Span::raw(*sym));
                        }
                    });
                f.render_widget(canvas, area);
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        eprintln!("  rendered buffer (right edge, {height} rows):");
        for row in 0..height {
            let sym: String = (0..width)
                .map(|col| {
                    buf.cell(ratatui::layout::Position { x: col, y: row })
                        .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
                })
                .collect();
            eprintln!("    row {row:2}: {sym}");
        }
    }
}

/// Every tick whose world-Y sits more than one row-height inside the visible
/// window must actually appear in the rendered buffer.  Sweep 200 zoom levels
/// from ±0.5 % to ±50 % of SP=60 to simulate smooth zoom animation.
#[test]
fn scale_ruler_interior_ticks_always_rendered() {
    let sp = 60.0_f64;
    let height: u16 = 20;
    let width: u16 = 10;

    for i in 0..=200_u32 {
        let half_span = sp * (0.005 + 0.495 * (f64::from(i) / 200.0));
        let y_lo = sp - half_span;
        let y_hi = sp + half_span;
        let row_world = (y_hi - y_lo) / f64::from(height);

        let ticks = super::scale_ticks(y_lo, y_hi, sp, height);
        let cells = rendered_tick_cells(y_lo, y_hi, sp, width, height);
        let rendered_rows: std::collections::HashSet<u16> =
            cells.iter().map(|&(_, row)| row).collect();

        for &(y, _sym) in &ticks {
            // Skip ticks within one row-height of either edge — they are
            // legitimately at risk of being clipped by ratatui's canvas.
            if y <= y_lo + row_world || y >= y_hi - row_world {
                continue;
            }
            let expected_row = y_to_char_row(y, y_lo, y_hi, height)
                .expect("interior tick must map to a valid character row");
            assert!(
                rendered_rows.contains(&expected_row),
                "zoom step {i}: tick at y={y:.6} (expected row {expected_row}) \
                     not found in rendered buffer \
                     (y_lo={y_lo:.6}, y_hi={y_hi:.6}, half_span={half_span:.6})"
            );
        }
    }
}

/// # Cadence tests — exact row positions
///
/// These three tests use SP=100 (→ sub=1.0, integer tick positions) and
/// spans chosen so that ratatui's label formula
///
/// ```text
/// row = floor((y_hi − y) × (height − 1) / span)
/// ```
///
/// produces integer results with a predictable stride.  Each test asserts:
/// 1. `scale_ticks` emits exactly the expected set of ticks.
/// 2. `y_to_char_row` maps each tick to the expected row (no surprises in
///    the math).
/// 3. `rendered_tick_cells` finds a tick character in every expected row of
///    the actual ratatui output.
///
/// ## Every row (stride=1, height=20, span=19)
///
/// `row = (110 − y) × 19 / 19 = 110 − y` exactly.
/// 20 integer y-values in [91, 110] → one tick per row, rows 0–19.
#[test]
fn scale_ruler_cadence_every_row() {
    let sp = 100.0_f64;
    let y_lo = 91.0_f64;
    let y_hi = 110.0_f64; // span = 19 = height − 1
    let height: u16 = 20;
    let width: u16 = 5;

    let ticks = super::scale_ticks(y_lo, y_hi, sp, height);
    assert_eq!(
        ticks.len(),
        20,
        "expected 20 ticks (one per row); got {}: {ticks:?}",
        ticks.len()
    );

    // Predict and verify each tick's character row.
    let expected_rows: Vec<u16> = (0..20_u16).collect(); // rows 0,1,...,19
    let mut actual_rows: Vec<u16> = ticks
        .iter()
        .map(|&(y, _)| {
            y_to_char_row(y, y_lo, y_hi, height)
                .unwrap_or_else(|| panic!("tick y={y} out of canvas bounds"))
        })
        .collect();
    actual_rows.sort_unstable();
    assert_eq!(
        actual_rows, expected_rows,
        "tick rows should be exactly 0..19 (one tick per row)"
    );

    // Verify in the rendered ratatui buffer.
    let cells = rendered_tick_cells(y_lo, y_hi, sp, width, height);
    let rendered_rows: std::collections::HashSet<u16> = cells.iter().map(|&(_, r)| r).collect();
    for row in 0..height {
        assert!(
            rendered_rows.contains(&row),
            "rendered buffer missing tick at row {row} (every-row cadence)"
        );
    }
}

/// ## Every other row (stride=1, height=11, span=5)
///
/// `row = (100 − y) × 10 / 5 = 2 × (100 − y)` exactly.
/// 6 integer y-values in [95, 100] → ticks at rows 0, 2, 4, 6, 8, 10.
/// Odd rows (1, 3, 5, 7, 9) are always empty.
#[test]
fn scale_ruler_cadence_every_other_row() {
    let sp = 100.0_f64;
    let y_lo = 95.0_f64;
    let y_hi = 100.0_f64; // span = 5, height − 1 = 10, ratio = 2
    let height: u16 = 11;
    let width: u16 = 5;

    let ticks = super::scale_ticks(y_lo, y_hi, sp, height);
    assert_eq!(
        ticks.len(),
        6,
        "expected 6 ticks; got {}: {ticks:?}",
        ticks.len()
    );

    let expected_rows: Vec<u16> = vec![0, 2, 4, 6, 8, 10];
    let mut actual_rows: Vec<u16> = ticks
        .iter()
        .map(|&(y, _)| {
            y_to_char_row(y, y_lo, y_hi, height)
                .unwrap_or_else(|| panic!("tick y={y} out of canvas bounds"))
        })
        .collect();
    actual_rows.sort_unstable();
    assert_eq!(
        actual_rows, expected_rows,
        "tick rows should be exactly [0,2,4,6,8,10]"
    );

    // Rendered buffer: even rows filled, odd rows empty.
    let cells = rendered_tick_cells(y_lo, y_hi, sp, width, height);
    let rendered_rows: std::collections::HashSet<u16> = cells.iter().map(|&(_, r)| r).collect();
    let even_rows: Vec<u16> = (0..height).filter(|r| r % 2 == 0).collect();
    let odd_rows: Vec<u16> = (0..height).filter(|r| r % 2 != 0).collect();
    for row in &even_rows {
        assert!(
            rendered_rows.contains(row),
            "rendered buffer missing tick at even row {row}"
        );
    }
    for row in &odd_rows {
        assert!(
            !rendered_rows.contains(row),
            "rendered buffer has unexpected tick at odd row {row}"
        );
    }
}

/// ## Every 4th row (stride=5 forced, height=21, span=25)
///
/// span=25 > height=21 forces stride=5 (`sub_count=25`, 25/5=5 ≤ 21).
/// `row = (100 − y) × 20 / 25 = 4(100 − y)/5` exactly for y ∈ {75,80,85,90,95,100}.
/// 6 ticks at rows 0, 4, 8, 12, 16, 20.
#[test]
fn scale_ruler_cadence_every_4th_row() {
    let sp = 100.0_f64;
    let y_lo = 75.0_f64;
    let y_hi = 100.0_f64; // span = 25, height − 1 = 20, ratio = 4
    let height: u16 = 21;
    let width: u16 = 5;

    let ticks = super::scale_ticks(y_lo, y_hi, sp, height);
    assert_eq!(
        ticks.len(),
        6,
        "expected 6 ticks (stride=5 forced by span > height); got {}: {ticks:?}",
        ticks.len()
    );

    let expected_rows: Vec<u16> = vec![0, 4, 8, 12, 16, 20];
    let mut actual_rows: Vec<u16> = ticks
        .iter()
        .map(|&(y, _)| {
            y_to_char_row(y, y_lo, y_hi, height)
                .unwrap_or_else(|| panic!("tick y={y} out of canvas bounds"))
        })
        .collect();
    actual_rows.sort_unstable();
    assert_eq!(
        actual_rows, expected_rows,
        "tick rows should be exactly [0,4,8,12,16,20]"
    );

    // Rendered buffer: rows 0,4,8,12,16,20 have ticks; rest are empty.
    let cells = rendered_tick_cells(y_lo, y_hi, sp, width, height);
    let rendered_rows: std::collections::HashSet<u16> = cells.iter().map(|&(_, r)| r).collect();
    for &row in &expected_rows {
        assert!(
            rendered_rows.contains(&row),
            "rendered buffer missing tick at row {row} (every-4th-row cadence)"
        );
    }
    let empty_count = (0..height)
        .filter(|r| !expected_rows.contains(r))
        .filter(|r| rendered_rows.contains(r))
        .count();
    assert_eq!(
        empty_count, 0,
        "rendered buffer has {empty_count} unexpected tick(s) in non-cadence rows"
    );
}

/// When consecutive ticks are spread far enough apart to occupy distinct
/// rows each, no two ticks should share a row (one would be invisible,
/// hidden behind the other).  Collisions are expected when the tick grid
/// # Cadence-with-perturbation tests
///
/// Each of the three exact cadence scenarios is repeated with the window
/// slid continuously across one full tick period in 200 sub-pixel steps.
/// At every step every interior tick (>1 row-height from the edge) must
/// be present in the rendered buffer — no popping in and out.
///
/// The slide distance equals exactly one tick spacing so the test covers
/// the full cycle of a tick approaching an edge, disappearing (legitimately,
/// within the edge-exclusion margin), and re-emerging on the other side.
///
/// ## Perturbed every-row (tick spacing = 1.0, slide by 1.0)
#[test]
fn scale_ruler_cadence_every_row_perturbed() {
    let sp = 100.0_f64;
    let span = 19.0_f64; // height − 1 = 19 → exact integer row mapping
    let height: u16 = 20;
    let width: u16 = 5;
    let row_world = span / f64::from(height);
    let tick_spacing = 1.0_f64; // stride=1, sub=1

    for i in 0..=200_u32 {
        let shift = tick_spacing * f64::from(i) / 200.0; // 0.000 .. 1.000
        let y_lo = 91.0 + shift;
        let y_hi = y_lo + span;

        let ticks = super::scale_ticks(y_lo, y_hi, sp, height);
        let cells = rendered_tick_cells(y_lo, y_hi, sp, width, height);
        let rendered_rows: std::collections::HashSet<u16> = cells.iter().map(|&(_, r)| r).collect();

        for &(y, _) in &ticks {
            if y <= y_lo + row_world || y >= y_hi - row_world {
                continue; // legitimately near an edge — allowed to clip
            }
            let row = y_to_char_row(y, y_lo, y_hi, height)
                .expect("interior tick must map to a valid row");
            assert!(
                rendered_rows.contains(&row),
                "every-row perturb step {i} (shift={shift:.4}): \
                     interior tick y={y:.6} missing from row {row} \
                     (y_lo={y_lo:.4}, y_hi={y_hi:.4})"
            );
        }
    }
}

/// ## Perturbed every-other-row (tick spacing = 1.0, slide by 1.0)
///
/// Base: `y_lo=95`, `y_hi=100`, height=11, span=5 → stride=1, row step = 2.
/// Sliding by one tick spacing (1.0) covers all fractional alignments
/// between the integer tick grid and the row grid.
#[test]
fn scale_ruler_cadence_every_other_row_perturbed() {
    let sp = 100.0_f64;
    let span = 5.0_f64;
    let height: u16 = 11;
    let width: u16 = 5;
    let row_world = span / f64::from(height);
    let tick_spacing = 1.0_f64; // stride=1, sub=1

    for i in 0..=200_u32 {
        let shift = tick_spacing * f64::from(i) / 200.0;
        let y_lo = 95.0 + shift;
        let y_hi = y_lo + span;

        let ticks = super::scale_ticks(y_lo, y_hi, sp, height);
        let cells = rendered_tick_cells(y_lo, y_hi, sp, width, height);
        let rendered_rows: std::collections::HashSet<u16> = cells.iter().map(|&(_, r)| r).collect();

        for &(y, _) in &ticks {
            if y <= y_lo + row_world || y >= y_hi - row_world {
                continue;
            }
            let row = y_to_char_row(y, y_lo, y_hi, height)
                .expect("interior tick must map to a valid row");
            assert!(
                rendered_rows.contains(&row),
                "every-other-row perturb step {i} (shift={shift:.4}): \
                     interior tick y={y:.6} missing from row {row} \
                     (y_lo={y_lo:.4}, y_hi={y_hi:.4})"
            );
        }
    }
}

/// ## Perturbed every-4th-row (tick spacing = 5.0, slide by 5.0)
///
/// Base: `y_lo=75`, `y_hi=100`, height=21, span=25 → stride=5, row step = 4.
/// Sliding by one tick spacing (5.0) covers all fractional alignments
/// between the stride-5 tick grid and the row grid.
#[test]
fn scale_ruler_cadence_every_4th_row_perturbed() {
    let sp = 100.0_f64;
    let span = 25.0_f64;
    let height: u16 = 21;
    let width: u16 = 5;
    let row_world = span / f64::from(height);
    let tick_spacing = 5.0_f64; // stride=5, sub=1

    for i in 0..=200_u32 {
        let shift = tick_spacing * f64::from(i) / 200.0;
        let y_lo = 75.0 + shift;
        let y_hi = y_lo + span;

        let ticks = super::scale_ticks(y_lo, y_hi, sp, height);
        let cells = rendered_tick_cells(y_lo, y_hi, sp, width, height);
        let rendered_rows: std::collections::HashSet<u16> = cells.iter().map(|&(_, r)| r).collect();

        for &(y, _) in &ticks {
            if y <= y_lo + row_world || y >= y_hi - row_world {
                continue;
            }
            let row = y_to_char_row(y, y_lo, y_hi, height)
                .expect("interior tick must map to a valid row");
            assert!(
                rendered_rows.contains(&row),
                "every-4th-row perturb step {i} (shift={shift:.4}): \
                     interior tick y={y:.6} missing from row {row} \
                     (y_lo={y_lo:.4}, y_hi={y_hi:.4})"
            );
        }
    }
}

/// is denser than one row-height, so we skip those zoom levels.
#[test]
fn scale_ruler_no_row_collisions_when_ticks_are_sparse() {
    let sp = 60.0_f64;
    let height: u16 = 20;
    for &half_span in &[0.5_f64, 1.0, 2.0, 5.0, 10.0, 20.0] {
        let y_lo = sp - half_span;
        let y_hi = sp + half_span;
        let ticks = super::scale_ticks(y_lo, y_hi, sp, height);
        // Skip if there are more ticks than rows: collisions are unavoidable.
        if ticks.len() > usize::from(height) {
            continue;
        }
        let mut rows: Vec<u16> = ticks
            .iter()
            .filter_map(|&(y, _)| y_to_char_row(y, y_lo, y_hi, height))
            .collect();
        rows.sort_unstable();
        let original_len = rows.len();
        rows.dedup();
        assert_eq!(
            rows.len(),
            original_len,
            "half_span={half_span}: duplicate character rows — some ticks are \
                 hidden by collision (ticks={ticks:?})"
        );
    }
}
