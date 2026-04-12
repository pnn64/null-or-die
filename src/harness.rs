use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::cli::HarnessCmd;
use crate::fs_scan::{baseline_rel_for_md5, discover_simfiles, md5_hex, rel_path};
use crate::model::{HarnessCase, HarnessReport};

const PY_REF_SCRIPT: &str = r#"#!/usr/bin/env python3
import argparse
import json
import os
import sys
from pathlib import Path

def parse_bool(raw):
    return str(raw).strip().lower() in ("1", "true", "yes", "on")

def parse_args():
    ap = argparse.ArgumentParser(description="null-or-die python reference runner")
    ap.add_argument("--simfile-path", required=True)
    ap.add_argument("--report-dir", required=True)
    ap.add_argument("--source-root")
    ap.add_argument("--tolerance", type=float, required=True)
    ap.add_argument("--consider-null", required=True)
    ap.add_argument("--consider-p9ms", required=True)
    ap.add_argument("--fingerprint-ms", type=float, required=True)
    ap.add_argument("--window-ms", type=float, required=True)
    ap.add_argument("--step-ms", type=float, required=True)
    ap.add_argument("--magic-offset-ms", type=float, required=True)
    ap.add_argument("--kernel-target", required=True)
    ap.add_argument("--kernel-type", required=True)
    ap.add_argument("--full-spectrogram", required=True)
    return ap.parse_args()

def add_source_root(source_root):
    if not source_root:
        return
    root = Path(source_root).resolve()
    if root.is_file():
        root = root.parent
    sys.path.insert(0, str(root))

def parse_kernel_target(nine_or_null, raw):
    lookup = {
        "digest": nine_or_null.KernelTarget.DIGEST,
        "acc": nine_or_null.KernelTarget.ACCUMULATOR,
        "accumulator": nine_or_null.KernelTarget.ACCUMULATOR,
        "0": nine_or_null.KernelTarget.DIGEST,
        "1": nine_or_null.KernelTarget.ACCUMULATOR,
    }
    low = str(raw).strip().lower()
    if low in lookup:
        return lookup[low]
    return nine_or_null.KernelTarget(int(raw))

def parse_kernel_type(nine_or_null, raw):
    lookup = {
        "rising": nine_or_null.BiasKernel.RISING,
        "loudest": nine_or_null.BiasKernel.LOUDEST,
        "0": nine_or_null.BiasKernel.RISING,
        "1": nine_or_null.BiasKernel.LOUDEST,
    }
    low = str(raw).strip().lower()
    if low in lookup:
        return lookup[low]
    return nine_or_null.BiasKernel(int(raw))

def has_own_timing(chart):
    return any(k in chart for k in ["OFFSET", "BPMS", "STOPS", "DELAYS", "WARPS"])

def chart_rows(args, base_simfile, simfile_dir, nine_or_null, params):
    charts_within = [None]
    for chart_index, chart in enumerate(base_simfile.charts):
        if has_own_timing(chart):
            charts_within.append(chart_index)
    base_music = base_simfile.get("MUSIC", "")

    rows = []
    for chart_index in charts_within:
        fp = nine_or_null.check_sync_bias(
            simfile_dir,
            base_simfile,
            chart_index=chart_index,
            source_simfile_path=str(args.simfile_path),
            report_path=args.report_dir,
            save_plots=False,
            show_intermediate_plots=False,
            **params
        )
        bias = float(fp["bias_result"])
        confidence = float(fp["confidence"])
        paradigm = nine_or_null.guess_paradigm(bias, **params)

        if chart_index is None:
            row = {
                "chart_index": None,
                "slot": "*",
                "slot_null": "*",
                "slot_p9ms": "*",
                "steps_type": None,
                "difficulty": None,
                "description": None,
                "music": base_music,
                "chart_has_own_timing": False,
                "bias_ms": bias,
                "confidence": confidence,
                "conv_quint": float(fp.get("conv_quint")) if fp.get("conv_quint") is not None else None,
                "conv_stdev": float(fp.get("conv_stdev")) if fp.get("conv_stdev") is not None else None,
                "sample_rate": int(fp.get("sample_rate")) if fp.get("sample_rate") is not None else None,
                "paradigm": paradigm,
            }
        else:
            chart = base_simfile.charts[chart_index]
            steps_type = chart.get("STEPSTYPE")
            difficulty = chart.get("DIFFICULTY")
            description = chart.get("DESCRIPTION")
            slot = nine_or_null.slot_abbreviation(
                steps_type,
                difficulty,
                chart_index=chart_index,
                paradigm=paradigm,
            )
            row = {
                "chart_index": int(chart_index),
                "slot": slot,
                "slot_null": nine_or_null.slot_abbreviation(steps_type, difficulty, chart_index=chart_index, paradigm="null"),
                "slot_p9ms": nine_or_null.slot_abbreviation(steps_type, difficulty, chart_index=chart_index, paradigm="+9ms"),
                "steps_type": steps_type,
                "difficulty": difficulty,
                "description": description,
                "music": chart.get("MUSIC") or base_music,
                "chart_has_own_timing": has_own_timing(chart),
                "bias_ms": bias,
                "confidence": confidence,
                "conv_quint": float(fp.get("conv_quint")) if fp.get("conv_quint") is not None else None,
                "conv_stdev": float(fp.get("conv_stdev")) if fp.get("conv_stdev") is not None else None,
                "sample_rate": int(fp.get("sample_rate")) if fp.get("sample_rate") is not None else None,
                "paradigm": paradigm,
            }
        rows.append(row)

    rows.sort(key=lambda r: (-1 if r["chart_index"] is None else int(r["chart_index"])))
    return rows

def main():
    args = parse_args()
    os.makedirs(args.report_dir, exist_ok=True)
    add_source_root(args.source_root)

    import simfile
    import nine_or_null

    params = {
        "tolerance": float(args.tolerance),
        "consider_null": parse_bool(args.consider_null),
        "consider_p9ms": parse_bool(args.consider_p9ms),
        "fingerprint_ms": float(args.fingerprint_ms),
        "window_ms": float(args.window_ms),
        "step_ms": float(args.step_ms),
        "magic_offset_ms": float(args.magic_offset_ms),
        "kernel_target": parse_kernel_target(nine_or_null, args.kernel_target),
        "kernel_type": parse_kernel_type(nine_or_null, args.kernel_type),
        "full_spectrogram": parse_bool(args.full_spectrogram),
    }

    sim_path = Path(args.simfile_path).resolve()
    base = simfile.open(str(sim_path))
    rows = chart_rows(args, base, str(sim_path.parent), nine_or_null, params)

    out = {
        "tool": "nine-or-null",
        "version": nine_or_null._VERSION,
        "simfile_path": str(sim_path),
        "title": base.get("TITLE", ""),
        "titletranslit": base.get("TITLETRANSLIT", ""),
        "subtitle": base.get("SUBTITLE", ""),
        "subtitletranslit": base.get("SUBTITLETRANSLIT", ""),
        "artist": base.get("ARTIST", ""),
        "artisttranslit": base.get("ARTISTTRANSLIT", ""),
        "music": base.get("MUSIC", ""),
        "offset": float(base.get("OFFSET", "0") or "0"),
        "params": {
            "tolerance": float(args.tolerance),
            "consider_null": parse_bool(args.consider_null),
            "consider_p9ms": parse_bool(args.consider_p9ms),
            "fingerprint_ms": float(args.fingerprint_ms),
            "window_ms": float(args.window_ms),
            "step_ms": float(args.step_ms),
            "magic_offset_ms": float(args.magic_offset_ms),
            "kernel_target": str(args.kernel_target),
            "kernel_type": str(args.kernel_type),
            "full_spectrogram": parse_bool(args.full_spectrogram),
        },
        "charts": rows,
    }
    json.dump(out, sys.stdout, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    sys.stdout.write("\n")

if __name__ == "__main__":
    try:
        main()
    except Exception as exc:
        print(str(exc), file=sys.stderr)
        raise
"#;

pub fn run(args: &HarnessCmd) -> Result<HarnessReport, String> {
    let source_root = resolve_source_root(args)?;
    let scratch_path = resolve_scratch_path(args);
    fs::create_dir_all(&args.baseline_path).map_err(|e| {
        format!(
            "create baseline dir {} failed: {e}",
            args.baseline_path.display()
        )
    })?;
    fs::create_dir_all(&scratch_path)
        .map_err(|e| format!("create scratch dir {} failed: {e}", scratch_path.display()))?;
    let script_path = write_python_script(&scratch_path)?;
    let simfiles = discover_simfiles(&args.root_path)?;
    let mut cases = Vec::with_capacity(simfiles.len());
    for simfile in simfiles {
        cases.push(run_one(
            args,
            &source_root,
            &scratch_path,
            &script_path,
            &simfile,
        ));
    }
    if !args.keep_scratch {
        let _ = fs::remove_dir_all(&scratch_path);
    }
    Ok(build_report(
        args,
        source_root.as_deref(),
        &scratch_path,
        cases,
    ))
}

fn run_one(
    args: &HarnessCmd,
    source_root: &Option<PathBuf>,
    scratch_path: &Path,
    script_path: &Path,
    simfile: &Path,
) -> HarnessCase {
    let simfile_rel = rel_path(&args.root_path, simfile);
    let bytes = match fs::read(simfile) {
        Ok(bytes) => bytes,
        Err(err) => return case_error(simfile_rel, String::new(), String::new(), err.to_string()),
    };
    let simfile_md5 = md5_hex(&bytes);
    let baseline_rel = fixture_rel_for_md5(&simfile_md5);
    let baseline_path = args.baseline_path.join(&baseline_rel);
    if baseline_path.exists() && !args.overwrite {
        return HarnessCase {
            simfile_rel,
            simfile_md5,
            baseline_rel,
            status: "skipped_existing".to_string(),
            error: None,
        };
    }
    let report_dir = scratch_path.join("report").join(&simfile_md5);
    if let Err(err) = fs::create_dir_all(&report_dir) {
        return case_error(simfile_rel, simfile_md5, baseline_rel, err.to_string());
    }
    let output = match run_python(
        args,
        source_root.as_deref(),
        script_path,
        simfile,
        &report_dir,
    ) {
        Ok(out) => out,
        Err(err) => return case_error(simfile_rel, simfile_md5, baseline_rel, err),
    };
    let canonical = match canonical_json(output) {
        Ok(json) => json,
        Err(err) => return case_error(simfile_rel, simfile_md5, baseline_rel, err),
    };
    if let Some(parent) = baseline_path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            return case_error(simfile_rel, simfile_md5, baseline_rel, err.to_string());
        }
    }
    let compressed = match zstd::stream::encode_all(&canonical[..], args.zstd_level) {
        Ok(data) => data,
        Err(err) => return case_error(simfile_rel, simfile_md5, baseline_rel, err.to_string()),
    };
    match fs::write(&baseline_path, compressed) {
        Ok(()) => HarnessCase {
            simfile_rel,
            simfile_md5,
            baseline_rel,
            status: "written".to_string(),
            error: None,
        },
        Err(err) => case_error(simfile_rel, simfile_md5, baseline_rel, err.to_string()),
    }
}

fn resolve_source_root(args: &HarnessCmd) -> Result<Option<PathBuf>, String> {
    if let Some(path) = &args.source_root {
        return Ok(Some(path.clone()));
    }
    if let Some(path) = default_source_root() {
        return Ok(Some(path));
    }
    Ok(None)
}

fn default_source_root() -> Option<PathBuf> {
    let cwd = env::current_dir().ok()?;
    let candidate = cwd.join("nine-or-null-0.8.0").join("nine-or-null");
    candidate.exists().then_some(candidate)
}

fn resolve_scratch_path(args: &HarnessCmd) -> PathBuf {
    if let Some(path) = &args.scratch_path {
        return path.clone();
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    args.baseline_path
        .join(format!("__null-or-die-harness-{stamp}"))
}

fn write_python_script(scratch_path: &Path) -> Result<PathBuf, String> {
    let script_path = scratch_path.join("null_or_die_python_ref.py");
    fs::write(&script_path, PY_REF_SCRIPT)
        .map_err(|e| format!("write script {} failed: {e}", script_path.display()))?;
    Ok(script_path)
}

fn run_python(
    args: &HarnessCmd,
    source_root: Option<&Path>,
    script_path: &Path,
    simfile: &Path,
    report_dir: &Path,
) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new(&args.python_bin);
    cmd.arg(script_path)
        .arg("--simfile-path")
        .arg(simfile)
        .arg("--report-dir")
        .arg(report_dir)
        .arg("--tolerance")
        .arg(args.tolerance.to_string())
        .arg("--consider-null")
        .arg(bool_str(args.consider_null))
        .arg("--consider-p9ms")
        .arg(bool_str(args.consider_p9ms))
        .arg("--fingerprint-ms")
        .arg(args.fingerprint_ms.to_string())
        .arg("--window-ms")
        .arg(args.window_ms.to_string())
        .arg("--step-ms")
        .arg(args.step_ms.to_string())
        .arg("--magic-offset-ms")
        .arg(args.magic_offset_ms.to_string())
        .arg("--kernel-target")
        .arg(&args.kernel_target)
        .arg("--kernel-type")
        .arg(&args.kernel_type)
        .arg("--full-spectrogram")
        .arg(bool_str(args.full_spectrogram));
    if let Some(source_root) = source_root {
        cmd.arg("--source-root").arg(source_root);
    }
    let output = cmd
        .output()
        .map_err(|e| format!("failed to launch {}: {e}", args.python_bin))?;
    if output.status.success() {
        if output.stdout.is_empty() {
            Err("python reference runner returned empty output".to_string())
        } else {
            Ok(output.stdout)
        }
    } else {
        Err(format!(
            "python exit {:?}: {}",
            output.status.code(),
            trim_text(&String::from_utf8_lossy(&output.stderr), 1200)
        ))
    }
}

fn canonical_json(raw: Vec<u8>) -> Result<Vec<u8>, String> {
    let value: Value = serde_json::from_slice(&raw)
        .map_err(|e| format!("python output is not valid JSON: {e}"))?;
    let mut out = serde_json::to_vec(&value).map_err(|e| format!("canonical json failed: {e}"))?;
    out.push(b'\n');
    Ok(out)
}

fn fixture_rel_for_md5(md5: &str) -> String {
    format!("{}.zst", baseline_rel_for_md5(md5))
}

fn case_error(
    simfile_rel: String,
    simfile_md5: String,
    baseline_rel: String,
    error: String,
) -> HarnessCase {
    HarnessCase {
        simfile_rel,
        simfile_md5,
        baseline_rel,
        status: "error".to_string(),
        error: Some(error),
    }
}

fn build_report(
    args: &HarnessCmd,
    source_root: Option<&Path>,
    scratch_path: &Path,
    cases: Vec<HarnessCase>,
) -> HarnessReport {
    let written = count_cases(&cases, "written");
    let skipped_existing = count_cases(&cases, "skipped_existing");
    let failed = count_cases(&cases, "error");
    HarnessReport {
        tool: crate::TOOL_NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        mode: "python-reference-harness".to_string(),
        root_path: args.root_path.display().to_string(),
        baseline_path: args.baseline_path.display().to_string(),
        python_bin: args.python_bin.clone(),
        source_root: source_root.map(|p| p.display().to_string()),
        scratch_path: scratch_path.display().to_string(),
        total_simfiles: cases.len(),
        written,
        skipped_existing,
        failed,
        cases,
    }
}

fn count_cases(cases: &[HarnessCase], status: &str) -> usize {
    cases.iter().filter(|c| c.status == status).count()
}

fn trim_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        format!(
            "{}...[truncated]",
            text.chars().take(max_chars).collect::<String>()
        )
    }
}

fn bool_str(value: bool) -> &'static str {
    if value { "1" } else { "0" }
}

#[cfg(test)]
mod tests {
    use super::{PY_REF_SCRIPT, fixture_rel_for_md5};

    #[test]
    fn fixture_path_shards_and_compresses() {
        let rel = fixture_rel_for_md5("abcdef0123456789abcdef0123456789");
        assert_eq!(rel, "ab/abcdef0123456789abcdef0123456789.json.zst");
    }

    #[test]
    fn python_fixture_rows_include_music_tag() {
        assert!(PY_REF_SCRIPT.contains("\"music\": base_music"));
        assert!(PY_REF_SCRIPT.contains("\"music\": chart.get(\"MUSIC\") or base_music"));
    }
}
