use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use serde::Serialize;
use std::sync::Arc;

use crate::model::{BiasKernel, KernelTarget};

const EPS: f64 = 1e-9;
const FREQ_EMPHASIS: f64 = 3000.0;
const NEARNESS_SCALAR: f64 = 10.0;
const NEARNESS_OFFSET: f64 = 0.5;
const THEORETICAL_UPPER: f64 = 0.83;
const FULL_SPEC_MAX_BYTES: usize = 256 * 1024 * 1024;
const PEAK_STABILIZE_QUINT_MAX: f64 = 0.06;
const PEAK_STABILIZE_REL_EPS: f64 = 0.002;
const PEAK_STABILIZE_MAX_SHIFT: usize = 4;

pub struct BiasCfg {
    pub fingerprint_ms: f64,
    pub window_ms: f64,
    pub step_ms: f64,
    pub magic_offset_ms: f64,
    pub kernel_target: KernelTarget,
    pub kernel_type: BiasKernel,
    pub _full_spectrogram: bool,
}

#[derive(Clone, Copy)]
pub struct BiasEstimate {
    pub bias_ms: f64,
    pub confidence: f64,
    pub conv_quint: f64,
    pub conv_stdev: f64,
}

pub struct BiasPlotData {
    pub freq_rows: usize,
    pub digest_rows: usize,
    pub cols: usize,
    pub post_rows: usize,
    pub post_target: KernelTarget,
    pub freq_domain: Vec<f64>,
    pub beat_digest: Vec<f64>,
    pub post_kernel: Vec<f64>,
    pub convolution: Vec<f64>,
    pub times_ms: Vec<f64>,
    pub freqs_khz: Vec<f64>,
    pub beat_indices: Vec<f64>,
    pub bias_ms: f64,
    pub edge_discard: usize,
}

pub struct BiasEstimateWithPlot {
    pub estimate: BiasEstimate,
    pub plot: BiasPlotData,
}

#[derive(Debug, Clone, Serialize)]
pub struct BiasTrace {
    pub setup: BiasTraceSetup,
    pub skip_counts: BiasTraceSkips,
    pub loop_stats: BiasTraceLoop,
    pub beat_head: Vec<BiasTraceBeat>,
    pub beat_tail: Vec<BiasTraceBeat>,
    pub convolution: BiasTraceConv,
    pub result: BiasTraceResult,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct BiasTraceSetup {
    pub sample_rate_hz: u32,
    pub samples: usize,
    pub nperseg: usize,
    pub nstep: usize,
    pub noverlap: usize,
    pub actual_step_sec: f64,
    pub fp_size: usize,
    pub n_freq_taps: usize,
    pub n_time_taps: isize,
    pub spectrogram_offset: f64,
    pub min_sep_sec: f64,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct BiasTraceSkips {
    pub too_early: usize,
    pub too_late: usize,
    pub too_soon: usize,
    pub short_window: usize,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct BiasTraceLoop {
    pub beats_scanned: usize,
    pub beats_used: usize,
    pub first_beat_index: usize,
    pub last_beat_index: usize,
    pub first_beat_time_s: f64,
    pub last_beat_time_s: f64,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct BiasTraceBeat {
    pub beat_index: usize,
    pub beat_time_s: f64,
    pub window_time_idx_raw_start: isize,
    pub window_time_idx_raw_end: isize,
    pub window_time_idx_start: usize,
    pub window_time_idx_end: usize,
    pub digest_min: f64,
    pub digest_max: f64,
    pub digest_mean: f64,
    pub digest_std: f64,
    pub digest_peak_idx: usize,
    pub digest_peak_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BiasTraceConv {
    pub edge_discard: usize,
    pub clip_min: f64,
    pub clip_max: f64,
    pub clip_mean: f64,
    pub clip_std: f64,
    pub top_peaks: Vec<BiasTracePeak>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct BiasTracePeak {
    pub clip_index: usize,
    pub full_index: usize,
    pub time_ms: f64,
    pub value: f64,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct BiasTraceResult {
    pub bias_ms: f64,
    pub confidence: f64,
    pub conv_quint: f64,
    pub conv_stdev: f64,
    pub i_max: usize,
    pub v_median: f64,
    pub v_max: f64,
    pub v20: f64,
    pub v80: f64,
    pub total_max_influence: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct BiasTraceCfg {
    pub keep: usize,
}

#[derive(Clone, Copy)]
struct Setup {
    nperseg: usize,
    nstep: usize,
    fp_size: usize,
    n_freq_taps: usize,
    n_time_taps: isize,
    actual_step_sec: f64,
    spectrogram_offset: f64,
}

#[derive(Clone, Copy)]
struct BeatWindow {
    beat_index: usize,
    beat_time_sec: f64,
    t_s_raw: isize,
    t_f_raw: isize,
    t_s: usize,
    t_f: usize,
}

#[derive(Clone, Default)]
struct BeatWindows {
    windows: Vec<BeatWindow>,
    skips: BiasTraceSkips,
    beats_scanned: usize,
}

#[derive(Clone, Default)]
struct TraceCollector {
    keep: usize,
    beat_head: Vec<BiasTraceBeat>,
    beat_tail: Vec<BiasTraceBeat>,
}

enum BiasResult {
    Estimate(BiasEstimate),
    WithTrace(BiasEstimate, BiasTrace),
    WithPlot(BiasEstimateWithPlot),
}

impl BiasResult {
    fn into_estimate(self) -> BiasEstimate {
        match self {
            Self::Estimate(v) | Self::WithTrace(v, _) => v,
            Self::WithPlot(v) => v.estimate,
        }
    }

    fn into_pair(self) -> Result<(BiasEstimate, BiasTrace), String> {
        match self {
            Self::WithTrace(v, t) => Ok((v, t)),
            Self::Estimate(_) | Self::WithPlot(_) => {
                Err("bias trace requested but not produced".to_string())
            }
        }
    }

    fn into_plot(self) -> Result<BiasEstimateWithPlot, String> {
        match self {
            Self::WithPlot(v) => Ok(v),
            Self::Estimate(_) | Self::WithTrace(_, _) => {
                Err("bias plot requested but not produced".to_string())
            }
        }
    }
}

struct ConvStats {
    estimate: BiasEstimate,
    convolution: BiasTraceConv,
    result: BiasTraceResult,
}

#[derive(Default)]
pub struct BiasRuntime {
    spectrogram: Vec<SpectrogramCacheEntry>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct SpectrogramKey {
    sample_rate_hz: u32,
    nperseg: usize,
    nstep: usize,
}

struct SpectrogramCacheEntry {
    key: SpectrogramKey,
    ctx: SpectrogramCtx,
}

impl TraceCollector {
    fn new(cfg: BiasTraceCfg) -> Self {
        Self {
            keep: cfg.keep.max(1),
            beat_head: Vec::new(),
            beat_tail: Vec::new(),
        }
    }

    fn push_beat(&mut self, beat: BiasTraceBeat) {
        if self.beat_head.len() < self.keep {
            self.beat_head.push(beat);
        }
        self.beat_tail.push(beat);
        if self.beat_tail.len() > self.keep {
            self.beat_tail.remove(0);
        }
    }
}

impl BeatWindows {
    fn loop_stats(&self) -> BiasTraceLoop {
        let Some(first) = self.windows.first() else {
            return BiasTraceLoop {
                beats_scanned: self.beats_scanned,
                beats_used: 0,
                first_beat_index: 0,
                last_beat_index: 0,
                first_beat_time_s: 0.0,
                last_beat_time_s: 0.0,
            };
        };
        let last = self.windows.last().unwrap_or(first);
        BiasTraceLoop {
            beats_scanned: self.beats_scanned,
            beats_used: self.windows.len(),
            first_beat_index: first.beat_index,
            last_beat_index: last.beat_index,
            first_beat_time_s: first.beat_time_sec,
            last_beat_time_s: last.beat_time_sec,
        }
    }
}

impl BiasRuntime {
    fn spectrogram_ctx(&mut self, key: SpectrogramKey) -> &mut SpectrogramCtx {
        if let Some(i) = self.spectrogram.iter().position(|entry| entry.key == key) {
            return &mut self.spectrogram[i].ctx;
        }
        self.spectrogram.push(SpectrogramCacheEntry {
            key,
            ctx: SpectrogramCtx::new(key.nperseg),
        });
        let last = self.spectrogram.len() - 1;
        &mut self.spectrogram[last].ctx
    }
}

#[allow(dead_code)]
pub fn estimate_bias(
    audio_mono: &[f32],
    sample_rate_hz: u32,
    chart: &rssp::ChartSummary,
    cfg: &BiasCfg,
) -> Result<BiasEstimate, String> {
    let mut runtime = BiasRuntime::default();
    estimate_bias_reuse(audio_mono, sample_rate_hz, chart, cfg, &mut runtime)
}

pub fn estimate_bias_reuse(
    audio_mono: &[f32],
    sample_rate_hz: u32,
    chart: &rssp::ChartSummary,
    cfg: &BiasCfg,
    runtime: &mut BiasRuntime,
) -> Result<BiasEstimate, String> {
    let setup = build_setup(audio_mono.len(), sample_rate_hz, cfg)?;
    let timing = rssp::timing::timing_data_from_segments(
        chart.chart_offset_seconds,
        0.0,
        &chart.timing_segments,
    );
    estimate_bias_with_timing_setup(
        audio_mono,
        sample_rate_hz,
        &timing,
        cfg,
        setup,
        runtime,
        None,
        false,
    )
    .map(BiasResult::into_estimate)
}

pub fn estimate_bias_reuse_with_plot(
    audio_mono: &[f32],
    sample_rate_hz: u32,
    chart: &rssp::ChartSummary,
    cfg: &BiasCfg,
    runtime: &mut BiasRuntime,
) -> Result<BiasEstimateWithPlot, String> {
    let setup = build_setup(audio_mono.len(), sample_rate_hz, cfg)?;
    let timing = rssp::timing::timing_data_from_segments(
        chart.chart_offset_seconds,
        0.0,
        &chart.timing_segments,
    );
    estimate_bias_with_timing_setup(
        audio_mono,
        sample_rate_hz,
        &timing,
        cfg,
        setup,
        runtime,
        None,
        true,
    )
    .and_then(BiasResult::into_plot)
}

pub fn estimate_bias_reuse_with_trace(
    audio_mono: &[f32],
    sample_rate_hz: u32,
    chart: &rssp::ChartSummary,
    cfg: &BiasCfg,
    runtime: &mut BiasRuntime,
    trace_cfg: BiasTraceCfg,
) -> Result<(BiasEstimate, BiasTrace), String> {
    let setup = build_setup(audio_mono.len(), sample_rate_hz, cfg)?;
    let timing = rssp::timing::timing_data_from_segments(
        chart.chart_offset_seconds,
        0.0,
        &chart.timing_segments,
    );
    estimate_bias_with_timing_setup(
        audio_mono,
        sample_rate_hz,
        &timing,
        cfg,
        setup,
        runtime,
        Some(trace_cfg),
        false,
    )
    .and_then(BiasResult::into_pair)
}

#[allow(dead_code)]
pub fn estimate_bias_with_timing(
    audio_mono: &[f32],
    sample_rate_hz: u32,
    timing: &rssp::timing::TimingData,
    cfg: &BiasCfg,
) -> Result<BiasEstimate, String> {
    let mut runtime = BiasRuntime::default();
    estimate_bias_with_timing_reuse(audio_mono, sample_rate_hz, timing, cfg, &mut runtime)
}

#[allow(dead_code)]
pub fn estimate_bias_with_timing_reuse(
    audio_mono: &[f32],
    sample_rate_hz: u32,
    timing: &rssp::timing::TimingData,
    cfg: &BiasCfg,
    runtime: &mut BiasRuntime,
) -> Result<BiasEstimate, String> {
    let setup = build_setup(audio_mono.len(), sample_rate_hz, cfg)?;
    estimate_bias_with_timing_setup(
        audio_mono,
        sample_rate_hz,
        timing,
        cfg,
        setup,
        runtime,
        None,
        false,
    )
    .map(BiasResult::into_estimate)
}

#[allow(dead_code)]
pub fn estimate_bias_with_beat_fn<F>(
    audio_mono: &[f32],
    sample_rate_hz: u32,
    cfg: &BiasCfg,
    beat_time_fn: F,
) -> Result<BiasEstimate, String>
where
    F: FnMut(usize) -> f64,
{
    let mut runtime = BiasRuntime::default();
    estimate_bias_with_beat_fn_reuse(audio_mono, sample_rate_hz, cfg, &mut runtime, beat_time_fn)
}

pub fn estimate_bias_with_beat_fn_reuse<F>(
    audio_mono: &[f32],
    sample_rate_hz: u32,
    cfg: &BiasCfg,
    runtime: &mut BiasRuntime,
    beat_time_fn: F,
) -> Result<BiasEstimate, String>
where
    F: FnMut(usize) -> f64,
{
    let setup = build_setup(audio_mono.len(), sample_rate_hz, cfg)?;
    estimate_bias_with_setup(
        audio_mono,
        sample_rate_hz,
        cfg,
        setup,
        runtime,
        None,
        false,
        beat_time_fn,
    )
    .map(BiasResult::into_estimate)
}

pub fn estimate_bias_with_beat_fn_trace_reuse<F>(
    audio_mono: &[f32],
    sample_rate_hz: u32,
    cfg: &BiasCfg,
    runtime: &mut BiasRuntime,
    trace_cfg: BiasTraceCfg,
    beat_time_fn: F,
) -> Result<(BiasEstimate, BiasTrace), String>
where
    F: FnMut(usize) -> f64,
{
    let setup = build_setup(audio_mono.len(), sample_rate_hz, cfg)?;
    estimate_bias_with_setup(
        audio_mono,
        sample_rate_hz,
        cfg,
        setup,
        runtime,
        Some(trace_cfg),
        false,
        beat_time_fn,
    )
    .and_then(BiasResult::into_pair)
}

fn estimate_bias_with_timing_setup(
    audio_mono: &[f32],
    sample_rate_hz: u32,
    timing: &rssp::timing::TimingData,
    cfg: &BiasCfg,
    setup: Setup,
    runtime: &mut BiasRuntime,
    trace_cfg: Option<BiasTraceCfg>,
    want_plot: bool,
) -> Result<BiasResult, String> {
    estimate_bias_with_setup(
        audio_mono,
        sample_rate_hz,
        cfg,
        setup,
        runtime,
        trace_cfg,
        want_plot,
        |beat| rssp::timing::get_time_for_beat(timing, beat as f64),
    )
}

fn estimate_bias_with_setup<F>(
    audio_mono: &[f32],
    sample_rate_hz: u32,
    cfg: &BiasCfg,
    setup: Setup,
    runtime: &mut BiasRuntime,
    trace_cfg: Option<BiasTraceCfg>,
    want_plot: bool,
    beat_time_fn: F,
) -> Result<BiasResult, String>
where
    F: FnMut(usize) -> f64,
{
    let window_stats =
        beat_windows_from_fn(audio_mono.len(), sample_rate_hz, cfg, setup, beat_time_fn);
    if window_stats.windows.is_empty() {
        return Err("no beat windows produced for bias calculation".to_string());
    }
    let key = SpectrogramKey {
        sample_rate_hz,
        nperseg: setup.nperseg,
        nstep: setup.nstep,
    };
    let sp = runtime.spectrogram_ctx(key);
    let mut collector = trace_cfg.map(TraceCollector::new);
    let use_full_spectrogram = cfg._full_spectrogram
        && should_use_full_spectrogram(audio_mono.len(), setup, window_stats.windows.len());
    let (acc, digest, beats) = build_fingerprints(
        audio_mono,
        sample_rate_hz,
        setup,
        use_full_spectrogram,
        &window_stats.windows,
        sp,
        collector.as_mut(),
    )?;
    let (rows, cols) = if cfg.kernel_target == KernelTarget::Accumulator {
        (setup.n_freq_taps, setup.fp_size)
    } else {
        (beats, setup.fp_size)
    };
    let kernel = make_kernel(cfg.kernel_type);
    let post = if cfg.kernel_target == KernelTarget::Accumulator {
        convolve_wrap_5x5(&acc, rows, cols, &kernel)
    } else {
        convolve_wrap_5x5(&digest, rows, cols, &kernel)
    };
    let flat = flatten_columns_sum(&post, rows, cols);
    let conv_stats = estimate_from_convolution(&flat, setup.actual_step_sec, cfg.magic_offset_ms)?;
    let bias_ms = conv_stats.estimate.bias_ms;
    let edge_discard = conv_stats.convolution.edge_discard;
    if let Some(collector) = collector {
        let setup_trace = BiasTraceSetup {
            sample_rate_hz,
            samples: audio_mono.len(),
            nperseg: setup.nperseg,
            nstep: setup.nstep,
            noverlap: setup.nperseg.saturating_sub(setup.nstep),
            actual_step_sec: setup.actual_step_sec,
            fp_size: setup.fp_size,
            n_freq_taps: setup.n_freq_taps,
            n_time_taps: setup.n_time_taps,
            spectrogram_offset: setup.spectrogram_offset,
            min_sep_sec: cfg.fingerprint_ms * 1e-3,
        };
        let loop_stats = window_stats.loop_stats();
        let trace = BiasTrace {
            setup: setup_trace,
            skip_counts: window_stats.skips,
            loop_stats,
            beat_head: collector.beat_head,
            beat_tail: collector.beat_tail,
            convolution: conv_stats.convolution,
            result: conv_stats.result,
        };
        Ok(BiasResult::WithTrace(conv_stats.estimate, trace))
    } else if want_plot {
        Ok(BiasResult::WithPlot(BiasEstimateWithPlot {
            estimate: conv_stats.estimate,
            plot: build_plot_data(
                sample_rate_hz,
                setup,
                cfg.kernel_target,
                acc,
                digest,
                beats,
                post,
                flat,
                bias_ms,
                edge_discard,
            ),
        }))
    } else {
        Ok(BiasResult::Estimate(conv_stats.estimate))
    }
}

fn build_setup(audio_len: usize, sample_rate_hz: u32, cfg: &BiasCfg) -> Result<Setup, String> {
    if sample_rate_hz == 0 {
        return Err("sample rate is zero".to_string());
    }
    let nperseg = (f64::from(sample_rate_hz) * cfg.window_ms * 1e-3).floor() as usize;
    let nstep = ((f64::from(sample_rate_hz) * cfg.step_ms * 1e-3).floor() as usize).max(1);
    if nperseg < 8 {
        return Err("spectrogram window is too small".to_string());
    }
    if audio_len <= nperseg {
        return Err("audio too short for spectrogram window".to_string());
    }
    let actual_step_sec = nstep as f64 / f64::from(sample_rate_hz);
    let fp_size = 2 * (py_round(cfg.fingerprint_ms * 1e-3 / actual_step_sec) as usize);
    if fp_size < 8 {
        return Err("fingerprint size is too small".to_string());
    }
    let n_freq_taps = 1 + nperseg / 2;
    let n_time_taps = ((audio_len - nperseg) as f64 / nstep as f64).ceil() as isize;
    let window_size = nperseg as f64 / nstep as f64;
    let spectrogram_offset = (0.5_f64).sqrt() * window_size;
    Ok(Setup {
        nperseg,
        nstep,
        fp_size,
        n_freq_taps,
        n_time_taps,
        actual_step_sec,
        spectrogram_offset,
    })
}

fn beat_windows_from_fn<F>(
    audio_len: usize,
    sample_rate_hz: u32,
    cfg: &BiasCfg,
    setup: Setup,
    mut beat_time_fn: F,
) -> BeatWindows
where
    F: FnMut(usize) -> f64,
{
    let audio_duration_sec =
        ((audio_len as f64 / f64::from(sample_rate_hz)) * 1000.0).floor() * 1e-3;
    let min_sep_sec = cfg.fingerprint_ms * 1e-3;
    let mut out = BeatWindows::default();
    let mut t_last = f64::NEG_INFINITY;
    let mut beat = 0usize;
    while beat < 200_000 {
        let beat_i = beat;
        let t = beat_time_fn(beat_i);
        beat += 1;
        out.beats_scanned = beat;
        if !t.is_finite() {
            break;
        }
        if t < 0.0 {
            out.skips.too_early += 1;
            continue;
        }
        if t > audio_duration_sec {
            out.skips.too_late += 1;
            break;
        }
        if t - t_last < min_sep_sec {
            out.skips.too_soon += 1;
            continue;
        }
        t_last = t;
        if let Some(w) = beat_time_to_window_taps(beat_i, t, setup) {
            out.windows.push(w);
        } else {
            out.skips.short_window += 1;
        }
    }
    out
}

fn beat_time_to_window_taps(beat_index: usize, beat_time: f64, setup: Setup) -> Option<BeatWindow> {
    let half = setup.fp_size as f64 * 0.5;
    let t_s = py_round(beat_time / setup.actual_step_sec - setup.spectrogram_offset - half);
    let t_f = py_round(beat_time / setup.actual_step_sec - setup.spectrogram_offset + half);
    let start = t_s.max(0);
    let end = t_f.min(setup.n_time_taps);
    if end - start != setup.fp_size as isize {
        None
    } else {
        Some(BeatWindow {
            beat_index,
            beat_time_sec: beat_time,
            t_s_raw: t_s,
            t_f_raw: t_f,
            t_s: start as usize,
            t_f: end as usize,
        })
    }
}

struct SpectrogramCtx {
    fft: Arc<dyn Fft<f64>>,
    window: Vec<f64>,
    buf: Vec<Complex<f64>>,
    out: Vec<f64>,
}

impl SpectrogramCtx {
    fn new(nperseg: usize) -> Self {
        let mut planner = FftPlanner::<f64>::new();
        let fft = planner.plan_fft_forward(nperseg);
        Self {
            fft,
            window: hann_periodic(nperseg),
            buf: vec![Complex::new(0.0, 0.0); nperseg],
            out: Vec::new(),
        }
    }
}

fn build_fingerprints(
    audio_mono: &[f32],
    sample_rate_hz: u32,
    setup: Setup,
    use_full_spectrogram: bool,
    windows: &[BeatWindow],
    sp: &mut SpectrogramCtx,
    trace: Option<&mut TraceCollector>,
) -> Result<(Vec<f64>, Vec<f64>, usize), String> {
    if use_full_spectrogram {
        build_fingerprints_full(audio_mono, sample_rate_hz, setup, windows, sp, trace)
    } else {
        build_fingerprints_legacy(audio_mono, sample_rate_hz, setup, windows, sp, trace)
    }
}

fn should_use_full_spectrogram(audio_len: usize, setup: Setup, windows: usize) -> bool {
    if audio_len < setup.nperseg {
        return false;
    }
    let full_cols = 1 + (audio_len - setup.nperseg) / setup.nstep;
    let legacy_cols = windows.saturating_mul(setup.fp_size);
    if full_cols > legacy_cols {
        return false;
    }
    let Some(full_cells) = setup.n_freq_taps.checked_mul(full_cols) else {
        return false;
    };
    let Some(full_bytes) = full_cells.checked_mul(std::mem::size_of::<f64>()) else {
        return false;
    };
    full_bytes <= FULL_SPEC_MAX_BYTES
}

fn build_fingerprints_full(
    audio_mono: &[f32],
    sample_rate_hz: u32,
    setup: Setup,
    windows: &[BeatWindow],
    sp: &mut SpectrogramCtx,
    mut trace: Option<&mut TraceCollector>,
) -> Result<(Vec<f64>, Vec<f64>, usize), String> {
    let rows = setup.n_freq_taps;
    let cols = setup.fp_size;
    let Some(full_cols) =
        spectrogram_log_full_into(audio_mono, setup.nperseg, setup.nstep, rows, sp)
    else {
        return Err("audio too short for full spectrogram".to_string());
    };
    let mut acc = vec![0.0; rows * cols];
    let mut digest = Vec::with_capacity(windows.len() * cols);
    let weights = frequency_weights(sample_rate_hz, setup.nperseg, rows);
    let mut digest_row = vec![0.0; cols];
    let mut beats = 0usize;
    let fp_times_ms = fingerprint_times_ms(cols, setup.actual_step_sec);
    for w in windows {
        if !window_matches_legacy_rules(audio_mono.len(), setup, w.t_s, w.t_f, cols, full_cols) {
            continue;
        }
        digest_row.fill(0.0);
        apply_window_weighting(
            &sp.out,
            &weights,
            rows,
            cols,
            full_cols,
            w.t_s,
            &mut acc,
            &mut digest_row,
        );
        if let Some(rec) = trace.as_mut() {
            rec.push_beat(beat_trace(w, &digest_row, &fp_times_ms));
        }
        digest.extend_from_slice(&digest_row);
        beats += 1;
    }
    if beats == 0 {
        Err("no valid beat snippets for spectrogram digest".to_string())
    } else {
        Ok((acc, digest, beats))
    }
}

fn build_fingerprints_legacy(
    audio_mono: &[f32],
    sample_rate_hz: u32,
    setup: Setup,
    windows: &[BeatWindow],
    sp: &mut SpectrogramCtx,
    mut trace: Option<&mut TraceCollector>,
) -> Result<(Vec<f64>, Vec<f64>, usize), String> {
    let rows = setup.n_freq_taps;
    let cols = setup.fp_size;
    let mut acc = vec![0.0; rows * cols];
    let mut digest = Vec::with_capacity(windows.len() * cols);
    let weights = frequency_weights(sample_rate_hz, setup.nperseg, rows);
    let mut digest_row = vec![0.0; cols];
    let mut beats = 0usize;
    let fp_times_ms = fingerprint_times_ms(cols, setup.actual_step_sec);
    for w in windows {
        let sample_s = w.t_s.saturating_mul(setup.nstep);
        let sample_f = (w.t_f.saturating_mul(setup.nstep) + setup.nperseg.saturating_sub(1))
            .min(audio_mono.len());
        if sample_f <= sample_s + setup.nperseg {
            continue;
        }
        if !spectrogram_log_into(
            &audio_mono[sample_s..sample_f],
            setup.nperseg,
            setup.nstep,
            rows,
            cols,
            sp,
        ) {
            continue;
        }
        digest_row.fill(0.0);
        apply_window_weighting(
            &sp.out,
            &weights,
            rows,
            cols,
            cols,
            0,
            &mut acc,
            &mut digest_row,
        );
        if let Some(rec) = trace.as_mut() {
            rec.push_beat(beat_trace(w, &digest_row, &fp_times_ms));
        }
        digest.extend_from_slice(&digest_row);
        beats += 1;
    }
    if beats == 0 {
        Err("no valid beat snippets for spectrogram digest".to_string())
    } else {
        Ok((acc, digest, beats))
    }
}

fn spectrogram_log_full_into(
    samples: &[f32],
    nperseg: usize,
    nstep: usize,
    rows: usize,
    sp: &mut SpectrogramCtx,
) -> Option<usize> {
    if samples.len() < nperseg {
        return None;
    }
    let cols = 1 + (samples.len() - nperseg) / nstep;
    if sp.out.len() != rows * cols {
        sp.out.resize(rows * cols, 0.0);
    }
    for c in 0..cols {
        let base = c * nstep;
        for i in 0..nperseg {
            sp.buf[i].re = f64::from(samples[base + i]) * sp.window[i];
            sp.buf[i].im = 0.0;
        }
        sp.fft.process(&mut sp.buf);
        for r in 0..rows {
            let v = sp.buf[r];
            sp.out[r * cols + c] = (v.re.mul_add(v.re, v.im * v.im) + EPS).log2();
        }
    }
    Some(cols)
}

fn spectrogram_log_into(
    samples: &[f32],
    nperseg: usize,
    nstep: usize,
    rows: usize,
    expected_cols: usize,
    sp: &mut SpectrogramCtx,
) -> bool {
    if samples.len() < nperseg {
        return false;
    }
    let cols = 1 + (samples.len() - nperseg) / nstep;
    if cols != expected_cols {
        return false;
    }
    if sp.out.len() != rows * cols {
        sp.out.resize(rows * cols, 0.0);
    }
    for c in 0..cols {
        let base = c * nstep;
        for i in 0..nperseg {
            sp.buf[i].re = f64::from(samples[base + i]) * sp.window[i];
            sp.buf[i].im = 0.0;
        }
        sp.fft.process(&mut sp.buf);
        for r in 0..rows {
            let v = sp.buf[r];
            sp.out[r * cols + c] = (v.re.mul_add(v.re, v.im * v.im) + EPS).log2();
        }
    }
    true
}

fn window_matches_legacy_rules(
    audio_len: usize,
    setup: Setup,
    t_s: usize,
    t_f: usize,
    cols: usize,
    full_cols: usize,
) -> bool {
    let sample_s = t_s.saturating_mul(setup.nstep);
    let sample_f =
        (t_f.saturating_mul(setup.nstep) + setup.nperseg.saturating_sub(1)).min(audio_len);
    if sample_f <= sample_s + setup.nperseg {
        return false;
    }
    let local_cols = 1 + (sample_f - sample_s - setup.nperseg) / setup.nstep;
    local_cols == cols && t_s.checked_add(cols).is_some_and(|end| end <= full_cols)
}

fn apply_window_weighting(
    full_log_spec: &[f64],
    weights: &[f64],
    rows: usize,
    cols: usize,
    full_cols: usize,
    start_col: usize,
    acc: &mut [f64],
    digest_row: &mut [f64],
) {
    for r in 0..rows {
        let w = weights[r];
        let src_off = r * full_cols + start_col;
        let dst_off = r * cols;
        for c in 0..cols {
            let v = full_log_spec[src_off + c] * w;
            acc[dst_off + c] += v;
            digest_row[c] += v;
        }
    }
}

fn beat_trace(window: &BeatWindow, digest_row: &[f64], fp_times_ms: &[f64]) -> BiasTraceBeat {
    let peak_idx = argmax(digest_row);
    let digest_mean = digest_row.iter().sum::<f64>() / digest_row.len() as f64;
    let digest_var = digest_row
        .iter()
        .map(|v| {
            let d = *v - digest_mean;
            d * d
        })
        .sum::<f64>()
        / digest_row.len() as f64;
    BiasTraceBeat {
        beat_index: window.beat_index,
        beat_time_s: window.beat_time_sec,
        window_time_idx_raw_start: window.t_s_raw,
        window_time_idx_raw_end: window.t_f_raw,
        window_time_idx_start: window.t_s,
        window_time_idx_end: window.t_f,
        digest_min: digest_row.iter().copied().fold(f64::INFINITY, f64::min),
        digest_max: digest_row.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        digest_mean,
        digest_std: digest_var.sqrt(),
        digest_peak_idx: peak_idx,
        digest_peak_ms: fp_times_ms.get(peak_idx).copied().unwrap_or(0.0),
    }
}

fn frequency_weights(sample_rate_hz: u32, nperseg: usize, rows: usize) -> Vec<f64> {
    let step = f64::from(sample_rate_hz) / nperseg as f64;
    (0..rows)
        .map(|r| {
            let f = r as f64 * step;
            f * (-f / FREQ_EMPHASIS).exp()
        })
        .collect()
}

fn hann_periodic(n: usize) -> Vec<f64> {
    let n_f = n as f64;
    (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n_f).cos())
        .collect()
}

fn make_kernel(kind: BiasKernel) -> [f64; 25] {
    let row = if kind == BiasKernel::Loudest {
        [1.0, 3.0, 10.0, 3.0, 1.0]
    } else {
        [1.0, 1.0, 0.0, -1.0, -1.0]
    };
    let mut k = [0.0; 25];
    for r in 0..5 {
        for c in 0..5 {
            k[r * 5 + c] = row[c];
        }
    }
    k
}

fn convolve_wrap_5x5(input: &[f64], rows: usize, cols: usize, kernel: &[f64; 25]) -> Vec<f64> {
    let mut out = vec![0.0; rows * cols];
    for r in 0..rows {
        for c in 0..cols {
            let mut sum = 0.0;
            for kr in 0..5 {
                for kc in 0..5 {
                    // scipy.signal.convolve2d uses true convolution (kernel flipped),
                    // not correlation.
                    let rr = wrap_idx(r as isize - kr as isize + 2, rows);
                    let cc = wrap_idx(c as isize - kc as isize + 2, cols);
                    sum += input[rr * cols + cc] * kernel[kr * 5 + kc];
                }
            }
            out[r * cols + c] = sum;
        }
    }
    out
}

fn wrap_idx(i: isize, len: usize) -> usize {
    i.rem_euclid(len as isize) as usize
}

fn flatten_columns_sum(matrix: &[f64], rows: usize, cols: usize) -> Vec<f64> {
    let mut out = vec![0.0; cols];
    for r in 0..rows {
        let row_off = r * cols;
        for c in 0..cols {
            out[c] += matrix[row_off + c];
        }
    }
    out
}

fn estimate_from_convolution(
    post_kernel_flat: &[f64],
    actual_step_sec: f64,
    magic_offset_ms: f64,
) -> Result<ConvStats, String> {
    let edge_discard = 2usize;
    if post_kernel_flat.len() <= edge_discard * 2 {
        return Err("convolution output too small for edge discard".to_string());
    }
    let clip = &post_kernel_flat[edge_discard..post_kernel_flat.len() - edge_discard];
    let v_clip = normalize_0_1(clip);
    let v20 = percentile(&v_clip, 20.0);
    let v80 = percentile(&v_clip, 80.0);
    let conv_quint = v80 - v20;
    let i_max = stabilized_peak_index(clip, argmax(clip), conv_quint);
    let times_ms = fingerprint_times_ms(post_kernel_flat.len(), actual_step_sec);
    let bias_ms = times_ms[i_max + edge_discard] + magic_offset_ms;
    let t_clip = &times_ms[edge_discard..post_kernel_flat.len() - edge_discard];
    let v_std = stdev_population(&v_clip);
    let v_median = percentile(&v_clip, 50.0);
    let v_max = v_clip[i_max];
    let t_peak = t_clip[i_max];

    let mut total = 0.0;
    for i in 0..v_clip.len() {
        let rival = rivaling_strength(v_clip[i], v_median, v_max);
        let close = ((t_clip[i] - t_peak).abs() - NEARNESS_OFFSET).max(0.0) / NEARNESS_SCALAR;
        total += rival.powi(4) * close.powf(1.5);
    }
    let total_max_influence = total / v_clip.len() as f64;
    let confidence =
        (((1.0 - total_max_influence.powf(0.2)) / THEORETICAL_UPPER).min(1.0)).clamp(0.0, 1.0);
    let estimate = BiasEstimate {
        bias_ms,
        confidence,
        conv_quint,
        conv_stdev: v_std,
    };
    let mut peak_idx = (0..clip.len()).collect::<Vec<_>>();
    peak_idx.sort_by(|a, b| clip[*b].total_cmp(&clip[*a]));
    peak_idx.truncate(12);
    let convolution = BiasTraceConv {
        edge_discard,
        clip_min: clip.iter().copied().fold(f64::INFINITY, f64::min),
        clip_max: clip.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        clip_mean: clip.iter().sum::<f64>() / clip.len() as f64,
        clip_std: stdev_population(clip),
        top_peaks: peak_idx
            .into_iter()
            .map(|i| BiasTracePeak {
                clip_index: i,
                full_index: i + edge_discard,
                time_ms: times_ms[i + edge_discard],
                value: clip[i],
            })
            .collect(),
    };
    let result = BiasTraceResult {
        bias_ms,
        confidence,
        conv_quint: estimate.conv_quint,
        conv_stdev: estimate.conv_stdev,
        i_max,
        v_median,
        v_max,
        v20,
        v80,
        total_max_influence,
    };
    Ok(ConvStats {
        estimate,
        convolution,
        result,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_plot_data(
    sample_rate_hz: u32,
    setup: Setup,
    post_target: KernelTarget,
    freq_domain: Vec<f64>,
    beat_digest: Vec<f64>,
    beats: usize,
    post_kernel: Vec<f64>,
    convolution: Vec<f64>,
    bias_ms: f64,
    edge_discard: usize,
) -> BiasPlotData {
    let hz_step = f64::from(sample_rate_hz) / setup.nperseg as f64;
    let freqs_khz = (0..setup.n_freq_taps)
        .map(|i| i as f64 * hz_step * 1e-3)
        .collect::<Vec<_>>();
    let beat_indices = (0..beats).map(|i| i as f64).collect::<Vec<_>>();
    let post_rows = if post_target == KernelTarget::Accumulator {
        setup.n_freq_taps
    } else {
        beats
    };
    BiasPlotData {
        freq_rows: setup.n_freq_taps,
        digest_rows: beats,
        cols: setup.fp_size,
        post_rows,
        post_target,
        freq_domain,
        beat_digest,
        post_kernel,
        convolution,
        times_ms: fingerprint_times_ms(setup.fp_size, setup.actual_step_sec),
        freqs_khz,
        beat_indices,
        bias_ms,
        edge_discard,
    }
}

fn fingerprint_times_ms(cols: usize, actual_step_sec: f64) -> Vec<f64> {
    let half = (cols / 2) as isize;
    let step_ms = actual_step_sec * 1e3;
    (0..cols)
        .map(|i| (i as isize - half) as f64 * step_ms)
        .collect()
}

#[inline]
fn py_round(v: f64) -> isize {
    v.round_ties_even() as isize
}

fn argmax(values: &[f64]) -> usize {
    let mut idx = 0usize;
    let mut best = f64::NEG_INFINITY;
    for (i, v) in values.iter().enumerate() {
        if *v > best {
            best = *v;
            idx = i;
        }
    }
    idx
}

fn stabilized_peak_index(values: &[f64], i_max: usize, conv_quint: f64) -> usize {
    if conv_quint > PEAK_STABILIZE_QUINT_MAX {
        return i_max;
    }
    let peak = values[i_max];
    if !peak.is_finite() {
        return i_max;
    }
    let floor = peak * (1.0 - PEAK_STABILIZE_REL_EPS);
    let mut pick = i_max;
    let end = values
        .len()
        .min(i_max.saturating_add(PEAK_STABILIZE_MAX_SHIFT + 1));
    for (j, v) in values.iter().enumerate().take(end).skip(i_max + 1) {
        if *v >= floor {
            pick = j;
        }
    }
    pick
}

fn normalize_0_1(values: &[f64]) -> Vec<f64> {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for v in values {
        min = min.min(*v);
        max = max.max(*v);
    }
    let range = max - min;
    if range.abs() < 1e-12 {
        vec![0.0; values.len()]
    } else {
        values.iter().map(|v| (v - min) / range).collect()
    }
}

fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut s = values.to_vec();
    s.sort_by(f64::total_cmp);
    if s.len() == 1 {
        return s[0];
    }
    let rank = (p / 100.0) * (s.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        s[lo]
    } else {
        let frac = rank - lo as f64;
        s[lo] * (1.0 - frac) + s[hi] * frac
    }
}

fn stdev_population(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let var = values
        .iter()
        .map(|v| {
            let d = *v - mean;
            d * d
        })
        .sum::<f64>()
        / values.len() as f64;
    var.sqrt()
}

fn rivaling_strength(value: f64, median: f64, vmax: f64) -> f64 {
    let denom = vmax - median;
    if denom.abs() < 1e-12 {
        0.0
    } else {
        ((value - median) / denom).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BeatWindow, BiasCfg, BiasRuntime, Setup, SpectrogramCtx, build_fingerprints_full,
        build_fingerprints_legacy, estimate_bias_with_beat_fn, estimate_bias_with_beat_fn_reuse,
        make_kernel, percentile,
    };
    use crate::model::{BiasKernel, KernelTarget};

    fn bias_cfg() -> BiasCfg {
        BiasCfg {
            fingerprint_ms: 50.0,
            window_ms: 10.0,
            step_ms: 0.2,
            magic_offset_ms: 0.0,
            kernel_target: KernelTarget::Digest,
            kernel_type: BiasKernel::Rising,
            _full_spectrogram: false,
        }
    }

    #[test]
    fn percentile_linear_interp_matches_expected() {
        let v = [0.0, 10.0, 20.0, 30.0];
        assert!((percentile(&v, 50.0) - 15.0).abs() < 1e-12);
        assert!((percentile(&v, 20.0) - 6.0).abs() < 1e-12);
        assert!((percentile(&v, 80.0) - 24.0).abs() < 1e-12);
    }

    #[test]
    fn kernels_have_expected_center_column_polarity() {
        let rising = make_kernel(BiasKernel::Rising);
        let loud = make_kernel(BiasKernel::Loudest);
        assert_eq!(rising[2], 0.0);
        assert!(rising[0] > rising[4]);
        assert_eq!(loud[2], 10.0);
    }

    #[test]
    fn fft_runtime_reuse_matches_one_shot() {
        let sample_rate = 4_000u32;
        let len = (sample_rate as usize) * 4;
        let mut audio = Vec::with_capacity(len);
        for i in 0..len {
            let t = i as f32 * 0.01;
            audio.push((t.sin() * (t * 0.1).cos()) * 0.8);
        }
        let cfg = bias_cfg();
        let one_shot =
            estimate_bias_with_beat_fn(&audio, sample_rate, &cfg, |beat| beat as f64 * 0.25)
                .expect("one-shot estimate should succeed");
        let mut runtime = BiasRuntime::default();
        let first =
            estimate_bias_with_beat_fn_reuse(&audio, sample_rate, &cfg, &mut runtime, |beat| {
                beat as f64 * 0.25
            })
            .expect("first cached estimate should succeed");
        let second =
            estimate_bias_with_beat_fn_reuse(&audio, sample_rate, &cfg, &mut runtime, |beat| {
                beat as f64 * 0.25
            })
            .expect("second cached estimate should succeed");
        assert!((one_shot.bias_ms - first.bias_ms).abs() < 1e-12);
        assert!((one_shot.confidence - first.confidence).abs() < 1e-12);
        assert!((one_shot.conv_quint - first.conv_quint).abs() < 1e-12);
        assert!((one_shot.conv_stdev - first.conv_stdev).abs() < 1e-12);
        assert!((first.bias_ms - second.bias_ms).abs() < 1e-12);
        assert!((first.confidence - second.confidence).abs() < 1e-12);
        assert!((first.conv_quint - second.conv_quint).abs() < 1e-12);
        assert!((first.conv_stdev - second.conv_stdev).abs() < 1e-12);
    }

    #[test]
    fn full_spectrogram_matches_legacy_fingerprints() {
        let sample_rate = 8_000u32;
        let mut audio = Vec::with_capacity(400);
        for i in 0..400 {
            let t = i as f32 * 0.07;
            audio.push((t.sin() * 0.8) + (t * 0.5).cos() * 0.2);
        }
        let setup = Setup {
            nperseg: 32,
            nstep: 8,
            fp_size: 10,
            n_freq_taps: 17,
            n_time_taps: 0,
            actual_step_sec: 0.0,
            spectrogram_offset: 0.0,
        };
        let windows = vec![
            BeatWindow {
                beat_index: 0,
                beat_time_sec: 0.0,
                t_s_raw: 0,
                t_f_raw: 10,
                t_s: 0,
                t_f: 10,
            },
            BeatWindow {
                beat_index: 5,
                beat_time_sec: 0.0,
                t_s_raw: 5,
                t_f_raw: 15,
                t_s: 5,
                t_f: 15,
            },
            BeatWindow {
                beat_index: 9,
                beat_time_sec: 0.0,
                t_s_raw: 9,
                t_f_raw: 19,
                t_s: 9,
                t_f: 19,
            },
            BeatWindow {
                beat_index: 20,
                beat_time_sec: 0.0,
                t_s_raw: 20,
                t_f_raw: 30,
                t_s: 20,
                t_f: 30,
            },
        ];
        let mut sp_full = SpectrogramCtx::new(setup.nperseg);
        let mut sp_legacy = SpectrogramCtx::new(setup.nperseg);
        let full =
            build_fingerprints_full(&audio, sample_rate, setup, &windows, &mut sp_full, None)
                .expect("full fingerprint build should succeed");
        let legacy =
            build_fingerprints_legacy(&audio, sample_rate, setup, &windows, &mut sp_legacy, None)
                .expect("legacy fingerprint build should succeed");
        assert_eq!(full.2, legacy.2);
        assert_eq!(full.0.len(), legacy.0.len());
        assert_eq!(full.1.len(), legacy.1.len());
        for (a, b) in full.0.iter().zip(legacy.0.iter()) {
            assert!((*a - *b).abs() < 1e-12);
        }
        for (a, b) in full.1.iter().zip(legacy.1.iter()) {
            assert!((*a - *b).abs() < 1e-12);
        }
    }
}
