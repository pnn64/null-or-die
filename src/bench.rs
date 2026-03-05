use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use rssp::{AnalysisOptions, analyze};
use serde::Serialize;

use crate::audio::decode_ogg_mono_like_python;
use crate::bias::{BiasCfg, BiasRuntime, estimate_bias_reuse};
use crate::cli::BenchCmd;
use crate::model::{BiasKernel, KernelTarget};

pub fn run(args: &BenchCmd) -> Result<BenchReport, String> {
    if args.iterations == 0 {
        return Err("iterations must be >= 1".to_string());
    }
    if !args.simfile_path.is_file() {
        return Err(format!(
            "simfile path is not a file: {}",
            args.simfile_path.display()
        ));
    }
    let cfg = parse_bias_cfg(args)?;
    for _ in 0..args.warmup {
        run_once(&args.simfile_path, &cfg)?;
    }
    let mut runs = Vec::with_capacity(args.iterations);
    for _ in 0..args.iterations {
        runs.push(run_once(&args.simfile_path, &cfg)?);
    }
    let last = runs
        .last()
        .ok_or_else(|| "internal bench error: no runs recorded".to_string())?;
    let timings = summarize_runs(&runs);
    Ok(BenchReport {
        tool: "rnon".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        mode: "bench-analyze".to_string(),
        simfile_path: args.simfile_path.display().to_string(),
        iterations: args.iterations,
        warmup: args.warmup,
        chart_count: last.chart_count,
        decoded_audio_files: last.decoded_audio_files,
        timings,
    })
}

fn run_once(path: &Path, cfg: &BiasCfg) -> Result<RunSample, String> {
    let total_start = Instant::now();
    let read_start = Instant::now();
    let simfile_bytes =
        fs::read(path).map_err(|e| format!("read simfile {} failed: {e}", path.display()))?;
    let read_simfile = read_start.elapsed();

    let analyze_start = Instant::now();
    let ext = simfile_ext(path);
    let summary = analyze(&simfile_bytes, &ext, &AnalysisOptions::default())
        .map_err(|e| format!("rssp analyze failed: {e}"))?;
    let analyze_parse = analyze_start.elapsed();

    let Some(song_dir) = path.parent() else {
        return Err(format!(
            "simfile has no parent directory: {}",
            path.display()
        ));
    };
    let music_tag = summary.music_path.trim();
    if music_tag.is_empty() {
        return Err("simfile has empty #MUSIC tag".to_string());
    }
    let Some(audio_path) = rssp::assets::resolve_music_path_like_itg(song_dir, music_tag) else {
        return Err(format!("could not resolve #MUSIC {:?}", music_tag));
    };
    if !is_ogg_path(&audio_path) {
        return Err(format!("unsupported audio format {}", audio_path.display()));
    }

    let decode_start = Instant::now();
    let decode = decode_ogg_mono_like_python(&audio_path)
        .map_err(|e| format!("audio decode failed for {}: {e}", audio_path.display()))?;
    let decode_audio = decode_start.elapsed();

    let bias_start = Instant::now();
    let mut bias_rt = BiasRuntime::default();
    for chart in &summary.charts {
        estimate_bias_reuse(&decode.mono, decode.sample_rate_hz, chart, cfg, &mut bias_rt)
            .map_err(|e| format!("bias estimation failed: {e}"))?;
    }
    let bias_estimation = bias_start.elapsed();
    let total = total_start.elapsed();
    Ok(RunSample {
        read_simfile,
        analyze_parse,
        decode_audio,
        bias_estimation,
        total,
        chart_count: summary.charts.len(),
        decoded_audio_files: 1,
    })
}

fn parse_bias_cfg(args: &BenchCmd) -> Result<BiasCfg, String> {
    Ok(BiasCfg {
        fingerprint_ms: args.fingerprint_ms,
        window_ms: args.window_ms,
        step_ms: args.step_ms,
        magic_offset_ms: args.magic_offset_ms,
        kernel_target: parse_kernel_target(&args.kernel_target)?,
        kernel_type: parse_kernel_type(&args.kernel_type)?,
        _full_spectrogram: args.full_spectrogram,
    })
}

fn parse_kernel_target(raw: &str) -> Result<KernelTarget, String> {
    match raw.to_ascii_lowercase().as_str() {
        "0" | "digest" => Ok(KernelTarget::Digest),
        "1" | "acc" | "accumulator" => Ok(KernelTarget::Accumulator),
        _ => Err(format!("invalid kernel target: {raw}")),
    }
}

fn parse_kernel_type(raw: &str) -> Result<BiasKernel, String> {
    match raw.to_ascii_lowercase().as_str() {
        "0" | "rising" => Ok(BiasKernel::Rising),
        "1" | "loudest" => Ok(BiasKernel::Loudest),
        _ => Err(format!("invalid kernel type: {raw}")),
    }
}

fn summarize_runs(runs: &[RunSample]) -> TimingSummary {
    TimingSummary {
        read_simfile_ms: summarize_phase(runs, |r| r.read_simfile),
        analyze_parse_ms: summarize_phase(runs, |r| r.analyze_parse),
        decode_audio_ms: summarize_phase(runs, |r| r.decode_audio),
        bias_estimation_ms: summarize_phase(runs, |r| r.bias_estimation),
        total_ms: summarize_phase(runs, |r| r.total),
    }
}

fn summarize_phase<F>(runs: &[RunSample], pick: F) -> PhaseStats
where
    F: Fn(&RunSample) -> Duration,
{
    let mut min = f64::INFINITY;
    let mut max = 0.0_f64;
    let mut sum = 0.0;
    for run in runs {
        let ms = pick(run).as_secs_f64() * 1000.0;
        min = min.min(ms);
        max = max.max(ms);
        sum += ms;
    }
    let avg = if runs.is_empty() {
        0.0
    } else {
        sum / runs.len() as f64
    };
    PhaseStats {
        avg_ms: avg,
        min_ms: if min.is_finite() { min } else { 0.0 },
        max_ms: max,
    }
}

fn simfile_ext(path: &Path) -> String {
    path.extension()
        .and_then(|s| s.to_str())
        .map_or_else(String::new, |s| s.to_ascii_lowercase())
}

fn is_ogg_path(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("ogg"))
}

#[derive(Clone, Copy)]
struct RunSample {
    read_simfile: Duration,
    analyze_parse: Duration,
    decode_audio: Duration,
    bias_estimation: Duration,
    total: Duration,
    chart_count: usize,
    decoded_audio_files: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchReport {
    pub tool: String,
    pub version: String,
    pub mode: String,
    pub simfile_path: String,
    pub iterations: usize,
    pub warmup: usize,
    pub chart_count: usize,
    pub decoded_audio_files: usize,
    pub timings: TimingSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimingSummary {
    pub read_simfile_ms: PhaseStats,
    pub analyze_parse_ms: PhaseStats,
    pub decode_audio_ms: PhaseStats,
    pub bias_estimation_ms: PhaseStats,
    pub total_ms: PhaseStats,
}

#[derive(Debug, Clone, Serialize)]
pub struct PhaseStats {
    pub avg_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
}
