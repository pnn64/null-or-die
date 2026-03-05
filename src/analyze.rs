use std::fs;
use std::path::{Path, PathBuf};

use rssp::{AnalysisOptions, analyze};

use crate::audio::{OggDecode, decode_ogg_mono_like_python};
use crate::bias::{BiasCfg, estimate_bias};
use crate::cli::AnalyzeCmd;
use crate::compat::guess_paradigm;
use crate::compat::slot_abbreviation;
use crate::fs_scan::{discover_simfiles, md5_hex, rel_path};
use crate::model::{
    AnalyzeParams, AnalyzeReport, BiasKernel, ChartScan, KernelTarget, SimfileScan,
};

pub fn run(args: &AnalyzeCmd) -> Result<AnalyzeReport, String> {
    let report_path = resolve_report_path(&args.root_path, args.report_path.as_deref())?;
    fs::create_dir_all(&report_path)
        .map_err(|e| format!("create report dir {} failed: {e}", report_path.display()))?;
    let params = build_params(args, &report_path)?;
    let bias_cfg = bias_cfg_from_params(&params);
    let simfiles = discover_simfiles(&args.root_path)?;
    let scanned = simfiles
        .iter()
        .map(|path| scan_one(path, &args.root_path, &params, &bias_cfg))
        .collect::<Vec<_>>();
    Ok(AnalyzeReport {
        tool: "rnon".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        mode: "scan".to_string(),
        params,
        simfile_count: scanned.len(),
        simfiles: scanned,
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

fn scan_one(path: &Path, root: &Path, params: &AnalyzeParams, bias_cfg: &BiasCfg) -> SimfileScan {
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
                params,
                bias_cfg,
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
    params: &AnalyzeParams,
    bias_cfg: &BiasCfg,
) {
    let mut cache = Vec::new();
    for (i, chart) in summary_charts.iter().enumerate() {
        let scan = &mut chart_scans[i];
        let music_tag = chart_music.get(i).map_or("", String::as_str);
        match decode_song_audio_cached(simfile_path, music_tag, &mut cache) {
            Ok(audio) => match estimate_bias(&audio.mono, audio.sample_rate_hz, chart, bias_cfg) {
                Ok(est) => write_bias(
                    scan,
                    est.bias_ms,
                    est.confidence,
                    est.conv_quint,
                    est.conv_stdev,
                    params,
                ),
                Err(err) => write_bias_error(scan, &err),
            },
            Err(err) => {
                scan.status = "audio_unavailable".to_string();
                scan.paradigm = Some("????".to_string());
                scan.description =
                    append_error(&scan.description, &format!("audio_unavailable: {err}"));
            }
        }
    }
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
    scan.paradigm = Some(
        guess_paradigm(
            bias_ms,
            params.tolerance,
            params.consider_null,
            params.consider_p9ms,
            true,
        )
        .to_string(),
    );
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

#[cfg(test)]
mod tests {
    use super::choose_music_tag;

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
}
