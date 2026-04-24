use super::history::{
    annotation_caret_line, expand_scale, history_range, history_trend, scale_ticks, spark_data,
    spark_marker_row, spark_tail_slice,
};
use super::input::{HELP_OVERLAY_TEXT, command_mode_hint, needed_decimals};
use super::model::{GAINS_H, GainFocus, PROCESS_MIN, TuneUiState};
use crate::CliError;
use crate::LoopArgs;
use pid_ctl::app::ControllerSession;
use pid_ctl_core::PidConfig;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Points};
use ratatui::widgets::{Block, Borders, Paragraph, Sparkline, Wrap};
use ratatui::{Frame, Terminal};
use std::io::Stdout;
use std::time::Duration;

pub(in crate::tune) fn draw(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<Stdout>>,
    session: &ControllerSession,
    args: &LoopArgs,
    ui: &TuneUiState,
    interval_secs: f64,
    until_next: Duration,
) -> Result<(), CliError> {
    terminal
        .draw(|f| {
            render_frame(f, session.config(), args, ui, interval_secs, until_next);
        })
        .map_err(|e| CliError::new(1, format!("draw: {e}")))?;
    Ok(())
}

pub(in crate::tune) fn cv_fill_fraction(cv: f64, lo: f64, hi: f64) -> Option<f64> {
    if !lo.is_finite() || !hi.is_finite() {
        return None;
    }
    let span = hi - lo;
    if span.abs() < f64::EPSILON {
        return None;
    }
    Some(((cv - lo) / span).clamp(0.0, 1.0))
}

pub(in crate::tune) fn cv_percent(cv: f64, lo: f64, hi: f64) -> Option<f64> {
    cv_fill_fraction(cv, lo, hi).map(|f| f * 100.0)
}

pub(in crate::tune) fn cv_bar_block(frac: f64, width: usize) -> String {
    let width = width.max(1);
    // frac is in [0.0, 1.0]; width is a small terminal column count.
    // Casting width (usize) to f64 is lossless for any realistic terminal width.
    // Casting the rounded product back to usize: result is clamped by .min(width).
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let filled = ((frac * width as f64).round() as usize).min(width);
    let mut s = String::with_capacity(width + 2);
    s.push('[');
    for i in 0..width {
        s.push(if i < filled { '█' } else { '░' });
    }
    s.push(']');
    s
}

pub(in crate::tune) fn gains_precision(cfg: &PidConfig, step: &[f64; 4]) -> usize {
    [
        cfg.kp,
        cfg.ki,
        cfg.kd,
        cfg.setpoint,
        step[0],
        step[1],
        step[2],
        step[3],
    ]
    .iter()
    .map(|&v| needed_decimals(v))
    .max()
    .unwrap_or(2)
    .max(1)
}

pub(in crate::tune) fn step_cell_for_row(
    focus: GainFocus,
    row: usize,
    step: &[f64; 4],
    prec: usize,
) -> String {
    if focus.idx() == row {
        format!("[step {:.*}]", prec, step[row])
    } else {
        " ".repeat(prec + 8)
    }
}

// Drawing a complex multi-panel TUI layout necessarily touches many widgets in sequence;
// extracting sub-panels would require threading the frame reference through many helpers.
#[allow(clippy::too_many_lines)]
pub(in crate::tune) fn render_frame(
    f: &mut Frame<'_>,
    cfg: &PidConfig,
    args: &LoopArgs,
    ui: &TuneUiState,
    interval_secs: f64,
    until_next: Duration,
) {
    // Export overlay — full-screen, highest priority (any key dismisses).
    if let Some(export_text) = &ui.export_overlay {
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Export (any key to dismiss)");
        let p = Paragraph::new(export_text.as_str())
            .wrap(Wrap { trim: false })
            .block(block);
        f.render_widget(p, f.area());
        return;
    }

    // Help overlay — full-screen, replaces dashboard.
    if ui.help_overlay {
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Help (Esc or ? to close)");
        let p = Paragraph::new(HELP_OVERLAY_TEXT)
            .wrap(Wrap { trim: true })
            .block(block);
        f.render_widget(p, f.area());
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(f.area());

    let name = args
        .name
        .clone()
        .or_else(|| {
            args.state_path
                .as_ref()
                .and_then(|p| p.file_stem()?.to_str().map(String::from))
        })
        .unwrap_or_else(|| "pid-ctl".to_string());
    let elapsed = ui.start.elapsed();
    let elapsed_s = elapsed.as_secs();
    // tune_history is a small integer (default 60); as f64 is exact for all practical values.
    #[allow(clippy::cast_precision_loss)]
    let hist_wall_s = interval_secs * args.tune_history as f64;
    let header = Line::from(vec![Span::styled(
        format!(
            "pid-ctl  controller={name}  interval={:.1}s  last {} ticks ~{:.0}s wall  iter {}  {:02}m{:02}s  next ~{:.1}s",
            interval_secs,
            args.tune_history,
            hist_wall_s,
            ui.last_record.as_ref().map_or(0, |r| r.iter),
            elapsed_s / 60,
            elapsed_s % 60,
            until_next.as_secs_f64()
        ),
        Style::default().fg(Color::Cyan),
    )]);
    f.render_widget(
        Paragraph::new(header).block(Block::default().borders(Borders::BOTTOM)),
        chunks[0],
    );

    // Layout priority (highest → lowest): gains → process info → sparklines.
    // Sparklines must disappear first: they only receive rows that remain after
    // gains (fixed at GAINS_H) and process info (guaranteed PROCESS_MIN rows with
    // any excess split 1:2 between process and sparklines).  A sparkline area
    // smaller than 2 rows (title + 0 bars) is useless, so we collapse it to zero.
    let body_h = chunks[1].height;
    let after_gains = body_h.saturating_sub(GAINS_H);
    let excess = after_gains.saturating_sub(PROCESS_MIN);
    let process_h = PROCESS_MIN + excess / 3; // process grows at 1/3 rate
    let raw_spark_h = after_gains.saturating_sub(process_h);
    let spark_h = if raw_spark_h < 2 { 0 } else { raw_spark_h };
    let process_h = after_gains.saturating_sub(spark_h);

    let body_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(process_h), // [0] process info
            Constraint::Length(GAINS_H),   // [1] gains — always visible
            Constraint::Length(spark_h),   // [2] history sparklines — first to collapse
        ])
        .split(chunks[1]);

    let spark_w = body_chunks[2].width.saturating_sub(4) as usize;
    // Canvas plots PV (cyan) and SP (yellow) on the same coordinate space.
    // Y scale covers PV ∪ SP from the last 1.5× window to dampen jarring rescales.
    let pv_scale_n = (spark_w * 3 / 2).max(1);
    let pv_scale = {
        let skip = ui.pv_history.len().saturating_sub(pv_scale_n);
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        let mut sum = 0.0f64;
        let mut count = 0usize;
        for &v in ui
            .pv_history
            .iter()
            .skip(skip)
            .chain(ui.sp_history.iter().skip(skip))
        {
            if v.is_finite() {
                lo = lo.min(v);
                hi = hi.max(v);
                sum += v;
                count += 1;
            }
        }
        if count > 0 {
            #[allow(clippy::cast_precision_loss)]
            let mean = sum / count as f64;
            Some(expand_scale(lo, hi, mean))
        } else {
            None
        }
    };
    let cv_spark_full = spark_data(&ui.cv_history);
    let cv_spark = spark_tail_slice(&cv_spark_full, spark_w);

    let serial_vec: Vec<u64> = ui.serial_history.iter().copied().collect();
    let serial_window = spark_tail_slice(&serial_vec, spark_w);

    let marker_row = spark_marker_row(&serial_window, &ui.annotations, spark_w);
    let ann_w = body_chunks[2].width.saturating_sub(2) as usize;
    let caret_line = annotation_caret_line(&serial_window, &ui.annotations, ann_w);

    let units = args.units.as_deref().unwrap_or("");
    let pv_val = ui.last_record.as_ref().map_or(0.0, |r| r.pv);
    let err_val = ui.last_record.as_ref().map_or(0.0, |r| r.err);
    let err_hint = if err_val > 0.0 {
        "▼ positive — PV below target, output increasing"
    } else if err_val < 0.0 {
        "▼ negative — PV above target, output reducing"
    } else {
        "at setpoint"
    };

    let cv_val = ui.last_record.as_ref().map_or(0.0, |r| r.cv);
    let bar_w = 15usize;
    let bar_str = cv_fill_fraction(cv_val, cfg.out_min, cfg.out_max)
        .map_or_else(|| "[ n/a ]".to_string(), |f| cv_bar_block(f, bar_w));
    let pct_str = cv_percent(cv_val, cfg.out_min, cfg.out_max)
        .map_or_else(|| "—".to_string(), |p| format!("{p:.0}%"));

    let i_acc_hint = "anti-windup active — accumulator self-corrects when saturated; press r to reset manually if output is stuck";

    let gprec = gains_precision(cfg, &ui.step);
    #[allow(clippy::uninlined_format_args)]
    let fmtg = |v: f64| format!("{:>8.*}", gprec, v);
    let gains_lines = format!(
        "{}Kp  {}  {}  proportional — immediate reaction\n\
         {}Ki  {}  {}  integral — drift correction\n\
         {}Kd  {}  {}  derivative — damping / braking\n\
         {}SP  {}  {}  setpoint target",
        if ui.focus == GainFocus::Kp {
            "▶ "
        } else {
            "  "
        },
        fmtg(cfg.kp),
        step_cell_for_row(ui.focus, 0, &ui.step, gprec),
        if ui.focus == GainFocus::Ki {
            "▶ "
        } else {
            "  "
        },
        fmtg(cfg.ki),
        step_cell_for_row(ui.focus, 1, &ui.step, gprec),
        if ui.focus == GainFocus::Kd {
            "▶ "
        } else {
            "  "
        },
        fmtg(cfg.kd),
        step_cell_for_row(ui.focus, 2, &ui.step, gprec),
        if ui.focus == GainFocus::Sp {
            "▶ "
        } else {
            "  "
        },
        fmtg(cfg.setpoint),
        step_cell_for_row(ui.focus, 3, &ui.step, gprec),
    );

    let process_block = format!(
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n\
         PROCESS                                              HINT\n\
         ──────────────────────────────────────────────────────────────────────\n\
         Setpoint      {sp:>8.3} {u}    target (ramps when --setpoint-ramp is active)\n\
         PV (actual)   {pv:>8.3} {u}    sensor reading\n\
         Error         {err:>8.3} {u}    Setpoint−PV  ({err_hint})\n\
         \n\
         OUTPUT\n\
         ──────────────────────────────────────────────────────────────────────\n\
         CV            {cv:>8.3}  {bar} {pct}    last applied / commanded output\n\
         Range         {lo:.3} – {hi:.3}    hold={hold}  dry_run={dry}\n\
         \n\
         PID BREAKDOWN\n\
         ──────────────────────────────────────────────────────────────────────\n\
         P  (proportional)   {p:>+9.4}    responds to current error\n\
         I  (integral)       {i:>+9.4}    persistent offset correction\n\
         D  (derivative)     {d:>+9.4}    PV rate (D-on-measurement)\n\
         I accumulator       {iac:>+9.4}    {iacc_hint}",
        sp = cfg.setpoint,
        u = units,
        pv = pv_val,
        err = err_val,
        err_hint = err_hint,
        cv = cv_val,
        bar = bar_str,
        pct = pct_str,
        lo = cfg.out_min,
        hi = cfg.out_max,
        hold = ui.hold,
        dry = ui.dry_run,
        p = ui.last_record.as_ref().map_or(0.0, |r| r.p),
        i = ui.last_record.as_ref().map_or(0.0, |r| r.i),
        d = ui.last_record.as_ref().map_or(0.0, |r| r.d),
        iac = ui.last_record.as_ref().map_or(0.0, |r| r.i_acc),
        iacc_hint = i_acc_hint,
    );

    // Gains rendered in its own fixed slot so it is always visible regardless of
    // terminal height. body_chunks[1] is Length(6): header + separator + 4 gain rows.
    let gains_block = format!(
        "GAINS — ↑↓ select   ←→ adjust   [ ] step size              s save   q quit\n\
         ──────────────────────────────────────────────────────────────────────\n\
         {gains_lines}"
    );

    let hist_title = format!(
        "HISTORY (last {} ticks / ~{:.0}s wall at {:.1}s interval)",
        args.tune_history, hist_wall_s, interval_secs
    );

    f.render_widget(
        Paragraph::new(process_block).wrap(Wrap { trim: true }),
        body_chunks[0],
    );
    f.render_widget(
        Paragraph::new(gains_block).style(Style::default().fg(Color::Yellow)),
        body_chunks[1],
    );

    // Sparklines use Fill(1) so they share all remaining height equally rather than
    // having a fixed Length that can overflow body_chunks[2] in ratatui 0.30's layout
    // engine.  Fill adapts to the available area: on a 24-row terminal each sparkline
    // gets ~3 rows (title + 2 bar rows); on a taller terminal it grows further.
    // Block::inner() subtracts 1 row for the block title, so the bar area is height-1.
    let hist_inner = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // [0] HISTORY title
            Constraint::Fill(1),   // [1] PV sparkline — expands with terminal height
            Constraint::Length(1), // [2] PV marker row
            Constraint::Fill(1),   // [3] CV sparkline — expands with terminal height
            Constraint::Length(1), // [4] CV marker row
            Constraint::Length(1), // [5] caret line
        ])
        .split(body_chunks[2]);

    f.render_widget(Paragraph::new(hist_title), hist_inner[0]);

    let pv_trend = history_trend(&ui.pv_history);
    let pv_range_str = pv_scale
        .map(|(lo, hi)| format!("  [{lo:.2} – {hi:.2}]"))
        .unwrap_or_default();
    let pv_title = format!(
        "PV {pv_trend} {pv_val:.2}  SP {:.2}{pv_range_str}",
        cfg.setpoint
    );

    // Canvas overlay: PV (cyan) and SP (yellow) plotted on the same coordinate space.
    // X = column index (oldest=0, newest=right), Y = actual engineering value.
    // Enforce minimum zoom of ±1% of |setpoint| so the graph never collapses to a dot.
    let sp_min_half = (0.01 * cfg.setpoint.abs()).max(1e-6);
    let (raw_y_lo, raw_y_hi) =
        pv_scale.unwrap_or((cfg.setpoint - sp_min_half, cfg.setpoint + sp_min_half));
    let y_lo = raw_y_lo.min(cfg.setpoint - sp_min_half);
    let y_hi = raw_y_hi.max(cfg.setpoint + sp_min_half);
    let pv_start = ui.pv_history.len().saturating_sub(spark_w);
    let sp_start = ui.sp_history.len().saturating_sub(spark_w);
    #[allow(clippy::cast_precision_loss)]
    let pv_coords: Vec<(f64, f64)> = ui
        .pv_history
        .iter()
        .skip(pv_start)
        .enumerate()
        .filter_map(|(i, &v)| v.is_finite().then_some((i as f64, v)))
        .collect();
    #[allow(clippy::cast_precision_loss)]
    let sp_coords: Vec<(f64, f64)> = ui
        .sp_history
        .iter()
        .skip(sp_start)
        .enumerate()
        .filter_map(|(i, &v)| v.is_finite().then_some((i as f64, v)))
        .collect();
    #[allow(clippy::cast_precision_loss)]
    let x_max = (spark_w as f64 - 1.0).max(1.0);
    let ticks = scale_ticks(y_lo, y_hi, cfg.setpoint, hist_inner[1].height);
    f.render_widget(
        Canvas::default()
            .block(Block::default().title(pv_title))
            .marker(Marker::Braille)
            .x_bounds([0.0, x_max])
            .y_bounds([y_lo, y_hi])
            .paint(move |ctx| {
                ctx.draw(&Points {
                    coords: &pv_coords,
                    color: Color::LightCyan,
                });
                ctx.layer();
                ctx.draw(&Points {
                    coords: &sp_coords,
                    color: Color::LightYellow,
                });
                ctx.layer();
                // Scale ruler: draw tick characters on the far-right column.
                for (y, sym) in &ticks {
                    ctx.print(
                        x_max,
                        *y,
                        ratatui::text::Span::styled(*sym, Style::default().fg(Color::DarkGray)),
                    );
                }
            }),
        hist_inner[1],
    );

    f.render_widget(
        Paragraph::new(marker_row.clone()).style(Style::default().fg(Color::White)),
        hist_inner[2],
    );

    let cv_trend = history_trend(&ui.cv_history);
    let cv_range_str = history_range(&ui.cv_history)
        .map(|(lo, hi)| format!("  [{lo:.2} – {hi:.2}]"))
        .unwrap_or_default();
    let cv_title = format!("CV {cv_trend} {cv_val:.2}{cv_range_str}");
    let cv_sparkline = Sparkline::default()
        .data(&cv_spark)
        .style(Style::default().fg(Color::LightGreen));
    f.render_widget(
        cv_sparkline.block(Block::default().title(cv_title)),
        hist_inner[3],
    );
    f.render_widget(
        Paragraph::new(marker_row).style(Style::default().fg(Color::White)),
        hist_inner[4],
    );

    let caret_para = if caret_line.is_empty() {
        String::new()
    } else {
        caret_line
    };
    f.render_widget(Paragraph::new(caret_para), hist_inner[5]);

    let keymap = "↑↓ select  ←→ adjust  [] step  / cmd  r reset  s save  c export  h hold  d dry-run  ? help  q quit";
    let flash_msg = ui.status_flash.as_ref().and_then(|(msg, t)| {
        if t.elapsed() < Duration::from_secs(3) {
            Some(msg.as_str())
        } else {
            None
        }
    });
    let footer_text = if let Some(msg) = flash_msg {
        format!("{msg}  |  {keymap}")
    } else {
        keymap.to_string()
    };
    let footer_style = if flash_msg.is_some() {
        Style::default().fg(Color::Green)
    } else {
        Style::default()
    };
    let footer = Paragraph::new(footer_text)
        .style(footer_style)
        .block(Block::default().borders(Borders::TOP));
    f.render_widget(footer, chunks[2]);

    if ui.command_mode {
        use ratatui::style::Modifier;
        use ratatui::widgets::Clear;
        let hint = command_mode_hint(
            &ui.command_buf,
            cfg,
            args.runtime.interval,
            args.units.as_deref(),
        );
        let area = centered_rect(88, 18, f.area());
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Command (Esc)");
        f.render_widget(Clear, area);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let [hint_area, input_area] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .areas(inner);
        if !hint.is_empty() {
            f.render_widget(Paragraph::new(hint).wrap(Wrap { trim: true }), hint_area);
        }
        let cursor_style = Style::default().add_modifier(Modifier::REVERSED);
        let input_line = Line::from(vec![
            Span::raw(format!("> {}", ui.command_buf)),
            Span::styled(" ", cursor_style),
        ]);
        f.render_widget(Paragraph::new(input_line), input_area);
    }
}

pub(in crate::tune) fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
