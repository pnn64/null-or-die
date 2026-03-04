use std::fs;
use std::path::{Path, PathBuf};

use rssp::{AnalysisOptions, analyze};

use crate::cli::AnalyzeCmd;
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
    let simfiles = discover_simfiles(&args.root_path)?;
    let scanned = simfiles
        .iter()
        .map(|path| scan_one(path, &args.root_path))
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

fn scan_one(path: &Path, root: &Path) -> SimfileScan {
    let rel = rel_path(root, path);
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => return read_error(path, &rel, format!("read failed: {err}")),
    };
    let ext = simfile_ext(path);
    let digest = md5_hex(&bytes);
    let options = AnalysisOptions::default();
    match analyze(&bytes, &ext, &options) {
        Ok(summary) => SimfileScan {
            simfile_path: path.display().to_string(),
            simfile_rel: rel,
            simfile_md5: digest,
            extension: ext,
            status: "stub".to_string(),
            error: None,
            title: Some(summary.title_str),
            subtitle: Some(summary.subtitle_str),
            artist: Some(summary.artist_str),
            offset_seconds: Some(summary.offset),
            music_tag: Some(summary.music_path),
            charts: charts_from_summary(&summary.charts),
        },
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
            slot_null: slot_abbreviation(&chart.step_type_str, &chart.difficulty_str, i, "null"),
            slot_p9ms: slot_abbreviation(&chart.step_type_str, &chart.difficulty_str, i, "+9ms"),
            chart_has_own_timing: chart.chart_has_own_timing,
            status: "stub".to_string(),
            bias_ms: None,
            confidence: None,
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
