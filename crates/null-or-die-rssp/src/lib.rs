use std::fs;
use std::path::{Path, PathBuf};

use rssp::{AnalysisOptions, analyze};

use null_or_die_audio::{OggDecode, decode_ogg_mono_like_python};
use null_or_die_core::{
    BiasCfg, BiasEstimate, BiasEstimateWithPlot, BiasKernel, BiasPlotData, BiasRuntime,
    BiasStreamCfg, BiasStreamEvent, BiasTrace, BiasTraceCfg, KernelTarget,
    estimate_bias_with_beat_fn_plot_reuse, estimate_bias_with_beat_fn_reuse,
    estimate_bias_with_beat_fn_stream_reuse, estimate_bias_with_beat_fn_trace_reuse,
};

#[derive(Debug, Clone)]
pub struct SyncChartMeta {
    pub simfile_path: String,
    pub chart_index: usize,
    pub title: String,
    pub subtitle: String,
    pub artist: String,
    pub step_type: String,
    pub difficulty: String,
    pub description: String,
    pub music_tag: String,
    pub music_path: String,
}

#[derive(Debug, Clone)]
pub struct SyncChartResult {
    pub meta: SyncChartMeta,
    pub estimate: BiasEstimate,
    pub plot: BiasPlotData,
}

pub fn default_bias_cfg() -> BiasCfg {
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
    let timing = timing_for_chart(chart);
    estimate_bias_with_timing_reuse(audio_mono, sample_rate_hz, &timing, cfg, runtime)
}

pub fn estimate_bias_reuse_with_plot(
    audio_mono: &[f32],
    sample_rate_hz: u32,
    chart: &rssp::ChartSummary,
    cfg: &BiasCfg,
    runtime: &mut BiasRuntime,
) -> Result<BiasEstimateWithPlot, String> {
    let timing = timing_for_chart(chart);
    estimate_bias_with_beat_fn_plot_reuse(audio_mono, sample_rate_hz, cfg, runtime, |beat| {
        rssp::timing::get_time_for_beat(&timing, beat as f64)
    })
}

pub fn estimate_bias_reuse_with_stream<F>(
    audio_mono: &[f32],
    sample_rate_hz: u32,
    chart: &rssp::ChartSummary,
    cfg: &BiasCfg,
    runtime: &mut BiasRuntime,
    stream_cfg: BiasStreamCfg,
    on_event: F,
) -> Result<BiasEstimateWithPlot, String>
where
    F: FnMut(BiasStreamEvent),
{
    let timing = timing_for_chart(chart);
    estimate_bias_with_beat_fn_stream_reuse(
        audio_mono,
        sample_rate_hz,
        cfg,
        runtime,
        stream_cfg,
        on_event,
        |beat| rssp::timing::get_time_for_beat(&timing, beat as f64),
    )
}

pub fn estimate_bias_reuse_with_trace(
    audio_mono: &[f32],
    sample_rate_hz: u32,
    chart: &rssp::ChartSummary,
    cfg: &BiasCfg,
    runtime: &mut BiasRuntime,
    trace_cfg: BiasTraceCfg,
) -> Result<(BiasEstimate, BiasTrace), String> {
    let timing = timing_for_chart(chart);
    estimate_bias_with_beat_fn_trace_reuse(
        audio_mono,
        sample_rate_hz,
        cfg,
        runtime,
        trace_cfg,
        |beat| rssp::timing::get_time_for_beat(&timing, beat as f64),
    )
}

pub fn estimate_bias_with_timing(
    audio_mono: &[f32],
    sample_rate_hz: u32,
    timing: &rssp::timing::TimingData,
    cfg: &BiasCfg,
) -> Result<BiasEstimate, String> {
    let mut runtime = BiasRuntime::default();
    estimate_bias_with_timing_reuse(audio_mono, sample_rate_hz, timing, cfg, &mut runtime)
}

pub fn estimate_bias_with_timing_reuse(
    audio_mono: &[f32],
    sample_rate_hz: u32,
    timing: &rssp::timing::TimingData,
    cfg: &BiasCfg,
    runtime: &mut BiasRuntime,
) -> Result<BiasEstimate, String> {
    estimate_bias_with_beat_fn_reuse(audio_mono, sample_rate_hz, cfg, runtime, |beat| {
        rssp::timing::get_time_for_beat(timing, beat as f64)
    })
}

pub fn inspect_simfile(simfile_path: &Path) -> Result<Vec<SyncChartMeta>, String> {
    let summary = read_summary(simfile_path)?;
    let song_dir = simfile_path.parent().ok_or_else(|| {
        format!(
            "simfile has no parent directory: {}",
            simfile_path.display()
        )
    })?;
    let music_tag = summary.music_path.trim().to_string();
    let music_path = resolve_music_path(song_dir, &music_tag)?;
    let out = summary
        .charts
        .iter()
        .enumerate()
        .map(|(chart_index, chart)| SyncChartMeta {
            simfile_path: simfile_path.display().to_string(),
            chart_index,
            title: summary.title_str.clone(),
            subtitle: summary.subtitle_str.clone(),
            artist: summary.artist_str.clone(),
            step_type: chart.step_type_str.clone(),
            difficulty: chart.difficulty_str.clone(),
            description: chart.description_str.clone(),
            music_tag: music_tag.clone(),
            music_path: music_path.display().to_string(),
        })
        .collect::<Vec<_>>();
    Ok(out)
}

fn timing_for_chart(chart: &rssp::ChartSummary) -> rssp::timing::TimingData {
    rssp::timing::timing_data_from_segments(chart.chart_offset_seconds, 0.0, &chart.timing_segments)
}

pub fn analyze_chart(
    simfile_path: &Path,
    chart_index: usize,
    cfg: &BiasCfg,
) -> Result<SyncChartResult, String> {
    let mut runtime = BiasRuntime::default();
    analyze_chart_with_runtime(simfile_path, chart_index, cfg, &mut runtime)
}

pub fn analyze_chart_stream<F>(
    simfile_path: &Path,
    chart_index: usize,
    cfg: &BiasCfg,
    stream_cfg: BiasStreamCfg,
    mut on_event: F,
) -> Result<SyncChartResult, String>
where
    F: FnMut(BiasStreamEvent),
{
    let summary = read_summary(simfile_path)?;
    let chart = summary
        .charts
        .get(chart_index)
        .ok_or_else(|| format!("chart index out of range: {chart_index}"))?;
    let (music_tag, music_path, decoded) = load_audio_for_summary(simfile_path, &summary)?;
    let mut runtime = BiasRuntime::default();
    let est_plot = estimate_bias_reuse_with_stream(
        &decoded.mono,
        decoded.sample_rate_hz,
        chart,
        cfg,
        &mut runtime,
        stream_cfg,
        &mut on_event,
    )?;
    Ok(chart_result(
        simfile_path,
        &summary,
        chart,
        chart_index,
        &music_tag,
        &music_path,
        est_plot,
    ))
}

pub fn analyze_chart_with_runtime(
    simfile_path: &Path,
    chart_index: usize,
    cfg: &BiasCfg,
    runtime: &mut BiasRuntime,
) -> Result<SyncChartResult, String> {
    let summary = read_summary(simfile_path)?;
    let chart = summary
        .charts
        .get(chart_index)
        .ok_or_else(|| format!("chart index out of range: {chart_index}"))?;
    let (music_tag, music_path, decoded) = load_audio_for_summary(simfile_path, &summary)?;
    let est_plot =
        estimate_bias_reuse_with_plot(&decoded.mono, decoded.sample_rate_hz, chart, cfg, runtime)?;
    Ok(chart_result(
        simfile_path,
        &summary,
        chart,
        chart_index,
        &music_tag,
        &music_path,
        est_plot,
    ))
}

fn chart_result(
    simfile_path: &Path,
    summary: &rssp::SimfileSummary,
    chart: &rssp::ChartSummary,
    chart_index: usize,
    music_tag: &str,
    music_path: &Path,
    est_plot: BiasEstimateWithPlot,
) -> SyncChartResult {
    SyncChartResult {
        meta: SyncChartMeta {
            simfile_path: simfile_path.display().to_string(),
            chart_index,
            title: summary.title_str.clone(),
            subtitle: summary.subtitle_str.clone(),
            artist: summary.artist_str.clone(),
            step_type: chart.step_type_str.clone(),
            difficulty: chart.difficulty_str.clone(),
            description: chart.description_str.clone(),
            music_tag: music_tag.to_string(),
            music_path: music_path.display().to_string(),
        },
        estimate: est_plot.estimate,
        plot: est_plot.plot,
    }
}

fn read_summary(simfile_path: &Path) -> Result<rssp::SimfileSummary, String> {
    if !simfile_path.is_file() {
        return Err(format!(
            "simfile path is not a file: {}",
            simfile_path.display()
        ));
    }
    let bytes = fs::read(simfile_path)
        .map_err(|e| format!("read {} failed: {e}", simfile_path.display()))?;
    let ext = simfile_ext(simfile_path);
    analyze(&bytes, &ext, &AnalysisOptions::default())
        .map_err(|e| format!("rssp analyze failed: {e}"))
}

fn load_audio_for_summary(
    simfile_path: &Path,
    summary: &rssp::SimfileSummary,
) -> Result<(String, PathBuf, OggDecode), String> {
    let song_dir = simfile_path.parent().ok_or_else(|| {
        format!(
            "simfile has no parent directory: {}",
            simfile_path.display()
        )
    })?;
    let music_tag = summary.music_path.trim().to_string();
    let music_path = resolve_music_path(song_dir, &music_tag)?;
    let decoded = decode_ogg_mono_like_python(&music_path)?;
    Ok((music_tag, music_path, decoded))
}

fn resolve_music_path(song_dir: &Path, music_tag: &str) -> Result<PathBuf, String> {
    if music_tag.trim().is_empty() {
        return Err("simfile has empty #MUSIC tag".to_string());
    }
    let Some(path) = rssp::assets::resolve_music_path_like_itg(song_dir, music_tag) else {
        return Err(format!("could not resolve #MUSIC {:?}", music_tag));
    };
    if !path
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("ogg"))
    {
        return Err(format!("unsupported audio format {}", path.display()));
    }
    Ok(path)
}

fn simfile_ext(path: &Path) -> String {
    path.extension()
        .and_then(|s| s.to_str())
        .map_or_else(String::new, |s| s.to_ascii_lowercase())
}
