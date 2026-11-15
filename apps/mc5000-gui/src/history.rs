//! Per-slot measurement history and the chart geometry drawn from it.

use std::{cell::RefCell, collections::VecDeque, fmt::Write as _, rc::Rc, time::Instant};

use chrono::{DateTime, Local};
use mc5000_protocol::{LedColor, Slot, SlotStatus};

/// Six hours of samples at the 1 Hz poll rate.
const MAX_SAMPLES: usize = 6 * 60 * 60;
/// Upper bound on plotted points per series, regardless of history length.
const MAX_POINTS: usize = 600;
/// A pause longer than this between samples breaks the line instead of bridging it.
const GAP_SECONDS: f64 = 5.0;
/// Narrowest time axis, so a fresh history doesn't stretch two samples edge to edge.
const MIN_SPAN_SECONDS: f64 = 60.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Metric {
    Voltage,
    Current,
    Temperature,
    Capacity,
}

impl Metric {
    pub(crate) const ALL: [Self; 4] = [
        Self::Voltage,
        Self::Current,
        Self::Temperature,
        Self::Capacity,
    ];

    pub(crate) fn from_index(index: i32) -> Self {
        usize::try_from(index)
            .ok()
            .and_then(|index| Self::ALL.get(index).copied())
            .unwrap_or(Self::Voltage)
    }

    const fn index(self) -> usize {
        self as usize
    }

    /// Smallest y-axis span, so sensor noise isn't magnified into a jagged line.
    const fn min_span(self) -> f64 {
        match self {
            Self::Voltage | Self::Current => 0.1,
            Self::Temperature => 2.0,
            Self::Capacity => 10.0,
        }
    }

    /// Formats a reading at the charger's full resolution (mV, mA, 0.1 °C, mAh).
    fn format_precise(self, value: f64) -> String {
        match self {
            Self::Voltage => format!("{value:.3} V"),
            Self::Current => format!("{value:.3} A"),
            Self::Temperature => format!("{value:.1} °C"),
            Self::Capacity => format!("{value:.0} mAh"),
        }
    }

    /// Formats an axis label, adding decimals when zoomed into a narrow `span`.
    fn format(self, value: f64, span: f64) -> String {
        let (base, unit) = match self {
            Self::Voltage => (2, "V"),
            Self::Current => (2, "A"),
            Self::Temperature => (1, "°C"),
            Self::Capacity => (0, "mAh"),
        };
        // Enough decimals to tell the three labels (span / 2 apart) apart, capped at 4.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let needed = (-(span / 2.0).log10()).ceil().clamp(0.0, 4.0) as usize;
        let decimals = needed.max(base);
        format!("{value:.decimals$} {unit}")
    }
}

#[derive(Clone, Copy, Debug)]
struct Sample {
    seconds: f64,
    values: [f64; 4],
}

/// Selectable time axis lengths, ending at "now". `None` shows all history.
pub(crate) const TIME_WINDOWS: [(&str, Option<f64>); 7] = [
    ("All", None),
    ("1 min", Some(60.0)),
    ("5 min", Some(300.0)),
    ("15 min", Some(900.0)),
    ("30 min", Some(1800.0)),
    ("1 h", Some(3600.0)),
    ("3 h", Some(10_800.0)),
];

pub(crate) fn time_window(index: i32) -> Option<f64> {
    usize::try_from(index)
        .ok()
        .and_then(|index| TIME_WINDOWS.get(index))
        .and_then(|(_, seconds)| *seconds)
}

/// How lines are drawn; the index matches the UI's `chart-smoothing`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Smoothing {
    /// The raw readings.
    #[default]
    Off,
    /// A centred moving average.
    Average,
    /// A penalised least-squares cubic spline fitted to each recorded run.
    Spline,
}

impl Smoothing {
    pub(crate) fn from_index(index: i32) -> Self {
        match index {
            1 => Self::Average,
            2 => Self::Spline,
            _ => Self::Off,
        }
    }

    /// Smooths `(seconds, value)` samples for a chart spanning `span` seconds.
    fn apply(self, samples: &[(f64, f64)], span: f64) -> Vec<f64> {
        match self {
            Self::Off => samples.iter().map(|&(_, value)| value).collect(),
            Self::Average => moving_average(samples, smoothing_window(span) / 2.0),
            Self::Spline => spline_fit(samples, knot_spacing(span)),
        }
    }

    /// How far before the visible range samples are needed so the smoothed
    /// line at the left edge matches what it would be mid-chart.
    fn lead_seconds(self, span: f64) -> f64 {
        match self {
            Self::Off => 0.0,
            Self::Average => smoothing_window(span) / 2.0,
            Self::Spline => knot_spacing(span) * SPLINE_LEAD_KNOTS,
        }
    }
}

/// How the chart is framed: the time window, a manual y-range while zoomed,
/// and how lines are smoothed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ChartView {
    pub(crate) window: Option<f64>,
    pub(crate) y_range: Option<(f64, f64)>,
    pub(crate) smoothing: Smoothing,
}

/// Scales `(low, high)` by `factor` (< 1 zooms in) around `anchor`, a fraction
/// of the plot height measured from the top, keeping that value under the cursor.
pub(crate) fn zoom_range((low, high): (f64, f64), factor: f64, anchor: f64) -> (f64, f64) {
    let anchor = anchor.clamp(0.0, 1.0);
    let pivot = high - (high - low) * anchor;
    let span = ((high - low) * factor).clamp(1e-4, 1e6);
    let high = pivot + span * anchor;
    (high - span, high)
}

/// Shifts `(low, high)` by `fraction` of its span; positive moves the view up,
/// matching dragging the plot downwards.
pub(crate) fn pan_range((low, high): (f64, f64), fraction: f64) -> (f64, f64) {
    let shift = (high - low) * fraction;
    (low + shift, high + shift)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Chart {
    /// SVG-style path commands per slot in plot pixels; empty when the slot has nothing to plot.
    pub(crate) paths: [String; 4],
    pub(crate) y_max: String,
    pub(crate) y_mid: String,
    pub(crate) y_min: String,
    pub(crate) x_start: String,
    pub(crate) empty: bool,
    /// The plotted value range, the starting point for zooming and panning.
    pub(crate) range: (f64, f64),
}

#[derive(Debug)]
pub(crate) struct History {
    origin: Instant,
    slots: [VecDeque<Sample>; 4],
    /// Bumped whenever the samples change, invalidating `cache`.
    revision: u64,
    /// The last plot built, so a refresh's chart and hover probe, and hovering
    /// between refreshes, don't repeat the smoothing work.
    cache: RefCell<Option<(PlotKey, Option<Rc<Plot>>)>>,
}

/// Everything a plot depends on.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PlotKey {
    metric: Metric,
    hidden: [bool; 4],
    view: ChartView,
    now: Instant,
    revision: u64,
}

impl History {
    pub(crate) fn new(origin: Instant) -> Self {
        Self {
            origin,
            slots: Default::default(),
            revision: 0,
            cache: RefCell::default(),
        }
    }

    /// Records a sample while the slot is active; idle slots only report
    /// held or zeroed values, which would flatten the graph.
    pub(crate) fn record(&mut self, slot: Slot, status: &SlotStatus, now: Instant) {
        if status.led_color() == LedColor::None {
            return;
        }
        self.revision += 1;
        let samples = &mut self.slots[slot.index()];
        if samples.len() == MAX_SAMPLES {
            samples.pop_front();
        }
        samples.push_back(Sample {
            seconds: now.saturating_duration_since(self.origin).as_secs_f64(),
            values: [
                status.voltage_volts(),
                status.current_amps(),
                status.temperature_celsius(),
                f64::from(status.capacity_mah),
            ],
        });
    }

    /// Every captured sample as CSV, oldest first, one row per slot reading.
    /// `now` and `wall_now` are the same moment, used to timestamp the rows.
    /// `None` when nothing has been captured.
    pub(crate) fn to_csv(&self, now: Instant, wall_now: DateTime<Local>) -> Option<String> {
        let mut rows = self
            .slots
            .iter()
            .enumerate()
            .flat_map(|(slot, samples)| samples.iter().map(move |sample| (slot, sample)))
            .collect::<Vec<_>>();
        rows.sort_by(|(a_slot, a), (b_slot, b)| {
            a.seconds.total_cmp(&b.seconds).then(a_slot.cmp(b_slot))
        });
        let first = rows.first()?.1.seconds;
        let end = now.saturating_duration_since(self.origin).as_secs_f64();

        let mut csv = String::from(
            "timestamp,elapsed_s,slot,voltage_v,current_a,temperature_c,capacity_mah\n",
        );
        for (slot, sample) in rows {
            let ago = chrono::Duration::milliseconds(
                // Within the six hours kept, so this always fits.
                #[allow(clippy::cast_possible_truncation)]
                {
                    ((end - sample.seconds).max(0.0) * 1000.0).round() as i64
                },
            );
            let [voltage, current, temperature, capacity] = sample.values;
            // Writing to a String cannot fail.
            let _ = writeln!(
                csv,
                "{},{:.1},{},{voltage:.3},{current:.3},{temperature:.1},{capacity:.0}",
                (wall_now - ago).format("%Y-%m-%dT%H:%M:%S%:z"),
                sample.seconds - first,
                slot + 1,
            );
        }
        Some(csv)
    }

    pub(crate) fn clear(&mut self) {
        self.revision += 1;
        for samples in &mut self.slots {
            samples.clear();
        }
    }

    /// Builds the chart for a plot of `width` x `height` logical pixels. Slint
    /// scales paths uniformly, so they are generated at the plot's exact size.
    pub(crate) fn chart(
        &self,
        metric: Metric,
        hidden: [bool; 4],
        view: ChartView,
        now: Instant,
        (width, height): (f64, f64),
    ) -> Chart {
        let Some(plot) = self.plot(metric, hidden, view, now) else {
            return Chart {
                empty: true,
                ..Chart::default()
            };
        };
        let Plot { frame, series } = &*plot;
        let frame = *frame;

        let mut paths: [String; 4] = Default::default();
        for (points, path) in series.iter().zip(&mut paths) {
            let stride = points.len().div_ceil(MAX_POINTS).max(1);
            let gap_seconds = GAP_SECONDS * f64::from(u32::try_from(stride).unwrap_or(u32::MAX));
            let mut previous: Option<f64> = None;
            for point in points.iter().step_by(stride) {
                let x = frame.x(point.seconds, width);
                let y = frame.y(point.value, height);
                let command = match previous {
                    Some(seconds) if point.seconds - seconds <= gap_seconds => 'L',
                    _ => 'M',
                };
                // Writing to a String cannot fail.
                let _ = write!(path, "{command} {x:.1} {y:.1} ");
                previous = Some(point.seconds);
            }
        }

        let Frame {
            span, low, high, ..
        } = frame;
        let span_y = high - low;
        Chart {
            paths,
            y_max: metric.format(high, span_y),
            y_mid: metric.format(f64::midpoint(high, low), span_y),
            y_min: metric.format(low, span_y),
            x_start: format!("−{}", format_span(span)),
            empty: false,
            range: (low, high),
        }
    }

    /// Finds the plotted point nearest the cursor at `(x, y)` plot pixels, if
    /// one is within [`HOVER_RADIUS`] of it.
    pub(crate) fn probe(
        &self,
        metric: Metric,
        hidden: [bool; 4],
        view: ChartView,
        now: Instant,
        (width, height): (f64, f64),
        (x, y): (f64, f64),
    ) -> Option<Probe> {
        let plot = self.plot(metric, hidden, view, now)?;
        let Plot { frame, series } = &*plot;
        let frame = *frame;
        let cursor_seconds = frame.start + x / width * frame.span;
        let mut best: Option<(f64, Probe)> = None;
        for (slot, points) in series.iter().enumerate() {
            let after = points.partition_point(|point| point.seconds < cursor_seconds);
            let Some(point) = [after.checked_sub(1), Some(after)]
                .into_iter()
                .flatten()
                .filter_map(|index| points.get(index))
                // The point leading in from the left edge isn't in view.
                .filter(|point| point.seconds >= frame.start)
                .min_by(|a, b| {
                    (a.seconds - cursor_seconds)
                        .abs()
                        .total_cmp(&(b.seconds - cursor_seconds).abs())
                })
            else {
                continue;
            };
            let position = (frame.x(point.seconds, width), frame.y(point.value, height));
            let distance = (position.0 - x).hypot(position.1 - y);
            if distance > HOVER_RADIUS || best.as_ref().is_some_and(|(d, _)| *d <= distance) {
                continue;
            }
            best = Some((
                distance,
                Probe {
                    slot,
                    x: position.0,
                    y: position.1,
                    value: metric.format_precise(point.raw),
                    smoothed: (view.smoothing != Smoothing::Off)
                        .then(|| metric.format_precise(point.value)),
                    seconds_ago: frame.end - point.seconds,
                },
            ));
        }
        best.map(|(_, probe)| probe)
    }

    /// The plot for these inputs, reusing the last one when nothing changed.
    fn plot(
        &self,
        metric: Metric,
        hidden: [bool; 4],
        view: ChartView,
        now: Instant,
    ) -> Option<Rc<Plot>> {
        let key = PlotKey {
            metric,
            hidden,
            view,
            now,
            revision: self.revision,
        };
        if let Some((cached, plot)) = &*self.cache.borrow()
            && *cached == key
        {
            return plot.clone();
        }
        let plot = self.build_plot(metric, hidden, view, now).map(Rc::new);
        *self.cache.borrow_mut() = Some((key, plot.clone()));
        plot
    }

    /// Works out the visible time range, the series to draw (smoothed when
    /// enabled) and the value range; `None` when nothing is shown.
    fn build_plot(
        &self,
        metric: Metric,
        hidden: [bool; 4],
        view: ChartView,
        now: Instant,
    ) -> Option<Plot> {
        let end = now.saturating_duration_since(self.origin).as_secs_f64();
        let window_start = view.window.map(|window| end - window);
        // Index of a slot's first sample inside the time window.
        let first_visible = |samples: &VecDeque<Sample>| {
            window_start.map_or(0, |start| {
                samples.partition_point(|sample| sample.seconds < start)
            })
        };
        let first = self
            .slots
            .iter()
            .zip(hidden)
            .filter(|(_, hidden)| !hidden)
            .filter_map(|(samples, _)| samples.get(first_visible(samples)))
            .map(|sample| sample.seconds)
            .reduce(f64::min)?;
        let start = window_start.unwrap_or_else(|| first.min(end - MIN_SPAN_SECONDS));
        let span = end - start;
        let lead_seconds = view.smoothing.lead_seconds(span);

        let series: [Vec<Point>; 4] = std::array::from_fn(|slot| {
            let samples = &self.slots[slot];
            let from = first_visible(samples);
            if hidden[slot] || from == samples.len() {
                return Vec::new();
            }
            // One sample before the window keeps the line running into the left edge.
            let from = from.saturating_sub(1);
            // Smoothing near the left edge also takes in samples just outside the view.
            let edge = samples[from].seconds - lead_seconds;
            let lead = samples.partition_point(|sample| sample.seconds < edge);
            let raw = samples
                .range(lead..)
                .map(|sample| (sample.seconds, sample.values[metric.index()]))
                .collect::<Vec<_>>();
            let values = view.smoothing.apply(&raw, span);
            raw.iter()
                .zip(values)
                .skip(from - lead)
                .map(|(&(seconds, raw), value)| Point {
                    seconds,
                    value,
                    raw,
                })
                .collect()
        });

        let (low, high) = view.y_range.unwrap_or_else(|| {
            let values = series
                .iter()
                .flatten()
                .filter(|point| point.seconds >= start)
                .map(|point| point.value);
            auto_range(metric, values)
        });
        Some(Plot {
            frame: Frame {
                start,
                span,
                end,
                low,
                high,
            },
            series,
        })
    }
}

/// Distance in plot pixels within which hovering picks up a data point.
pub(crate) const HOVER_RADIUS: f64 = 16.0;

/// The data point under the cursor.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Probe {
    pub(crate) slot: usize,
    /// Position of the point in plot pixels.
    pub(crate) x: f64,
    pub(crate) y: f64,
    /// The reading as measured.
    pub(crate) value: String,
    /// The smoothed value the line is drawn at, while smoothing.
    pub(crate) smoothed: Option<String>,
    pub(crate) seconds_ago: f64,
}

/// One plotted point: `value` is what's drawn, `raw` what was measured.
#[derive(Clone, Copy, Debug)]
struct Point {
    seconds: f64,
    value: f64,
    raw: f64,
}

#[derive(Debug)]
struct Plot {
    frame: Frame,
    /// Points per slot, empty for hidden or idle slots.
    series: [Vec<Point>; 4],
}

/// Mapping from samples to plot pixels for one chart refresh.
#[derive(Clone, Copy, Debug)]
struct Frame {
    start: f64,
    span: f64,
    end: f64,
    low: f64,
    high: f64,
}

impl Frame {
    fn x(&self, seconds: f64, width: f64) -> f64 {
        (seconds - self.start) / self.span * width
    }

    fn y(&self, value: f64, height: f64) -> f64 {
        (1.0 - (value - self.low) / (self.high - self.low)) * height
    }
}

/// Smoothing averages over this fraction of the visible time span, so the
/// effect looks the same at every time scale.
const SMOOTHING_FRACTION: f64 = 1.0 / 40.0;
/// Shortest averaging window, a few polls even when zoomed into a minute.
const MIN_SMOOTHING_SECONDS: f64 = 5.0;

fn smoothing_window(span: f64) -> f64 {
    (span * SMOOTHING_FRACTION).max(MIN_SMOOTHING_SECONDS)
}

/// Centred moving average of `(seconds, value)` samples over `half_window`
/// seconds either side. Averages never reach across a gap in recording.
fn moving_average(samples: &[(f64, f64)], half_window: f64) -> Vec<f64> {
    let mut prefix = Vec::with_capacity(samples.len() + 1);
    prefix.push(0.0);
    let mut segment = Vec::with_capacity(samples.len());
    let mut current = 0_usize;
    for (index, &(seconds, value)) in samples.iter().enumerate() {
        if index > 0 && seconds - samples[index - 1].0 > GAP_SECONDS {
            current += 1;
        }
        segment.push(current);
        prefix.push(prefix[index] + value);
    }
    let (mut low, mut high) = (0, 0);
    samples
        .iter()
        .enumerate()
        .map(|(index, &(seconds, _))| {
            while samples[low].0 < seconds - half_window || segment[low] != segment[index] {
                low += 1;
            }
            high = high.max(index);
            while high + 1 < samples.len()
                && samples[high + 1].0 <= seconds + half_window
                && segment[high + 1] == segment[index]
            {
                high += 1;
            }
            #[allow(clippy::cast_precision_loss)]
            let count = (high - low + 1) as f64;
            (prefix[high + 1] - prefix[low]) / count
        })
        .collect()
}

/// Spline knots are spread over this fraction of the visible time span.
const SPLINE_FRACTION: f64 = 1.0 / 20.0;
/// Closest knot spacing, so a one-minute view still smooths over several polls.
const MIN_KNOT_SECONDS: f64 = 10.0;
/// Knot intervals fitted before the visible range, so the curve at the left
/// edge is constrained by data on both sides.
const SPLINE_LEAD_KNOTS: f64 = 3.0;
/// Weight of the curvature penalty relative to the samples per knot interval.
/// It keeps the fit steady across sparse intervals and irons out wiggles
/// between knots without pulling straight trends off course.
const SPLINE_PENALTY: f64 = 1.0;

/// Weights of a second difference, the penalty's measure of curvature.
const SECOND_DIFFERENCE: [f64; 3] = [1.0, -2.0, 1.0];

fn knot_spacing(span: f64) -> f64 {
    (span * SPLINE_FRACTION).max(MIN_KNOT_SECONDS)
}

/// Fits a penalised cubic B-spline (a P-spline) with knots about `spacing`
/// seconds apart to each gap-free run of `(seconds, value)` samples, and
/// returns the fitted value at every sample. Unlike a moving average, the fit
/// follows a steady trend right up to the ends of a run.
fn spline_fit(samples: &[(f64, f64)], spacing: f64) -> Vec<f64> {
    let mut fitted = Vec::with_capacity(samples.len());
    let mut start = 0;
    for end in 1..=samples.len() {
        if end == samples.len() || samples[end].0 - samples[end - 1].0 > GAP_SECONDS {
            fitted.extend(fit_run(&samples[start..end], spacing));
            start = end;
        }
    }
    fitted
}

/// Uniform cubic B-spline basis: the first of the four coefficients that are
/// non-zero at `seconds`, and their weights.
fn basis(seconds: f64, origin: f64, step: f64, intervals: usize) -> (usize, [f64; 4]) {
    let position = ((seconds - origin) / step).max(0.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let interval = (position.floor() as usize).min(intervals - 1);
    #[allow(clippy::cast_precision_loss)]
    let t = position - interval as f64;
    let u = 1.0 - t;
    (
        interval,
        [
            u * u * u / 6.0,
            (3.0 * t * t * t - 6.0 * t * t + 4.0) / 6.0,
            (-3.0 * t * t * t + 3.0 * t * t + 3.0 * t + 1.0) / 6.0,
            t * t * t / 6.0,
        ],
    )
}

fn fit_run(run: &[(f64, f64)], spacing: f64) -> Vec<f64> {
    let raw = || run.iter().map(|&(_, value)| value).collect();
    let (Some(&(first, _)), Some(&(last, _))) = (run.first(), run.last()) else {
        return Vec::new();
    };
    let length = last - first;
    // Too short to say anything about the trend.
    if run.len() < 3 || length <= 0.0 {
        return raw();
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let intervals = ((length / spacing).ceil() as usize).max(1);
    #[allow(clippy::cast_precision_loss)]
    let step = length / intervals as f64;
    let count = intervals + 3;

    // Normal equations (BᵀB + λDᵀD) c = Bᵀy. The matrix is symmetric with
    // three off-diagonals; `matrix[row][k]` holds the entry at (row, row + k).
    let mut matrix = vec![[0.0; 4]; count];
    let mut rhs = vec![0.0; count];
    for &(seconds, value) in run {
        let (first, weights) = basis(seconds, first, step, intervals);
        for (p, &wp) in weights.iter().enumerate() {
            rhs[first + p] += wp * value;
            for (q, &wq) in weights.iter().enumerate().skip(p) {
                matrix[first + p][q - p] += wp * wq;
            }
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let penalty = SPLINE_PENALTY * run.len() as f64 / intervals as f64;
    for row in 0..count - 2 {
        for (p, &dp) in SECOND_DIFFERENCE.iter().enumerate() {
            for (q, &dq) in SECOND_DIFFERENCE.iter().enumerate().skip(p) {
                matrix[row + p][q - p] += penalty * dp * dq;
            }
        }
    }

    let Some(coefficients) = solve_banded(&matrix, &rhs) else {
        return raw();
    };
    run.iter()
        .map(|&(seconds, _)| {
            let (first, weights) = basis(seconds, first, step, intervals);
            weights
                .iter()
                .zip(&coefficients[first..first + 4])
                .map(|(weight, coefficient)| weight * coefficient)
                .sum()
        })
        .collect()
}

/// Solves a symmetric positive-definite system with three off-diagonals by
/// banded Cholesky factorisation; `None` if it isn't positive definite.
fn solve_banded(matrix: &[[f64; 4]], rhs: &[f64]) -> Option<Vec<f64>> {
    let count = rhs.len();
    // `lower[row][k]` holds the factor's entry at (row, row - k).
    let mut lower = vec![[0.0; 4]; count];
    for row in 0..count {
        for k in (1..4).rev().filter(|&k| k <= row) {
            let column = row - k;
            let mut sum = matrix[column][k];
            for j in (k + 1)..4 {
                if j <= row && j - k < 4 {
                    sum -= lower[row][j] * lower[column][j - k];
                }
            }
            lower[row][k] = sum / lower[column][0];
        }
        let diagonal = matrix[row][0] - lower[row][1..].iter().map(|l| l * l).sum::<f64>();
        if diagonal <= 0.0 || !diagonal.is_finite() {
            return None;
        }
        lower[row][0] = diagonal.sqrt();
    }

    let mut solution = rhs.to_vec();
    for row in 0..count {
        for k in (1..4).filter(|&k| k <= row) {
            solution[row] -= lower[row][k] * solution[row - k];
        }
        solution[row] /= lower[row][0];
    }
    for row in (0..count).rev() {
        for k in (1..4).filter(|&k| row + k < count) {
            solution[row] -= lower[row + k][k] * solution[row + k];
        }
        solution[row] /= lower[row][0];
    }
    Some(solution)
}

/// Fits the y axis to `values` with a little headroom.
fn auto_range(metric: Metric, values: impl Iterator<Item = f64>) -> (f64, f64) {
    let (mut low, mut high) = values.fold((f64::MAX, f64::MIN), |(low, high), value| {
        (low.min(value), high.max(value))
    });
    if high - low < metric.min_span() {
        let middle = f64::midpoint(high, low);
        low = middle - metric.min_span() / 2.0;
        high = middle + metric.min_span() / 2.0;
    }
    if metric == Metric::Capacity {
        // Capacity accumulates from zero; anchoring there shows progress.
        low = 0.0;
    }
    let padding = (high - low) * 0.05;
    // None of these measurements go negative, so don't waste space below zero.
    ((low - padding).max(0.0), high + padding)
}

fn format_span(seconds: f64) -> String {
    // Spans are bounded by the six-hour history, well within u64.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let seconds = seconds.round().max(0.0) as u64;
    match seconds {
        0..120 => format!("{seconds} s"),
        120..7200 => format!("{} min", seconds / 60),
        _ => format!("{} h {:02} min", seconds / 3600, (seconds % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mc5000_protocol::{BatteryType, ErrorCode, ModeCode, StatusCode};

    use super::*;

    fn status(code: u8, voltage_mv: u16, capacity_mah: u16) -> SlotStatus {
        SlotStatus {
            leading_byte: 0,
            current_ma: 1000,
            voltage_mv,
            temperature_milli_celsius: 25_000,
            capacity_mah,
            elapsed_seconds: 0,
            resistance_raw: 0,
            status: StatusCode(code),
            mode: ModeCode(0),
            error: ErrorCode(0),
            battery_type: BatteryType(0),
        }
    }

    const PLOT: (f64, f64) = (1000.0, 1000.0);

    fn points(path: &str) -> Vec<(char, f64, f64)> {
        let tokens = path.split_whitespace().collect::<Vec<_>>();
        tokens
            .chunks(3)
            .map(|chunk| {
                (
                    chunk[0].chars().next().unwrap(),
                    chunk[1].parse().unwrap(),
                    chunk[2].parse().unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn empty_and_idle_slots_produce_an_empty_chart() {
        let origin = Instant::now();
        let mut history = History::new(origin);
        history.record(Slot::One, &status(0, 1200, 0), origin);
        let chart = history.chart(
            Metric::Voltage,
            [false; 4],
            ChartView::default(),
            origin,
            PLOT,
        );
        assert!(chart.empty);
        assert!(chart.paths.iter().all(String::is_empty));
    }

    #[test]
    fn active_samples_span_the_plot_with_the_latest_at_now() {
        let origin = Instant::now();
        let mut history = History::new(origin);
        for second in 0..=120u16 {
            history.record(
                Slot::Two,
                &status(1, 1200 + second, second * 10),
                origin + Duration::from_secs(u64::from(second)),
            );
        }
        let chart = history.chart(
            Metric::Voltage,
            [false; 4],
            ChartView::default(),
            origin + Duration::from_secs(120),
            PLOT,
        );
        assert!(!chart.empty);
        assert_eq!(chart.paths[0], "");
        let plotted = points(&chart.paths[1]);
        assert_eq!(plotted.len(), 121);
        assert_eq!(plotted[0].0, 'M');
        assert!(plotted[1..].iter().all(|point| point.0 == 'L'));
        assert!(plotted[0].1 < 1.0 && (plotted[120].1 - 1000.0).abs() < 1.0);
        // Rising voltage: first point low on screen (large y), last point high.
        assert!(plotted[0].2 > 900.0 && plotted[120].2 < 100.0);
        assert_eq!(chart.x_start, "−2 min");
        assert_eq!(chart.y_max, "1.33 V");
    }

    #[test]
    fn hidden_slots_and_metrics_change_the_plot() {
        let origin = Instant::now();
        let mut history = History::new(origin);
        history.record(Slot::One, &status(1, 1200, 100), origin);
        history.record(Slot::Three, &status(1, 4200, 2000), origin);
        let both = history.chart(
            Metric::Voltage,
            [false; 4],
            ChartView::default(),
            origin,
            PLOT,
        );
        assert!(both.y_max.starts_with("4.3"));
        let hidden = history.chart(
            Metric::Voltage,
            [false, false, true, false],
            ChartView::default(),
            origin,
            PLOT,
        );
        assert_eq!(hidden.paths[2], "");
        assert_eq!(
            hidden.y_max, "1.25 V",
            "hidden slots don't stretch the y axis"
        );
        let capacity = history.chart(
            Metric::Capacity,
            [false; 4],
            ChartView::default(),
            origin,
            PLOT,
        );
        assert!(capacity.y_max.ends_with("mAh"));
        assert_eq!(capacity.y_min, "0 mAh");
    }

    #[test]
    fn gaps_break_the_line_and_long_histories_are_decimated() {
        let origin = Instant::now();
        let mut history = History::new(origin);
        for second in (0..10).chain(60..70) {
            history.record(
                Slot::One,
                &status(1, 1200, 0),
                origin + Duration::from_secs(second),
            );
        }
        let chart = history.chart(
            Metric::Voltage,
            [false; 4],
            ChartView::default(),
            origin + Duration::from_secs(70),
            PLOT,
        );
        let moves = points(&chart.paths[0])
            .iter()
            .filter(|point| point.0 == 'M')
            .count();
        assert_eq!(moves, 2);

        history.clear();
        for second in 0..(MAX_SAMPLES as u64 + 100) {
            history.record(
                Slot::One,
                &status(1, 1200, 0),
                origin + Duration::from_secs(second),
            );
        }
        let now = origin + Duration::from_secs(MAX_SAMPLES as u64 + 100);
        let chart = history.chart(Metric::Voltage, [false; 4], ChartView::default(), now, PLOT);
        let plotted = points(&chart.paths[0]);
        assert!(plotted.len() <= MAX_POINTS);
        assert_eq!(
            plotted.iter().filter(|point| point.0 == 'M').count(),
            1,
            "decimation must not be mistaken for gaps"
        );
        assert_eq!(chart.x_start, "−6 h 00 min");
    }

    #[test]
    fn time_window_limits_the_axis_and_y_range() {
        let origin = Instant::now();
        let mut history = History::new(origin);
        for second in 0..=600u16 {
            history.record(
                Slot::One,
                &status(1, 1000 + second, 0),
                origin + Duration::from_secs(u64::from(second)),
            );
        }
        let view = ChartView {
            window: time_window(1),
            y_range: None,
            smoothing: Smoothing::Off,
        };
        let now = origin + Duration::from_secs(600);
        let chart = history.chart(Metric::Voltage, [false; 4], view, now, PLOT);
        assert_eq!(chart.x_start, "−60 s");
        let plotted = points(&chart.paths[0]);
        // 61 samples in the window plus one leading in from the left edge.
        assert_eq!(plotted.len(), 62);
        assert!(plotted[0].1 < 0.0 && plotted[1].1.abs() < 1.0);
        assert!(chart.range.0 > 1.5, "y axis fits only the visible minute");

        let later = now + Duration::from_secs(120);
        assert!(
            history
                .chart(Metric::Voltage, [false; 4], view, later, PLOT)
                .empty
        );
        assert_eq!(time_window(0), None);
        assert_eq!(time_window(99), None);
    }

    #[test]
    fn manual_y_range_zooms_and_pans() {
        let zoomed = zoom_range((0.0, 10.0), 0.5, 0.5);
        assert_eq!(zoomed, (2.5, 7.5));
        // Anchored at the top: the top value stays put.
        assert_eq!(zoom_range((0.0, 10.0), 0.5, 0.0), (5.0, 10.0));
        assert_eq!(pan_range((0.0, 10.0), 0.1), (1.0, 11.0));

        let origin = Instant::now();
        let mut history = History::new(origin);
        history.record(Slot::One, &status(1, 1250, 0), origin);
        let view = ChartView {
            window: None,
            y_range: Some((1.2, 1.3)),
            smoothing: Smoothing::Off,
        };
        let chart = history.chart(Metric::Voltage, [false; 4], view, origin, PLOT);
        assert_eq!(chart.range, (1.2, 1.3));
        assert_eq!(chart.y_mid, "1.25 V");
        assert!((points(&chart.paths[0])[0].2 - 500.0).abs() < 1.0);
        let fine = ChartView {
            window: None,
            y_range: Some((1.245, 1.255)),
            smoothing: Smoothing::Off,
        };
        let chart = history.chart(Metric::Voltage, [false; 4], fine, origin, PLOT);
        assert_eq!(chart.y_max, "1.255 V", "narrow ranges gain decimals");
    }

    #[test]
    fn probe_finds_the_nearest_visible_point() {
        let origin = Instant::now();
        let mut history = History::new(origin);
        for second in 0..=120u16 {
            let at = origin + Duration::from_secs(u64::from(second));
            history.record(Slot::One, &status(1, 1200 + second, 0), at);
            history.record(Slot::Two, &status(1, 1400, 0), at);
        }
        let now = origin + Duration::from_secs(120);
        let view = ChartView::default();
        let chart = history.chart(Metric::Voltage, [false; 4], view, now, PLOT);
        let (_, x, y) = points(&chart.paths[0])[60];
        let probe =
            |hidden, cursor| history.probe(Metric::Voltage, hidden, view, now, PLOT, cursor);

        let hit = probe([false; 4], (x + 3.0, y - 3.0)).unwrap();
        assert_eq!(hit.slot, 0);
        assert_eq!(hit.value, "1.260 V");
        assert!((hit.seconds_ago - 60.0).abs() < 1e-6);
        assert!((hit.x - x).abs() < 0.1 && (hit.y - y).abs() < 0.1);

        assert_eq!(
            probe([false; 4], (x, y + 100.0)),
            None,
            "too far from any line"
        );
        assert_eq!(
            probe([true, false, false, false], (x, y)),
            None,
            "hidden slot"
        );
        let flat = points(&chart.paths[1])[60];
        assert_eq!(probe([false; 4], (flat.1, flat.2)).unwrap().slot, 1);
    }

    #[test]
    fn smoothing_averages_noise_without_bridging_gaps() {
        let averaged = moving_average(&[(0.0, 1.0), (1.0, 3.0), (2.0, 5.0), (20.0, 9.0)], 1.0);
        assert_eq!(averaged, [2.0, 3.0, 4.0, 9.0], "the gap keeps 9.0 out");

        let origin = Instant::now();
        let mut history = History::new(origin);
        // 1.300 V with ±20 mV of alternating noise.
        // Under MAX_POINTS, so no decimation hides the noise.
        for second in 0..500u16 {
            let noise = if second % 2 == 0 { 1320 } else { 1280 };
            history.record(
                Slot::One,
                &status(1, noise, 0),
                origin + Duration::from_secs(u64::from(second)),
            );
        }
        let now = origin + Duration::from_secs(500);
        let raw_view = ChartView::default();
        let smooth_view = ChartView {
            smoothing: Smoothing::Average,
            ..raw_view
        };
        let raw = history.chart(Metric::Voltage, [false; 4], raw_view, now, PLOT);
        let smooth = history.chart(Metric::Voltage, [false; 4], smooth_view, now, PLOT);
        let swing = |chart: &Chart| {
            let ys = points(&chart.paths[0])
                .iter()
                .map(|point| point.2)
                .collect::<Vec<_>>();
            ys.iter().copied().fold(f64::MIN, f64::max)
                - ys.iter().copied().fold(f64::MAX, f64::min)
        };
        // Both use the 0.1 V minimum axis span, so pixel swings are comparable.
        assert!(swing(&raw) > 300.0);
        assert!(
            swing(&smooth) < swing(&raw) / 5.0,
            "smoothed line is nearly flat"
        );

        let (_, x, y) = points(&smooth.paths[0])[300];
        let probe = history
            .probe(Metric::Voltage, [false; 4], smooth_view, now, PLOT, (x, y))
            .unwrap();
        assert!(
            probe.value == "1.320 V" || probe.value == "1.280 V",
            "raw reading"
        );
        let smoothed = probe.smoothed.unwrap();
        assert!(smoothed == "1.300 V" || smoothed == "1.302 V", "{smoothed}");
        let (_, raw_x, raw_y) = points(&raw.paths[0])[300];
        let raw_point = (raw_x, raw_y);
        let probe = history
            .probe(Metric::Voltage, [false; 4], raw_view, now, PLOT, raw_point)
            .unwrap();
        assert_eq!(probe.smoothed, None);
    }

    #[test]
    fn spline_follows_trends_to_the_ends_without_bridging_gaps() {
        // A straight ramp is reproduced exactly, ends included, which a
        // moving average can't do once it runs out of samples on one side.
        let ramp = (0..120)
            .map(|second| (f64::from(second), 3.6 + f64::from(second) * 0.002))
            .collect::<Vec<_>>();
        for (fitted, &(_, value)) in spline_fit(&ramp, 10.0).iter().zip(&ramp) {
            assert!((fitted - value).abs() < 1e-9, "{fitted} vs {value}");
        }

        // Two runs at different levels stay separate.
        let runs = (0..30)
            .map(|second| (f64::from(second), 1.0))
            .chain((60..90).map(|second| (f64::from(second), 5.0)))
            .collect::<Vec<_>>();
        let fitted = spline_fit(&runs, 10.0);
        assert!(fitted[..30].iter().all(|value| (value - 1.0).abs() < 1e-9));
        assert!(fitted[30..].iter().all(|value| (value - 5.0).abs() < 1e-9));

        // Runs too short to fit are left alone.
        assert_eq!(spline_fit(&[(0.0, 2.0), (1.0, 4.0)], 10.0), [2.0, 4.0]);
        assert_eq!(spline_fit(&[], 10.0), Vec::<f64>::new());
    }

    #[test]
    fn spline_smooths_noise_on_a_ramp() {
        let origin = Instant::now();
        let mut history = History::new(origin);
        // Rising 1 mV/s with ±20 mV of alternating noise.
        for second in 0..500u16 {
            let noise = if second % 2 == 0 { 20 } else { -20 };
            let voltage = i32::from(3600 + second) + noise;
            history.record(
                Slot::One,
                &status(1, u16::try_from(voltage).unwrap(), 0),
                origin + Duration::from_secs(u64::from(second)),
            );
        }
        let now = origin + Duration::from_secs(500);
        let view = ChartView {
            smoothing: Smoothing::Spline,
            ..ChartView::default()
        };
        let plot = history
            .plot(Metric::Voltage, [false; 4], view, now)
            .unwrap();
        let worst = plot.series[0]
            .iter()
            .map(|point| (point.value - (3.6 + point.seconds * 0.001)).abs())
            .fold(0.0, f64::max);
        assert!(
            worst < 0.004,
            "fit stays on the trend, ends included: {worst}"
        );

        let chart = history.chart(Metric::Voltage, [false; 4], view, now, PLOT);
        let (_, x, y) = points(&chart.paths[0])[250];
        let probe = history
            .probe(Metric::Voltage, [false; 4], view, now, PLOT, (x, y))
            .unwrap();
        assert!(probe.smoothed.is_some(), "the popup shows the fitted value");
    }

    #[test]
    fn smoothing_modes_map_from_the_ui_index() {
        assert_eq!(Smoothing::from_index(0), Smoothing::Off);
        assert_eq!(Smoothing::from_index(1), Smoothing::Average);
        assert_eq!(Smoothing::from_index(2), Smoothing::Spline);
        assert_eq!(Smoothing::from_index(-1), Smoothing::Off);
    }

    #[test]
    fn cached_plot_is_rebuilt_when_samples_change() {
        let origin = Instant::now();
        let mut history = History::new(origin);
        let now = origin + Duration::from_secs(10);
        let view = ChartView::default();
        history.record(Slot::One, &status(1, 1200, 0), origin);
        let first = history.chart(Metric::Voltage, [false; 4], view, now, PLOT);
        assert_eq!(
            history.chart(Metric::Voltage, [false; 4], view, now, PLOT),
            first
        );
        history.record(
            Slot::One,
            &status(1, 1300, 0),
            origin + Duration::from_secs(1),
        );
        assert_ne!(
            history.chart(Metric::Voltage, [false; 4], view, now, PLOT),
            first,
            "a new sample redraws even at the same time"
        );
        history.clear();
        assert!(
            history
                .chart(Metric::Voltage, [false; 4], view, now, PLOT)
                .empty
        );
    }

    #[test]
    fn csv_lists_every_sample_in_time_order() {
        let origin = Instant::now();
        let mut history = History::new(origin);
        let now = origin + Duration::from_secs(10);
        let wall_now = Local::now();
        assert_eq!(history.to_csv(now, wall_now), None, "nothing captured");

        history.record(
            Slot::Three,
            &status(1, 4100, 12),
            origin + Duration::from_secs(2),
        );
        history.record(
            Slot::One,
            &status(1, 3650, 7),
            origin + Duration::from_secs(2),
        );
        history.record(
            Slot::One,
            &status(1, 3700, 8),
            origin + Duration::from_secs(3),
        );
        // Idle readings aren't captured, so aren't exported either.
        history.record(Slot::Two, &status(0, 1, 0), origin + Duration::from_secs(3));

        let csv = history.to_csv(now, wall_now).unwrap();
        let lines = csv.lines().collect::<Vec<_>>();
        assert_eq!(
            lines[0],
            "timestamp,elapsed_s,slot,voltage_v,current_a,temperature_c,capacity_mah"
        );
        let fields = |line: &str| line.split(',').map(str::to_owned).collect::<Vec<_>>();
        let rows = lines[1..]
            .iter()
            .map(|line| fields(line))
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0][1..], ["0.0", "1", "3.650", "1.000", "25.0", "7"]);
        assert_eq!(rows[1][1..], ["0.0", "3", "4.100", "1.000", "25.0", "12"]);
        assert_eq!(rows[2][1..], ["1.0", "1", "3.700", "1.000", "25.0", "8"]);

        let stamp = |row: &[String]| DateTime::parse_from_rfc3339(&row[0]).unwrap();
        assert_eq!(
            stamp(&rows[2]) - stamp(&rows[0]),
            chrono::Duration::seconds(1)
        );
        let latest = (wall_now - chrono::Duration::seconds(7)).timestamp();
        assert_eq!(stamp(&rows[2]).timestamp(), latest);
    }
}
