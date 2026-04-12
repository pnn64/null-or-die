use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use rssp::{AnalysisOptions, analyze};
use serde::Serialize;

use crate::audio::{OggDecode, decode_ogg_mono_like_python};
use crate::bias::{
    BiasCfg, BiasRuntime, BiasTrace, BiasTraceCfg, estimate_bias_reuse,
    estimate_bias_reuse_with_plot, estimate_bias_reuse_with_trace,
};
use crate::cli::AnalyzeCmd;
use crate::compat::guess_paradigm;
use crate::compat::slot_abbreviation;
use crate::fs_scan::{discover_simfiles, md5_hex, rel_path};
use crate::model::{
    AnalyzeParams, AnalyzeReport, BiasKernel, ChartScan, KernelTarget, SimfileScan,
};
use crate::plot::write_nine_or_null_plots;

pub fn run(args: &AnalyzeCmd) -> Result<AnalyzeReport, String> {
    let report_path = resolve_report_path(&args.root_path, args.report_path.as_deref())?;
    fs::create_dir_all(&report_path)
        .map_err(|e| format!("create report dir {} failed: {e}", report_path.display()))?;
    let params = build_params(args, &report_path)?;
    let bias_cfg = bias_cfg_from_params(&params);
    let trace_ctl = TraceCtl::from_env();
    let simfiles = discover_simfiles(&args.root_path)?;
    let scanned = simfiles
        .iter()
        .map(|path| {
            scan_one(
                path,
                &args.root_path,
                &report_path,
                args.plot,
                &params,
                &bias_cfg,
                &trace_ctl,
            )
        })
        .collect::<Vec<_>>();
    Ok(AnalyzeReport {
        tool: crate::TOOL_NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        mode: "scan".to_string(),
        params,
        simfile_count: scanned.len(),
        simfiles: scanned,
    })
}

struct TraceCtl {
    enabled: bool,
    keep: usize,
    tokens: Vec<String>,
    dump_dir: Option<PathBuf>,
}

impl TraceCtl {
    fn from_env() -> Self {
        let enabled = env_bool("NOD_BIAS_TRACE");
        let keep = env::var("NOD_BIAS_TRACE_KEEP")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .map(|v| v.max(1))
            .unwrap_or(24);
        let tokens = env::var("NOD_BIAS_TRACE_FILTER")
            .ok()
            .map(|raw| {
                raw.split([',', ';'])
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_ascii_lowercase())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let dump_dir = env::var("NOD_BIAS_TRACE_DIR")
            .ok()
            .map(|v| PathBuf::from(v.trim()))
            .filter(|p| !p.as_os_str().is_empty());
        Self {
            enabled,
            keep,
            tokens,
            dump_dir,
        }
    }

    fn matches(&self, simfile_path: &Path, chart: &rssp::ChartSummary, chart_index: usize) -> bool {
        if !self.enabled {
            return false;
        }
        if self.tokens.is_empty() {
            return true;
        }
        let hay = format!(
            "{}|{}|{}|{}|{}",
            simfile_path.display(),
            chart_index,
            chart.step_type_str,
            chart.difficulty_str,
            chart.description_str
        )
        .to_ascii_lowercase();
        self.tokens.iter().any(|t| hay.contains(t))
    }
}

fn env_bool(name: &str) -> bool {
    env::var(name).ok().is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn build_params(args: &AnalyzeCmd, report_path: &Path) -> Result<AnalyzeParams, String> {
    let to_paradigm = validate_paradigm(args.to_paradigm.as_deref())?;
    Ok(AnalyzeParams {
        root_path: args.root_path.display().to_string(),
        report_path: report_path.display().to_string(),
        consider_null: args.consider_null,
        consider_p9ms: args.consider_p9ms,
        tolerance: args.tolerance,
        confidence_limit: args.confidence_limit,
        fingerprint_ms: args.fingerprint_ms,
        window_ms: args.window_ms,
        step_ms: args.step_ms,
        magic_offset_ms: args.magic_offset_ms,
        kernel_target: parse_kernel_target(&args.kernel_target)?,
        kernel_type: parse_kernel_type(&args.kernel_type)?,
        full_spectrogram: args.full_spectrogram,
        to_paradigm,
    })
}

fn validate_paradigm(raw: Option<&str>) -> Result<Option<String>, String> {
    let Some(value) = raw else {
        return Ok(None);
    };
    if value == "null" || value == "+9ms" {
        Ok(Some(value.to_string()))
    } else {
        Err(format!("invalid paradigm: {value}"))
    }
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

fn resolve_report_path(root: &Path, explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    if root.is_file() {
        let parent = root
            .parent()
            .ok_or_else(|| format!("cannot resolve parent dir for {}", root.display()))?;
        Ok(parent.join("__bias-check"))
    } else {
        Ok(root.join("__bias-check"))
    }
}

fn scan_one(
    path: &Path,
    root: &Path,
    report_path: &Path,
    plot_enabled: bool,
    params: &AnalyzeParams,
    bias_cfg: &BiasCfg,
    trace_ctl: &TraceCtl,
) -> SimfileScan {
    let rel = rel_path(root, path);
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => return read_error(path, &rel, format!("read failed: {err}")),
    };
    let ext = simfile_ext(path);
    let digest = md5_hex(&bytes);
    let options = AnalysisOptions::default();
    match analyze(&bytes, &ext, &options) {
        Ok(summary) => {
            let mut charts = charts_from_summary(&summary.charts);
            let chart_music = chart_music_tags(&summary.charts, &summary.music_path);
            assign_chart_music(&mut charts, &chart_music);
            apply_bias_estimates(
                path,
                &summary.charts,
                &mut charts,
                &chart_music,
                &summary.title_str,
                &summary.subtitle_str,
                report_path,
                plot_enabled,
                params,
                bias_cfg,
                trace_ctl,
            );
            SimfileScan {
                simfile_path: path.display().to_string(),
                simfile_rel: rel,
                simfile_md5: digest,
                extension: ext,
                status: "scanned".to_string(),
                error: None,
                title: Some(summary.title_str),
                subtitle: Some(summary.subtitle_str),
                artist: Some(summary.artist_str),
                offset_seconds: Some(summary.offset),
                music_tag: Some(summary.music_path),
                charts,
            }
        }
        Err(err) => SimfileScan {
            simfile_path: path.display().to_string(),
            simfile_rel: rel,
            simfile_md5: digest,
            extension: ext,
            status: "error".to_string(),
            error: Some(format!("rssp analyze failed: {err}")),
            title: None,
            subtitle: None,
            artist: None,
            offset_seconds: None,
            music_tag: None,
            charts: Vec::new(),
        },
    }
}

fn charts_from_summary(charts: &[rssp::ChartSummary]) -> Vec<ChartScan> {
    charts
        .iter()
        .enumerate()
        .map(|(i, chart)| ChartScan {
            chart_index: i,
            steps_type: chart.step_type_str.clone(),
            difficulty: chart.difficulty_str.clone(),
            description: chart.description_str.clone(),
            music_tag: None,
            slot_null: slot_abbreviation(&chart.step_type_str, &chart.difficulty_str, i, "null"),
            slot_p9ms: slot_abbreviation(&chart.step_type_str, &chart.difficulty_str, i, "+9ms"),
            chart_has_own_timing: chart.chart_has_own_timing,
            status: "stub".to_string(),
            bias_ms: None,
            confidence: None,
            conv_quint: None,
            conv_stdev: None,
            paradigm: None,
        })
        .collect()
}

fn read_error(path: &Path, rel: &str, err: String) -> SimfileScan {
    SimfileScan {
        simfile_path: path.display().to_string(),
        simfile_rel: rel.to_string(),
        simfile_md5: String::new(),
        extension: simfile_ext(path),
        status: "error".to_string(),
        error: Some(err),
        title: None,
        subtitle: None,
        artist: None,
        offset_seconds: None,
        music_tag: None,
        charts: Vec::new(),
    }
}

fn simfile_ext(path: &Path) -> String {
    path.extension()
        .and_then(|s| s.to_str())
        .map_or_else(String::new, |s| s.to_ascii_lowercase())
}

fn bias_cfg_from_params(params: &AnalyzeParams) -> BiasCfg {
    BiasCfg {
        fingerprint_ms: params.fingerprint_ms,
        window_ms: params.window_ms,
        step_ms: params.step_ms,
        magic_offset_ms: params.magic_offset_ms,
        kernel_target: params.kernel_target,
        kernel_type: params.kernel_type,
        _full_spectrogram: params.full_spectrogram,
    }
}

fn chart_music_tags(charts: &[rssp::ChartSummary], fallback: &str) -> Vec<String> {
    charts
        .iter()
        .map(|_| choose_music_tag(None, fallback))
        .collect()
}

fn choose_music_tag(chart_music: Option<&str>, fallback: &str) -> String {
    let own = chart_music.map(str::trim).filter(|s| !s.is_empty());
    let root = fallback.trim();
    own.unwrap_or(root).to_string()
}

fn assign_chart_music(charts: &mut [ChartScan], tags: &[String]) {
    for (chart, tag) in charts.iter_mut().zip(tags.iter()) {
        let trimmed = tag.trim();
        if !trimmed.is_empty() {
            chart.music_tag = Some(trimmed.to_string());
        }
    }
}

struct AudioCacheEntry {
    path: PathBuf,
    decode: Result<OggDecode, String>,
}

fn resolve_song_audio_path(simfile_path: &Path, music_tag: &str) -> Result<PathBuf, String> {
    let Some(song_dir) = simfile_path.parent() else {
        return Err("simfile has no parent directory".to_string());
    };
    let Some(audio_path) = rssp::assets::resolve_music_path_like_itg(song_dir, music_tag) else {
        return Err(format!("could not resolve #MUSIC {:?}", music_tag));
    };
    if !is_ogg_path(&audio_path) {
        return Err(format!("unsupported audio format {}", audio_path.display()));
    }
    Ok(audio_path)
}

fn decode_song_audio(audio_path: &Path) -> Result<OggDecode, String> {
    decode_ogg_mono_like_python(audio_path)
}

fn decode_song_audio_cached(
    simfile_path: &Path,
    music_tag: &str,
    cache: &mut Vec<AudioCacheEntry>,
) -> Result<OggDecode, String> {
    let path = resolve_song_audio_path(simfile_path, music_tag)?;
    for entry in cache.iter() {
        if entry.path == path {
            return entry.decode.clone();
        }
    }
    let decode = decode_song_audio(&path);
    cache.push(AudioCacheEntry {
        path,
        decode: decode.clone(),
    });
    decode
}

fn apply_bias_estimates(
    simfile_path: &Path,
    summary_charts: &[rssp::ChartSummary],
    chart_scans: &mut [ChartScan],
    chart_music: &[String],
    song_title: &str,
    song_subtitle: &str,
    report_path: &Path,
    plot_enabled: bool,
    params: &AnalyzeParams,
    bias_cfg: &BiasCfg,
    trace_ctl: &TraceCtl,
) {
    let mut cache = Vec::new();
    let mut bias_rt = BiasRuntime::default();
    for (i, chart) in summary_charts.iter().enumerate() {
        let scan = &mut chart_scans[i];
        let music_tag = chart_music.get(i).map_or("", String::as_str);
        match decode_song_audio_cached(simfile_path, music_tag, &mut cache) {
            Ok(audio) => {
                if plot_enabled {
                    match estimate_bias_reuse_with_plot(
                        &audio.mono,
                        audio.sample_rate_hz,
                        chart,
                        bias_cfg,
                        &mut bias_rt,
                    ) {
                        Ok(est_plot) => {
                            write_bias(
                                scan,
                                est_plot.estimate.bias_ms,
                                est_plot.estimate.confidence,
                                est_plot.estimate.conv_quint,
                                est_plot.estimate.conv_stdev,
                                params,
                            );
                            if let Err(err) = write_chart_plots(
                                report_path,
                                song_title,
                                song_subtitle,
                                chart,
                                i,
                                est_plot.estimate.bias_ms,
                                params,
                                &est_plot.plot,
                            ) {
                                scan.description =
                                    append_error(&scan.description, &format!("plot_error: {err}"));
                            }
                            if trace_ctl.matches(simfile_path, chart, i) {
                                let _ = dump_trace_from_chart(
                                    simfile_path,
                                    chart,
                                    i,
                                    music_tag,
                                    params,
                                    &audio,
                                    bias_cfg,
                                    trace_ctl,
                                    &mut bias_rt,
                                );
                            }
                        }
                        Err(err) => write_bias_error(scan, &err),
                    }
                } else if trace_ctl.matches(simfile_path, chart, i) {
                    match estimate_bias_reuse_with_trace(
                        &audio.mono,
                        audio.sample_rate_hz,
                        chart,
                        bias_cfg,
                        &mut bias_rt,
                        BiasTraceCfg {
                            keep: trace_ctl.keep,
                        },
                    ) {
                        Ok((est, trace)) => {
                            write_bias(
                                scan,
                                est.bias_ms,
                                est.confidence,
                                est.conv_quint,
                                est.conv_stdev,
                                params,
                            );
                            let _ = dump_trace(
                                simfile_path,
                                chart,
                                i,
                                music_tag,
                                params,
                                &est,
                                &trace,
                                trace_ctl,
                            );
                        }
                        Err(err) => write_bias_error(scan, &err),
                    }
                } else {
                    match estimate_bias_reuse(
                        &audio.mono,
                        audio.sample_rate_hz,
                        chart,
                        bias_cfg,
                        &mut bias_rt,
                    ) {
                        Ok(est) => write_bias(
                            scan,
                            est.bias_ms,
                            est.confidence,
                            est.conv_quint,
                            est.conv_stdev,
                            params,
                        ),
                        Err(err) => write_bias_error(scan, &err),
                    }
                }
            }
            Err(err) => {
                scan.status = "audio_unavailable".to_string();
                scan.paradigm = Some("????".to_string());
                scan.description =
                    append_error(&scan.description, &format!("audio_unavailable: {err}"));
            }
        }
    }
}

fn dump_trace_from_chart(
    simfile_path: &Path,
    chart: &rssp::ChartSummary,
    chart_index: usize,
    music_tag: &str,
    params: &AnalyzeParams,
    audio: &OggDecode,
    bias_cfg: &BiasCfg,
    trace_ctl: &TraceCtl,
    bias_rt: &mut BiasRuntime,
) -> Result<(), String> {
    let (est, trace) = estimate_bias_reuse_with_trace(
        &audio.mono,
        audio.sample_rate_hz,
        chart,
        bias_cfg,
        bias_rt,
        BiasTraceCfg {
            keep: trace_ctl.keep,
        },
    )?;
    dump_trace(
        simfile_path,
        chart,
        chart_index,
        music_tag,
        params,
        &est,
        &trace,
        trace_ctl,
    )
}

fn write_chart_plots(
    report_path: &Path,
    song_title: &str,
    song_subtitle: &str,
    chart: &rssp::ChartSummary,
    chart_index: usize,
    bias_ms: f64,
    params: &AnalyzeParams,
    plot_data: &crate::bias::BiasPlotData,
) -> Result<(), String> {
    let guess = guess_paradigm(
        bias_ms,
        params.tolerance,
        params.consider_null,
        params.consider_p9ms,
        true,
    );
    let slot = slot_abbreviation(
        &chart.step_type_str,
        &chart.difficulty_str,
        chart_index,
        guess,
    );
    let stem_raw = format!("{} {}", full_title(song_title, song_subtitle), slot);
    let mut stem = slugify_ascii(&stem_raw);
    if stem.is_empty() {
        stem = format!("chart-{chart_index}");
    }
    write_nine_or_null_plots(report_path, &stem, plot_data)
}

fn full_title(title: &str, subtitle: &str) -> String {
    let title = title.trim();
    let subtitle = subtitle.trim();
    if subtitle.is_empty() {
        title.to_string()
    } else if title.is_empty() {
        subtitle.to_string()
    } else {
        format!("{title} {subtitle}")
    }
}

fn slugify_ascii(raw: &str) -> String {
    let mut cleaned = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch.is_ascii_whitespace() {
            cleaned.push(ch.to_ascii_lowercase());
        }
    }
    let mut out = String::with_capacity(cleaned.len());
    let mut last_dash = false;
    for ch in cleaned.chars() {
        if ch == '-' || ch.is_ascii_whitespace() {
            if !last_dash {
                out.push('-');
            }
            last_dash = true;
        } else {
            out.push(ch);
            last_dash = false;
        }
    }
    out.trim_matches(['-', '_']).to_string()
}

fn write_bias(
    scan: &mut ChartScan,
    bias_ms: f64,
    confidence: f64,
    conv_quint: f64,
    conv_stdev: f64,
    params: &AnalyzeParams,
) {
    scan.status = "computed".to_string();
    scan.bias_ms = Some(bias_ms);
    scan.confidence = Some(confidence);
    scan.conv_quint = Some(conv_quint);
    scan.conv_stdev = Some(conv_stdev);
    scan.paradigm = Some(resolve_paradigm(bias_ms, confidence, params));
}

fn resolve_paradigm(bias_ms: f64, confidence: f64, params: &AnalyzeParams) -> String {
    if confidence < params.confidence_limit {
        return "????".to_string();
    }
    let (consider_null, consider_p9ms) = target_paradigm_flags(params);
    guess_paradigm(
        bias_ms,
        params.tolerance,
        consider_null,
        consider_p9ms,
        true,
    )
    .to_string()
}

fn target_paradigm_flags(params: &AnalyzeParams) -> (bool, bool) {
    match params.to_paradigm.as_deref() {
        Some("null") => (true, false),
        Some("+9ms") => (false, true),
        _ => (params.consider_null, params.consider_p9ms),
    }
}

fn write_bias_error(scan: &mut ChartScan, err: &str) {
    scan.status = "bias_error".to_string();
    scan.paradigm = Some("????".to_string());
    scan.description = append_error(&scan.description, &format!("bias_error: {err}"));
}

fn append_error(desc: &str, extra: &str) -> String {
    if desc.trim().is_empty() {
        format!("[{extra}]")
    } else {
        format!("{desc} [{extra}]")
    }
}

fn is_ogg_path(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("ogg"))
}

fn dump_trace(
    simfile_path: &Path,
    chart: &rssp::ChartSummary,
    chart_index: usize,
    music_tag: &str,
    params: &AnalyzeParams,
    est: &crate::bias::BiasEstimate,
    trace: &BiasTrace,
    ctl: &TraceCtl,
) -> Result<(), String> {
    let dump_dir = ctl.dump_dir.clone().unwrap_or_else(|| {
        simfile_path
            .parent()
            .unwrap_or(simfile_path)
            .join("__bias-check")
    });
    fs::create_dir_all(&dump_dir)
        .map_err(|e| format!("create trace dir {} failed: {e}", dump_dir.display()))?;
    let sim_stem = simfile_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("simfile");
    let safe_sim = sanitize_file_stem(sim_stem);
    let out_path = dump_dir.join(format!(
        "null-or-die-trace-{safe_sim}-chart{chart_index}.json"
    ));
    let payload = AnalyzeTraceDump {
        simfile_path: simfile_path.display().to_string(),
        chart_index,
        steps_type: chart.step_type_str.clone(),
        difficulty: chart.difficulty_str.clone(),
        description: chart.description_str.clone(),
        music_tag: music_tag.to_string(),
        params: AnalyzeTraceParams {
            fingerprint_ms: params.fingerprint_ms,
            window_ms: params.window_ms,
            step_ms: params.step_ms,
            magic_offset_ms: params.magic_offset_ms,
            kernel_target: format!("{:?}", params.kernel_target),
            kernel_type: format!("{:?}", params.kernel_type),
            full_spectrogram: params.full_spectrogram,
        },
        estimate: AnalyzeTraceMetric {
            bias_ms: est.bias_ms,
            confidence: est.confidence,
            conv_quint: est.conv_quint,
            conv_stdev: est.conv_stdev,
        },
        trace: trace.clone(),
    };
    let json = serde_json::to_vec_pretty(&payload)
        .map_err(|e| format!("trace json encode failed for {}: {e}", out_path.display()))?;
    fs::write(&out_path, json)
        .map_err(|e| format!("trace write failed for {}: {e}", out_path.display()))?;
    Ok(())
}

fn sanitize_file_stem(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').to_string()
}

#[derive(Clone, Serialize)]
struct AnalyzeTraceDump {
    simfile_path: String,
    chart_index: usize,
    steps_type: String,
    difficulty: String,
    description: String,
    music_tag: String,
    params: AnalyzeTraceParams,
    estimate: AnalyzeTraceMetric,
    trace: BiasTrace,
}

#[derive(Clone, Serialize)]
struct AnalyzeTraceParams {
    fingerprint_ms: f64,
    window_ms: f64,
    step_ms: f64,
    magic_offset_ms: f64,
    kernel_target: String,
    kernel_type: String,
    full_spectrogram: bool,
}

#[derive(Clone, Serialize)]
struct AnalyzeTraceMetric {
    bias_ms: f64,
    confidence: f64,
    conv_quint: f64,
    conv_stdev: f64,
}

#[cfg(test)]
mod tests {
    use super::{choose_music_tag, resolve_paradigm};
    use crate::model::{AnalyzeParams, BiasKernel, KernelTarget};

    fn test_params() -> AnalyzeParams {
        AnalyzeParams {
            root_path: ".".to_string(),
            report_path: ".".to_string(),
            consider_null: true,
            consider_p9ms: true,
            tolerance: 4.0,
            confidence_limit: 0.8,
            fingerprint_ms: 50.0,
            window_ms: 10.0,
            step_ms: 0.2,
            magic_offset_ms: 0.0,
            kernel_target: KernelTarget::Digest,
            kernel_type: BiasKernel::Rising,
            full_spectrogram: false,
            to_paradigm: None,
        }
    }

    #[test]
    fn choose_music_prefers_chart_value() {
        assert_eq!(
            choose_music_tag(Some(" split.ogg "), "base.ogg"),
            "split.ogg".to_string()
        );
    }

    #[test]
    fn choose_music_falls_back_to_root() {
        assert_eq!(choose_music_tag(None, "base.ogg"), "base.ogg".to_string());
    }

    #[test]
    fn resolve_paradigm_applies_confidence_limit() {
        let params = test_params();
        assert_eq!(resolve_paradigm(0.1, 0.79, &params), "????".to_string());
        assert_eq!(resolve_paradigm(0.1, 0.80, &params), "null".to_string());
    }

    #[test]
    fn resolve_paradigm_respects_to_paradigm_target() {
        let mut params = test_params();
        params.to_paradigm = Some("null".to_string());
        assert_eq!(resolve_paradigm(8.9, 0.95, &params), "????".to_string());
        assert_eq!(resolve_paradigm(0.1, 0.95, &params), "null".to_string());

        params.to_paradigm = Some("+9ms".to_string());
        assert_eq!(resolve_paradigm(0.1, 0.95, &params), "????".to_string());
        assert_eq!(resolve_paradigm(8.9, 0.95, &params), "+9ms".to_string());
    }
}
