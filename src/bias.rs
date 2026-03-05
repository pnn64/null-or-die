use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;

use crate::model::{BiasKernel, KernelTarget};

const EPS: f64 = 1e-9;
const FREQ_EMPHASIS: f64 = 3000.0;
const NEARNESS_SCALAR: f64 = 10.0;
const NEARNESS_OFFSET: f64 = 0.5;
const THEORETICAL_UPPER: f64 = 0.83;

pub struct BiasCfg {
    pub fingerprint_ms: f64,
    pub window_ms: f64,
    pub step_ms: f64,
    pub magic_offset_ms: f64,
    pub kernel_target: KernelTarget,
    pub kernel_type: BiasKernel,
    pub _full_spectrogram: bool,
}

pub struct BiasEstimate {
    pub bias_ms: f64,
    pub confidence: f64,
    pub conv_quint: f64,
    pub conv_stdev: f64,
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
    estimate_bias_with_timing_setup(audio_mono, sample_rate_hz, &timing, cfg, setup, runtime)
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
    estimate_bias_with_timing_setup(audio_mono, sample_rate_hz, timing, cfg, setup, runtime)
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
    estimate_bias_with_setup(audio_mono, sample_rate_hz, cfg, setup, runtime, beat_time_fn)
}

fn estimate_bias_with_timing_setup(
    audio_mono: &[f32],
    sample_rate_hz: u32,
    timing: &rssp::timing::TimingData,
    cfg: &BiasCfg,
    setup: Setup,
    runtime: &mut BiasRuntime,
) -> Result<BiasEstimate, String> {
    estimate_bias_with_setup(
        audio_mono,
        sample_rate_hz,
        cfg,
        setup,
        runtime,
        |beat| rssp::timing::get_time_for_beat(timing, beat as f64),
    )
}

fn estimate_bias_with_setup<F>(
    audio_mono: &[f32],
    sample_rate_hz: u32,
    cfg: &BiasCfg,
    setup: Setup,
    runtime: &mut BiasRuntime,
    beat_time_fn: F,
) -> Result<BiasEstimate, String>
where
    F: FnMut(usize) -> f64,
{
    let windows = beat_windows_from_fn(audio_mono.len(), sample_rate_hz, cfg, setup, beat_time_fn);
    if windows.is_empty() {
        return Err("no beat windows produced for bias calculation".to_string());
    }
    let key = SpectrogramKey {
        sample_rate_hz,
        nperseg: setup.nperseg,
        nstep: setup.nstep,
    };
    let sp = runtime.spectrogram_ctx(key);
    let (acc, digest, beats) = build_fingerprints(audio_mono, sample_rate_hz, setup, &windows, sp)?;
    let (rows, cols, target) = if cfg.kernel_target == KernelTarget::Accumulator {
        (setup.n_freq_taps, setup.fp_size, acc)
    } else {
        (beats, setup.fp_size, digest)
    };
    let kernel = make_kernel(cfg.kernel_type);
    let post = convolve_wrap_5x5(&target, rows, cols, &kernel);
    let flat = flatten_columns_sum(&post, rows, cols);
    estimate_from_convolution(&flat, setup.actual_step_sec, cfg.magic_offset_ms)
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
) -> Vec<(usize, usize)>
where
    F: FnMut(usize) -> f64,
{
    let audio_duration_sec = audio_len as f64 / f64::from(sample_rate_hz);
    let min_sep_sec = cfg.fingerprint_ms * 1e-3;
    let mut windows = Vec::new();
    let mut t_last = f64::NEG_INFINITY;
    let mut beat = 0usize;
    while beat < 200_000 {
        let t = beat_time_fn(beat);
        beat += 1;
        if !t.is_finite() {
            break;
        }
        if t < 0.0 {
            continue;
        }
        if t > audio_duration_sec {
            break;
        }
        if t - t_last < min_sep_sec {
            continue;
        }
        t_last = t;
        if let Some(w) = beat_time_to_window_taps(t, setup) {
            windows.push(w);
        }
    }
    windows
}

fn beat_time_to_window_taps(beat_time: f64, setup: Setup) -> Option<(usize, usize)> {
    let half = setup.fp_size as f64 * 0.5;
    let t_s = py_round(beat_time / setup.actual_step_sec - setup.spectrogram_offset - half);
    let t_f = py_round(beat_time / setup.actual_step_sec - setup.spectrogram_offset + half);
    let start = t_s.max(0);
    let end = t_f.min(setup.n_time_taps);
    if end - start != setup.fp_size as isize {
        None
    } else {
        Some((start as usize, end as usize))
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
    windows: &[(usize, usize)],
    sp: &mut SpectrogramCtx,
) -> Result<(Vec<f64>, Vec<f64>, usize), String> {
    let rows = setup.n_freq_taps;
    let cols = setup.fp_size;
    let mut acc = vec![0.0; rows * cols];
    let mut digest = Vec::with_capacity(windows.len() * cols);
    let weights = frequency_weights(sample_rate_hz, setup.nperseg, rows);
    let mut digest_row = vec![0.0; cols];
    let mut beats = 0usize;
    for (t_s, t_f) in windows {
        let sample_s = t_s.saturating_mul(setup.nstep);
        let sample_f = (t_f.saturating_mul(setup.nstep) + setup.nperseg.saturating_sub(1))
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
        apply_frequency_weighting(&sp.out, &weights, rows, cols, &mut acc, &mut digest_row);
        digest.extend_from_slice(&digest_row);
        beats += 1;
    }
    if beats == 0 {
        Err("no valid beat snippets for spectrogram digest".to_string())
    } else {
        Ok((acc, digest, beats))
    }
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

fn apply_frequency_weighting(
    log_spec: &[f64],
    weights: &[f64],
    rows: usize,
    cols: usize,
    acc: &mut [f64],
    digest_row: &mut [f64],
) {
    for r in 0..rows {
        let w = weights[r];
        let row_off = r * cols;
        for c in 0..cols {
            let v = log_spec[row_off + c] * w;
            acc[row_off + c] += v;
            digest_row[c] += v;
        }
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
) -> Result<BiasEstimate, String> {
    let edge_discard = 2usize;
    if post_kernel_flat.len() <= edge_discard * 2 {
        return Err("convolution output too small for edge discard".to_string());
    }
    let clip = &post_kernel_flat[edge_discard..post_kernel_flat.len() - edge_discard];
    let i_max = argmax(clip);
    let times_ms = fingerprint_times_ms(post_kernel_flat.len(), actual_step_sec);
    let bias_ms = times_ms[i_max + edge_discard] + magic_offset_ms;

    let v_clip = normalize_0_1(clip);
    let t_clip = &times_ms[edge_discard..post_kernel_flat.len() - edge_discard];
    let v_std = stdev_population(&v_clip);
    let v_median = percentile(&v_clip, 50.0);
    let v20 = percentile(&v_clip, 20.0);
    let v80 = percentile(&v_clip, 80.0);
    let v_max = v_clip[i_max];
    let t_peak = t_clip[i_max];

    let mut total = 0.0;
    for i in 0..v_clip.len() {
        let rival = rivaling_strength(v_clip[i], v_median, v_max);
        let close = ((t_clip[i] - t_peak).abs() - NEARNESS_OFFSET).max(0.0) / NEARNESS_SCALAR;
        total += rival.powi(4) * close.powf(1.5);
    }
    let total = total / v_clip.len() as f64;
    let confidence = (((1.0 - total.powf(0.2)) / THEORETICAL_UPPER).min(1.0)).clamp(0.0, 1.0);
    Ok(BiasEstimate {
        bias_ms,
        confidence,
        conv_quint: v80 - v20,
        conv_stdev: v_std,
    })
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
        BiasCfg, BiasRuntime, estimate_bias_with_beat_fn, estimate_bias_with_beat_fn_reuse,
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
        let first = estimate_bias_with_beat_fn_reuse(
            &audio,
            sample_rate,
            &cfg,
            &mut runtime,
            |beat| beat as f64 * 0.25,
        )
        .expect("first cached estimate should succeed");
        let second = estimate_bias_with_beat_fn_reuse(
            &audio,
            sample_rate,
            &cfg,
            &mut runtime,
            |beat| beat as f64 * 0.25,
        )
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
}
