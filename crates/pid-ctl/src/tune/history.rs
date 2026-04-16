use super::model::GainAnnotation;
use std::collections::VecDeque;

pub(in crate::tune) fn expand_scale(lo: f64, hi: f64, center: f64) -> (f64, f64) {
    let min_span = (0.01 * center.abs()).max(1e-9);
    let actual_span = hi - lo;
    if actual_span >= min_span {
        return (lo, hi);
    }
    let half = (min_span * 0.5).max(hi - center).max(center - lo);
    (center - half, center + half)
}

pub(in crate::tune) fn history_range(history: &VecDeque<f64>) -> Option<(f64, f64)> {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for &v in history {
        if v.is_finite() {
            lo = lo.min(v);
            hi = hi.max(v);
            sum += v;
            count += 1;
        }
    }
    if count == 0 {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    let mean = sum / count as f64;
    Some(expand_scale(lo, hi, mean))
}

/// Compute tick positions for the right-edge scale ruler on the PV canvas.
///
/// Returns `(y_value, symbol)` pairs for each tick that falls within `[y_lo, y_hi]`.
///
/// ## Stability guarantee
///
/// `sub = base_unit / 10` is fixed to the setpoint's natural scale — it never
/// changes with zoom.  Ticks are classified by `k_rel = k − k_sp` (integer offset
/// from setpoint), so a given world Y always maps to the same character regardless
/// of the current zoom level.  As zoom changes, ticks only appear or disappear at
/// the boundary of the visible range; they never change character.
///
/// | Symbol | Distance from setpoint      |
/// |--------|-----------------------------|
/// | `╣`    | 0 (setpoint itself) or ×10          |
/// | `╡`    | ×5 × `base_unit`                    |
/// | `┤`    | ×1 × `base_unit`                    |
/// | `╴`    | ×0.5 × `base_unit`                  |
/// | `·`    | ×0.1 × `base_unit` (sub-step)       |
///
/// `base_unit = 10^(floor(log10(|SP|)) - 1)`:
///   `SP=60` → `base_unit=1`,  `SP=600` → `base_unit=10`,  `SP=6` → `base_unit=0.1`
pub(in crate::tune) fn scale_ticks(
    y_lo: f64,
    y_hi: f64,
    setpoint: f64,
    canvas_rows: u16,
) -> Vec<(f64, &'static str)> {
    let span = y_hi - y_lo;
    if span <= 0.0 {
        return vec![];
    }

    // Natural scale unit for this setpoint magnitude.
    let sp_abs = setpoint.abs().max(1e-9);
    let base_unit = 10_f64.powf(sp_abs.log10().floor() - 1.0);
    // Finest sub-step — FIXED for a given SP, independent of current zoom.
    let sub = base_unit / 10.0;

    // Integer index of setpoint on the sub-step grid.  `.round()` absorbs floating-
    // point error in setpoint/sub (error << 0.5 for any reasonable SP).
    #[allow(clippy::cast_possible_truncation)]
    let k_sp = (setpoint / sub).round() as i64;

    // Number of sub-steps visible in the current range.
    #[allow(clippy::cast_possible_truncation)]
    let sub_count = (span / sub).round() as i64;

    // Stride through sub-steps so at most `canvas_rows` ticks are emitted.
    // With this limit, consecutive ticks are guaranteed to be at least 1 row
    // apart: tick_spacing = stride × sub, row_height = span / canvas_rows,
    // so ticks_per_row = row_height / tick_spacing = sub_count / (stride × canvas_rows) ≤ 1.
    // Stride from {1,5,10,50,…} so each zoom level naturally hides finer ticks.
    #[allow(clippy::items_after_statements)]
    const STRIDES: &[i64] = &[1, 5, 10, 50, 100, 500, 1_000, 5_000, 10_000];
    let max_ticks = i64::from(canvas_rows.max(1));
    let stride = STRIDES
        .iter()
        .copied()
        .find(|&s| sub_count / s <= max_ticks)
        .unwrap_or(10_000);

    // First k ≥ ceil(y_lo/sub) that sits on a stride boundary relative to k_sp.
    #[allow(clippy::cast_possible_truncation)]
    let first_k_raw = (y_lo / sub).ceil() as i64;
    let offset = (first_k_raw - k_sp).rem_euclid(stride);
    let first_k = first_k_raw + if offset == 0 { 0 } else { stride - offset };
    #[allow(clippy::cast_possible_truncation)]
    let last_k = (y_hi / sub).floor() as i64;

    if first_k > last_k {
        return vec![];
    }

    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let capacity = ((last_k - first_k) / stride + 1) as usize;
    let mut ticks = Vec::with_capacity(capacity);
    let mut k = first_k;
    while k <= last_k {
        #[allow(clippy::cast_precision_loss)]
        let y = k as f64 * sub;
        let k_rel = k - k_sp;
        let sym = if k_rel % 100 == 0 {
            "╣" // setpoint, or ±10 × base_unit
        } else if k_rel % 50 == 0 {
            "╡" // ±5 × base_unit
        } else if k_rel % 10 == 0 {
            "┤" // ±1 × base_unit
        } else if k_rel % 5 == 0 {
            "╴" // ±0.5 × base_unit
        } else {
            "·" // ±0.1 × base_unit (sub-step)
        };
        ticks.push((y, sym));
        k += stride;
    }
    ticks
}

pub(in crate::tune) fn spark_data(values: &VecDeque<f64>) -> Vec<u64> {
    if values.is_empty() {
        return vec![];
    }
    let mut min_v = f64::INFINITY;
    let mut max_v = f64::NEG_INFINITY;
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for &v in values {
        if v.is_finite() {
            min_v = min_v.min(v);
            max_v = max_v.max(v);
            sum += v;
            count += 1;
        }
    }
    if count == 0 {
        return vec![0; values.len()];
    }
    #[allow(clippy::cast_precision_loss)]
    let mean = sum / count as f64;
    let (lo, hi) = expand_scale(min_v, max_v, mean);
    let span = hi - lo;
    if span <= 1e-9 {
        // Constant series: `(v - min) / span` would be all zeros — ratatui draws no visible bars.
        // Use a flat mid-line so history is visible (e.g. dry-run + sim: PV stuck until CV reaches plant).
        return vec![50; values.len()];
    }
    values
        .iter()
        .map(|v| {
            if !v.is_finite() {
                return 0u64;
            }
            // Value is clamped to [0.0, 100.0] before rounding, so truncation and sign loss are safe.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                (((v - lo) / span) * 100.0).clamp(0.0, 100.0).round() as u64
            }
        })
        .collect()
}

pub(in crate::tune) fn spark_tail_slice<T: Clone>(values: &[T], max_w: usize) -> Vec<T> {
    let n = values.len();
    if n == 0 {
        return vec![];
    }
    let take = n.min(max_w);
    values[n - take..].to_vec()
}

pub(in crate::tune) fn spark_marker_row(
    serial_window: &[u64],
    annotations: &VecDeque<GainAnnotation>,
    width: usize,
) -> String {
    let w = width.max(1);
    // Start with time-tick dots at multiples of 10.
    let mut chars: Vec<char> = serial_window
        .iter()
        .take(w)
        .map(|&s| if s % 10 == 0 { '·' } else { ' ' })
        .collect();
    while chars.len() < w {
        chars.push(' ');
    }
    // Gain-change pipes overwrite dots.
    for ann in annotations {
        if let Some(col) = serial_window.iter().position(|s| *s == ann.marker_tick)
            && col < w
        {
            chars[col] = '|';
        }
    }
    chars.into_iter().collect()
}

pub(in crate::tune) fn annotation_caret_line(
    serial_window: &[u64],
    annotations: &VecDeque<GainAnnotation>,
    max_width: usize,
) -> String {
    let w = max_width.max(1);
    let mut chars: Vec<char> = vec![' '; w];
    // Iterate oldest→newest so newer annotations overwrite older ones.
    for ann in annotations {
        let Some(col) = serial_window.iter().position(|s| *s == ann.marker_tick) else {
            continue;
        };
        if col >= w {
            continue;
        }
        let label = ann.display_text();
        // Place the caret at the marker column, then the label text two chars to the right.
        chars[col] = '^';
        let text_start = col + 2;
        for (i, ch) in label.chars().enumerate() {
            let pos = text_start + i;
            if pos < w {
                chars[pos] = ch;
            }
        }
    }
    // Trim trailing spaces.
    let s: String = chars.into_iter().collect();
    s.trim_end().to_string()
}

pub(in crate::tune) fn history_trend(history: &VecDeque<f64>) -> &'static str {
    let first = history.iter().find(|v| v.is_finite()).copied();
    let last = history.iter().rev().find(|v| v.is_finite()).copied();
    match (first, last) {
        (Some(f), Some(l)) if l > f + 1e-9 => "▲",
        (Some(f), Some(l)) if l < f - 1e-9 => "▼",
        _ => "→",
    }
}
