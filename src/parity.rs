use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use rssp::{AnalysisOptions, analyze};
use serde::Deserialize;

use crate::audio::{OggDecode, decode_ogg_mono_like_python};
use crate::bias::{BiasCfg, estimate_bias_with_beat_fn};
use crate::cli::ParityCmd;
use crate::compat::{guess_paradigm, slot_abbreviation};
use crate::fs_scan::{baseline_rel_for_md5, discover_simfiles, md5_hex, rel_path};
use crate::model::{BiasKernel, KernelTarget, ParityCase, ParityReport};

const BIAS_MS_TOL: f64 = 0.25;
const CONF_TOL: f64 = 1e-3;
const CONV_TOL: f64 = 1e-3;

pub fn run(args: &ParityCmd) -> Result<ParityReport, String> {
    let simfiles = discover_simfiles(&args.root_path)?;
    let mut cases = Vec::with_capacity(simfiles.len());
    for simfile in simfiles {
        cases.push(check_one(&simfile, &args.root_path, &args.baseline_path));
    }
    Ok(build_report(&args.root_path, &args.baseline_path, cases))
}

fn check_one(path: &Path, root: &Path, baseline_root: &Path) -> ParityCase {
    let simfile_rel = rel_path(root, path);
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            return case_error(
                simfile_rel,
                "read_error",
                format!("read simfile failed: {err}"),
            );
        }
    };
    let digest = md5_hex(&bytes);
    let baseline_rel = baseline_rel_for_md5(&digest);
    let (candidate_json, candidate_zst) = baseline_candidates(baseline_root, &digest);
    let baseline_path = if candidate_json.exists() {
        candidate_json
    } else if candidate_zst.exists() {
        candidate_zst
    } else {
        return ParityCase {
            simfile_rel,
            simfile_md5: digest,
            baseline_rel: Some(baseline_rel),
            status: "missing_baseline".to_string(),
            error: None,
        };
    };
    let baseline = match read_baseline(&baseline_path).and_then(parse_baseline) {
        Ok(b) => b,
        Err(err) => {
            return ParityCase {
                simfile_rel,
                simfile_md5: digest,
                baseline_rel: Some(baseline_rel),
                status: "invalid_baseline".to_string(),
                error: Some(err),
            };
        }
    };
    match compare_baseline(path, &bytes, &baseline) {
        Ok(None) => ParityCase {
            simfile_rel,
            simfile_md5: digest,
            baseline_rel: Some(baseline_rel),
            status: "matched".to_string(),
            error: None,
        },
        Ok(Some(msg)) => ParityCase {
            simfile_rel,
            simfile_md5: digest,
            baseline_rel: Some(baseline_rel),
            status: "mismatch".to_string(),
            error: Some(msg),
        },
        Err(err) => ParityCase {
            simfile_rel,
            simfile_md5: digest,
            baseline_rel: Some(baseline_rel),
            status: "invalid_baseline".to_string(),
            error: Some(err),
        },
    }
}

fn case_error(simfile_rel: String, status: &str, error: String) -> ParityCase {
    ParityCase {
        simfile_rel,
        simfile_md5: String::new(),
        baseline_rel: None,
        status: status.to_string(),
        error: Some(error),
    }
}

struct CompareOutcome {
    mismatches: Vec<String>,
    #[cfg(test)]
    logs: Vec<String>,
}

struct CompareState {
    verbose: bool,
    mismatches: Vec<String>,
    logs: Vec<String>,
}

impl CompareState {
    fn new(verbose: bool) -> Self {
        Self {
            verbose,
            mismatches: Vec::new(),
            logs: Vec::new(),
        }
    }

    fn note(&mut self, msg: String) {
        if self.verbose {
            self.logs.push(msg);
        }
    }

    fn check(&mut self, display: String, ok: bool, mismatch: String) {
        if self.verbose {
            let status = if ok { "....ok" } else { "....MISMATCH" };
            self.logs.push(format!("{display} {status}"));
        }
        if !ok {
            self.mismatches.push(mismatch);
        }
    }

    fn mismatch(&mut self, mismatch: String) {
        self.check(mismatch.clone(), false, mismatch);
    }

    fn finish(self) -> CompareOutcome {
        CompareOutcome {
            mismatches: self.mismatches,
            #[cfg(test)]
            logs: self.logs,
        }
    }
}

fn compare_baseline(
    simfile_path: &Path,
    simfile_bytes: &[u8],
    baseline: &BaselineFixture,
) -> Result<Option<String>, String> {
    let outcome = compare_baseline_inner(simfile_path, simfile_bytes, baseline, false)?;
    if outcome.mismatches.is_empty() {
        Ok(None)
    } else {
        Ok(Some(outcome.mismatches.join("; ")))
    }
}

#[cfg(test)]
fn compare_baseline_verbose(
    simfile_path: &Path,
    simfile_bytes: &[u8],
    baseline: &BaselineFixture,
) -> Result<CompareOutcome, String> {
    compare_baseline_inner(simfile_path, simfile_bytes, baseline, true)
}

fn compare_baseline_inner(
    simfile_path: &Path,
    simfile_bytes: &[u8],
    baseline: &BaselineFixture,
    verbose: bool,
) -> Result<CompareOutcome, String> {
    if baseline.charts.is_empty() {
        let mut state = CompareState::new(verbose);
        state.note("  no chart rows in baseline; skipping".to_string());
        return Ok(state.finish());
    }
    let mut state = CompareState::new(verbose);
    state.note(format!("analyzing {}", simfile_path.display()));
    let ext = simfile_ext(simfile_path);
    let summary = analyze(simfile_bytes, &ext, &AnalysisOptions::default())
        .map_err(|e| format!("rssp analyze failed: {e}"))?;
    let cfg = bias_cfg_from_params(&baseline.params)?;
    let global_timing = global_timing_data(&summary)?;
    let song_dir = simfile_path.parent().ok_or_else(|| {
        format!(
            "simfile has no parent directory: {}",
            simfile_path.display()
        )
    })?;
    let mut cache = Vec::new();
    for row in &baseline.charts {
        compare_row(
            row,
            baseline,
            &summary,
            &global_timing,
            song_dir,
            &cfg,
            &mut cache,
            &mut state,
        )?;
    }
    Ok(state.finish())
}

fn compare_row(
    row: &BaselineChart,
    baseline: &BaselineFixture,
    summary: &rssp::SimfileSummary,
    global_timing: &PyTimingEngine,
    song_dir: &Path,
    cfg: &BiasCfg,
    cache: &mut Vec<AudioCacheEntry>,
    state: &mut CompareState,
) -> Result<(), String> {
    state.note(format!("  {}", chart_label(row)));
    if row.chart_index.is_none() {
        compare_base_row_meta(row, state);
        if !row_needs_audio(row) {
            return Ok(());
        }
        let Some(music_tag) = chart_music_tag(row, &baseline.music, None, &summary.music_path)
        else {
            state.mismatch(format!("{} missing music tag", chart_label(row)));
            return Ok(());
        };
        let Some(audio_path) = rssp::assets::resolve_music_path_like_itg(song_dir, music_tag)
        else {
            return Err(format!(
                "{} unresolved #MUSIC {:?}",
                chart_label(row),
                music_tag
            ));
        };
        if !is_ogg_path(&audio_path) {
            return Err(format!(
                "{} unsupported audio format {}",
                chart_label(row),
                audio_path.display()
            ));
        }
        let decode = decode_cached(&audio_path, cache)
            .map_err(|e| format!("{} audio decode failed: {e}", chart_label(row)))?;
        compare_sample_rate(row, decode.sample_rate_hz, state);
        if !row_has_bias_fields(row) {
            return Ok(());
        }
        let est = estimate_bias_with_beat_fn(&decode.mono, decode.sample_rate_hz, cfg, |beat| {
            global_timing.time_at_stop(beat as f64)
        })
        .map_err(|e| format!("{} bias estimation failed: {e}", chart_label(row)))?;
        compare_base_row_fields(row, baseline, &est, state);
        return Ok(());
    }
    let Some(chart) = chart_for_row(summary, row.chart_index) else {
        state.mismatch(format!("{} missing in simfile summary", chart_label(row)));
        return Ok(());
    };
    compare_row_meta(row, chart, state);
    if !row_needs_audio(row) {
        return Ok(());
    }
    let Some(music_tag) = chart_music_tag(row, &baseline.music, None, &summary.music_path) else {
        state.mismatch(format!("{} missing music tag", chart_label(row)));
        return Ok(());
    };
    let Some(audio_path) = rssp::assets::resolve_music_path_like_itg(song_dir, music_tag) else {
        return Err(format!(
            "{} unresolved #MUSIC {:?}",
            chart_label(row),
            music_tag
        ));
    };
    if !is_ogg_path(&audio_path) {
        return Err(format!(
            "{} unsupported audio format {}",
            chart_label(row),
            audio_path.display()
        ));
    }
    let decode = decode_cached(&audio_path, cache)
        .map_err(|e| format!("{} audio decode failed: {e}", chart_label(row)))?;
    compare_sample_rate(row, decode.sample_rate_hz, state);
    if !row_has_bias_fields(row) {
        return Ok(());
    }
    let timing = chart_timing_data(summary, chart, global_timing)?;
    let est = estimate_bias_with_beat_fn(&decode.mono, decode.sample_rate_hz, cfg, |beat| {
        timing.time_at_stop(beat as f64)
    })
    .map_err(|e| format!("{} bias estimation failed: {e}", chart_label(row)))?;
    compare_row_fields(row, baseline, chart, &est, state);
    Ok(())
}

fn compare_row_meta(row: &BaselineChart, chart: &rssp::ChartSummary, state: &mut CompareState) {
    compare_opt_text(
        row,
        "steps_type",
        row.steps_type.as_deref(),
        Some(chart.step_type_str.as_str()),
        state,
    );
    compare_opt_text(
        row,
        "difficulty",
        row.difficulty.as_deref(),
        Some(chart.difficulty_str.as_str()),
        state,
    );
    compare_opt_text(
        row,
        "description",
        row.description.as_deref(),
        Some(chart.description_str.as_str()),
        state,
    );
    if let Some(base) = row.chart_has_own_timing {
        let expected = chart.chart_has_own_timing;
        let display = format!(
            "    {}.chart_has_own_timing: baseline={base} expected={expected}",
            chart_label(row)
        );
        let mismatch = format!(
            "{}.chart_has_own_timing mismatch: baseline={base} expected={expected}",
            chart_label(row)
        );
        state.check(display, base == expected, mismatch);
    }
    compare_opt_text(
        row,
        "slot_null",
        row.slot_null.as_deref(),
        Some(expected_slot_value(row, chart, "null").as_str()),
        state,
    );
    compare_opt_text(
        row,
        "slot_p9ms",
        row.slot_p9ms.as_deref(),
        Some(expected_slot_value(row, chart, "+9ms").as_str()),
        state,
    );
}

fn compare_base_row_meta(row: &BaselineChart, state: &mut CompareState) {
    if let Some(base) = row.chart_has_own_timing {
        let expected = false;
        let display = format!(
            "    {}.chart_has_own_timing: baseline={base} expected={expected}",
            chart_label(row)
        );
        let mismatch = format!(
            "{}.chart_has_own_timing mismatch: baseline={base} expected={expected}",
            chart_label(row)
        );
        state.check(display, base == expected, mismatch);
    }
    compare_opt_text(row, "slot_null", row.slot_null.as_deref(), Some("*"), state);
    compare_opt_text(row, "slot_p9ms", row.slot_p9ms.as_deref(), Some("*"), state);
}

fn compare_row_fields(
    row: &BaselineChart,
    baseline: &BaselineFixture,
    chart: &rssp::ChartSummary,
    est: &crate::bias::BiasEstimate,
    state: &mut CompareState,
) {
    compare_float(row, "bias_ms", row.bias_ms, est.bias_ms, BIAS_MS_TOL, state);
    compare_float(
        row,
        "confidence",
        row.confidence,
        est.confidence,
        CONF_TOL,
        state,
    );
    compare_float(
        row,
        "conv_quint",
        row.conv_quint,
        est.conv_quint,
        CONV_TOL,
        state,
    );
    compare_float(
        row,
        "conv_stdev",
        row.conv_stdev,
        est.conv_stdev,
        CONV_TOL,
        state,
    );
    let expected_paradigm = guess_paradigm(
        est.bias_ms,
        baseline.params.tolerance,
        baseline.params.consider_null,
        baseline.params.consider_p9ms,
        true,
    );
    if let Some(base) = normalize_opt_text(row.paradigm.as_deref()) {
        let display = format!(
            "    {}.paradigm: baseline={:?} expected={:?}",
            chart_label(row),
            base,
            expected_paradigm
        );
        let mismatch = format!(
            "{}.paradigm mismatch: baseline={:?} expected={:?}",
            chart_label(row),
            base,
            expected_paradigm
        );
        state.check(display, base == expected_paradigm, mismatch);
    }
    compare_opt_text(
        row,
        "slot",
        row.slot.as_deref(),
        Some(expected_slot_value(row, chart, expected_paradigm).as_str()),
        state,
    );
}

fn compare_base_row_fields(
    row: &BaselineChart,
    baseline: &BaselineFixture,
    est: &crate::bias::BiasEstimate,
    state: &mut CompareState,
) {
    compare_float(row, "bias_ms", row.bias_ms, est.bias_ms, BIAS_MS_TOL, state);
    compare_float(
        row,
        "confidence",
        row.confidence,
        est.confidence,
        CONF_TOL,
        state,
    );
    compare_float(
        row,
        "conv_quint",
        row.conv_quint,
        est.conv_quint,
        CONV_TOL,
        state,
    );
    compare_float(
        row,
        "conv_stdev",
        row.conv_stdev,
        est.conv_stdev,
        CONV_TOL,
        state,
    );
    let expected_paradigm = guess_paradigm(
        est.bias_ms,
        baseline.params.tolerance,
        baseline.params.consider_null,
        baseline.params.consider_p9ms,
        true,
    );
    if let Some(base) = normalize_opt_text(row.paradigm.as_deref()) {
        let display = format!(
            "    {}.paradigm: baseline={:?} expected={:?}",
            chart_label(row),
            base,
            expected_paradigm
        );
        let mismatch = format!(
            "{}.paradigm mismatch: baseline={:?} expected={:?}",
            chart_label(row),
            base,
            expected_paradigm
        );
        state.check(display, base == expected_paradigm, mismatch);
    }
    compare_opt_text(row, "slot", row.slot.as_deref(), Some("*"), state);
}

fn compare_sample_rate(row: &BaselineChart, expected: u32, state: &mut CompareState) {
    let Some(base) = row.sample_rate else { return };
    let display = format!(
        "    {}.sample_rate: baseline={base} expected={expected}",
        chart_label(row)
    );
    let mismatch = format!(
        "{}.sample_rate mismatch: baseline={base} expected={expected}",
        chart_label(row)
    );
    state.check(display, base == expected, mismatch);
}

fn compare_opt_text(
    row: &BaselineChart,
    field: &str,
    baseline: Option<&str>,
    expected: Option<&str>,
    state: &mut CompareState,
) {
    let Some(base) = normalize_opt_text(baseline) else {
        return;
    };
    let expect = expected.and_then(non_empty_trim).unwrap_or("");
    let display = format!(
        "    {}.{field}: baseline={:?} expected={:?}",
        chart_label(row),
        base,
        expect
    );
    let mismatch = format!(
        "{}.{field} mismatch: baseline={:?} expected={:?}",
        chart_label(row),
        base,
        expect
    );
    state.check(display, base == expect, mismatch);
}

fn expected_slot_value(row: &BaselineChart, chart: &rssp::ChartSummary, paradigm: &str) -> String {
    if row.chart_index.is_none() {
        return "*".to_string();
    }
    slot_abbreviation(
        &chart.step_type_str,
        &chart.difficulty_str,
        row.chart_index.unwrap_or(0),
        paradigm,
    )
}

fn row_needs_audio(row: &BaselineChart) -> bool {
    row.sample_rate.is_some() || row_has_bias_fields(row)
}

fn compare_float(
    row: &BaselineChart,
    field: &str,
    baseline: Option<f64>,
    expected: f64,
    tol: f64,
    state: &mut CompareState,
) {
    let Some(base) = baseline else { return };
    let ok = (base - expected).abs() <= tol;
    let display = format!(
        "    {}.{field}: baseline={base:.6} expected={expected:.6} tolerance={tol:.6}",
        chart_label(row)
    );
    let mismatch = format!(
        "{}.{field} mismatch: baseline={base:.6} expected={expected:.6} tolerance={tol:.6}",
        chart_label(row)
    );
    state.check(display, ok, mismatch);
}

fn row_has_bias_fields(row: &BaselineChart) -> bool {
    row.bias_ms.is_some()
        || row.confidence.is_some()
        || row.conv_quint.is_some()
        || row.conv_stdev.is_some()
        || normalize_opt_text(row.paradigm.as_deref()).is_some()
}

fn normalize_opt_text(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

fn chart_for_row(
    summary: &rssp::SimfileSummary,
    chart_index: Option<usize>,
) -> Option<&rssp::ChartSummary> {
    match chart_index {
        Some(i) => summary.charts.get(i),
        None => summary
            .charts
            .iter()
            .find(|chart| !chart.chart_has_own_timing)
            .or_else(|| summary.charts.first()),
    }
}

fn chart_music_tag<'a>(
    row: &'a BaselineChart,
    root_music: &'a str,
    chart_music: Option<&'a str>,
    summary_music: &'a str,
) -> Option<&'a str> {
    row.music
        .as_deref()
        .and_then(non_empty_trim)
        .or_else(|| chart_music.and_then(non_empty_trim))
        .or_else(|| non_empty_trim(root_music))
        .or_else(|| non_empty_trim(summary_music))
}

fn non_empty_trim(s: &str) -> Option<&str> {
    let t = s.trim();
    if t.is_empty() { None } else { Some(t) }
}

fn chart_label(row: &BaselineChart) -> String {
    row.chart_index
        .map_or_else(|| "chart[base]".to_string(), |i| format!("chart[{i}]"))
}

fn decode_cached(path: &Path, cache: &mut Vec<AudioCacheEntry>) -> Result<OggDecode, String> {
    let mut decode = |p: &Path| decode_ogg_mono_like_python(p);
    decode_cached_with(path, cache, &mut decode)
}

fn decode_cached_with<F>(
    path: &Path,
    cache: &mut Vec<AudioCacheEntry>,
    decode_fn: &mut F,
) -> Result<OggDecode, String>
where
    F: FnMut(&Path) -> Result<OggDecode, String>,
{
    for entry in cache.iter() {
        if entry.path == path {
            return entry.decode.clone();
        }
    }
    let decode = decode_fn(path);
    cache.push(AudioCacheEntry {
        path: path.to_path_buf(),
        decode: decode.clone(),
    });
    decode
}

fn is_ogg_path(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("ogg"))
}

fn bias_cfg_from_params(params: &BaselineParams) -> Result<BiasCfg, String> {
    Ok(BiasCfg {
        fingerprint_ms: params.fingerprint_ms,
        window_ms: params.window_ms,
        step_ms: params.step_ms,
        magic_offset_ms: params.magic_offset_ms,
        kernel_target: parse_kernel_target(&params.kernel_target)?,
        kernel_type: parse_kernel_type(&params.kernel_type)?,
        _full_spectrogram: params.full_spectrogram,
    })
}

fn global_timing_data(summary: &rssp::SimfileSummary) -> Result<PyTimingEngine, String> {
    PyTimingEngine::from_maps(
        summary.offset,
        &summary.normalized_bpms,
        &summary.normalized_stops,
        &summary.normalized_delays,
        &summary.normalized_warps,
    )
}

fn chart_timing_data(
    summary: &rssp::SimfileSummary,
    chart: &rssp::ChartSummary,
    global: &PyTimingEngine,
) -> Result<PyTimingEngine, String> {
    if !python_uses_chart_timing(summary, chart) {
        return Ok(global.clone());
    }
    let chart_offset = if (chart.chart_offset_seconds - summary.offset).abs() > 1e-12 {
        chart.chart_offset_seconds
    } else {
        0.0
    };
    PyTimingEngine::from_maps(
        chart_offset,
        chart.chart_bpms.as_deref().unwrap_or(""),
        chart.chart_stops.as_deref().unwrap_or(""),
        chart.chart_delays.as_deref().unwrap_or(""),
        chart.chart_warps.as_deref().unwrap_or(""),
    )
}

fn python_uses_chart_timing(summary: &rssp::SimfileSummary, chart: &rssp::ChartSummary) -> bool {
    summary.timing_format == rssp::timing::TimingFormat::Ssc
        && summary.ssc_version >= 0.7
        && chart_has_timing_props(chart)
}

fn chart_has_timing_props(chart: &rssp::ChartSummary) -> bool {
    opt_has_text(chart.chart_bpms.as_deref())
        || opt_has_text(chart.chart_stops.as_deref())
        || opt_has_text(chart.chart_delays.as_deref())
        || opt_has_text(chart.chart_time_signatures.as_deref())
        || opt_has_text(chart.chart_tickcounts.as_deref())
        || opt_has_text(chart.chart_combos.as_deref())
        || opt_has_text(chart.chart_warps.as_deref())
        || opt_has_text(chart.chart_speeds.as_deref())
        || opt_has_text(chart.chart_scrolls.as_deref())
        || opt_has_text(chart.chart_fakes.as_deref())
        || opt_has_text(chart.chart_labels.as_deref())
}

fn opt_has_text(value: Option<&str>) -> bool {
    value.is_some_and(|s| !s.trim().is_empty())
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PyEventTag {
    Warp = 0,
    WarpEnd = 1,
    Bpm = 2,
    Delay = 3,
    DelayEnd = 4,
    Stop = 5,
    StopEnd = 6,
}

#[derive(Clone, Copy)]
struct PyEvent {
    beat: f64,
    value: f64,
    tag: PyEventTag,
    seq: usize,
}

#[derive(Clone, Copy)]
struct PyTimingState {
    beat: f64,
    value: f64,
    tag: PyEventTag,
    time: f64,
    bpm: f64,
    warp: bool,
}

#[derive(Clone)]
struct PyTimingEngine {
    states: Vec<PyTimingState>,
}

impl PyTimingEngine {
    fn from_maps(
        offset_sec: f64,
        bpms_raw: &str,
        stops_raw: &str,
        delays_raw: &str,
        warps_raw: &str,
    ) -> Result<Self, String> {
        let mut bpms = parse_beat_values(bpms_raw);
        if bpms.is_empty() {
            return Err("timing parse failed: empty BPMS".to_string());
        }
        bpms.sort_by(|a, b| a.0.total_cmp(&b.0));
        let first_bpm = bpms[0];
        if first_bpm.0 != 0.0 {
            return Err(format!(
                "timing parse failed: first BPM must be at beat 0, got {}",
                first_bpm.0
            ));
        }

        let mut stops = parse_beat_values(stops_raw);
        let mut delays = parse_beat_values(delays_raw);
        let mut warps = parse_beat_values(warps_raw);
        stops.sort_by(|a, b| a.0.total_cmp(&b.0));
        delays.sort_by(|a, b| a.0.total_cmp(&b.0));
        warps.sort_by(|a, b| a.0.total_cmp(&b.0));
        let warp_ranges = coalesce_warps(&warps);

        let mut seq = 0usize;
        let mut events = Vec::with_capacity(
            bpms.len().saturating_sub(1)
                + stops.len() * 2
                + delays.len() * 2
                + warp_ranges.len() * 2,
        );
        for (beat, bpm) in bpms.into_iter().skip(1) {
            events.push(PyEvent {
                beat,
                value: bpm,
                tag: PyEventTag::Bpm,
                seq,
            });
            seq += 1;
        }
        for (beat, value) in delays {
            events.push(PyEvent {
                beat,
                value,
                tag: PyEventTag::Delay,
                seq,
            });
            seq += 1;
            events.push(PyEvent {
                beat,
                value,
                tag: PyEventTag::DelayEnd,
                seq,
            });
            seq += 1;
        }
        for (beat, value) in stops {
            events.push(PyEvent {
                beat,
                value,
                tag: PyEventTag::Stop,
                seq,
            });
            seq += 1;
            events.push(PyEvent {
                beat,
                value,
                tag: PyEventTag::StopEnd,
                seq,
            });
            seq += 1;
        }
        for (start, end) in warp_ranges {
            events.push(PyEvent {
                beat: start,
                value: 0.0,
                tag: PyEventTag::Warp,
                seq,
            });
            seq += 1;
            events.push(PyEvent {
                beat: end,
                value: 0.0,
                tag: PyEventTag::WarpEnd,
                seq,
            });
            seq += 1;
        }
        events.sort_by(|a, b| {
            a.beat
                .total_cmp(&b.beat)
                .then_with(|| a.tag.cmp(&b.tag))
                .then_with(|| a.seq.cmp(&b.seq))
        });

        let mut states = Vec::with_capacity(events.len() + 1);
        let mut last = PyTimingState {
            beat: 0.0,
            value: first_bpm.1,
            tag: PyEventTag::Bpm,
            time: -offset_sec,
            bpm: first_bpm.1,
            warp: false,
        };
        states.push(last);

        for event in events {
            let dt = py_time_until(last, event.beat, event.tag);
            let time = last.time + dt;
            let bpm = if event.tag == PyEventTag::Bpm {
                event.value
            } else {
                last.bpm
            };
            let warp = match event.tag {
                PyEventTag::Warp => true,
                PyEventTag::WarpEnd => false,
                _ => last.warp,
            };
            last = PyTimingState {
                beat: event.beat,
                value: event.value,
                tag: event.tag,
                time,
                bpm,
                warp,
            };
            states.push(last);
        }

        Ok(Self { states })
    }

    fn time_at_stop(&self, beat: f64) -> f64 {
        let idx = bisect_tagged_beats(&self.states, beat, PyEventTag::Stop).saturating_sub(1);
        let prior = self.states[idx];
        prior.time + py_time_until(prior, beat, PyEventTag::Stop)
    }
}

fn parse_beat_values(raw: &str) -> Vec<(f64, f64)> {
    raw.trim()
        .split(',')
        .filter_map(|part| {
            let (beat_s, value_s) = part.trim().split_once('=')?;
            let beat = beat_s.trim().parse::<f64>().ok()?;
            let value = value_s.trim().parse::<f64>().ok()?;
            (beat.is_finite() && value.is_finite()).then_some((beat, value))
        })
        .collect()
}

fn coalesce_warps(warps: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for (beat, value) in warps {
        let end = beat + value;
        if let Some((_, last_end)) = out.last_mut() {
            if *beat <= *last_end {
                if end > *last_end {
                    *last_end = end;
                }
                continue;
            }
        }
        out.push((*beat, end));
    }
    out
}

fn bisect_tagged_beats(states: &[PyTimingState], beat: f64, tag: PyEventTag) -> usize {
    let mut lo = 0usize;
    let mut hi = states.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        let s = states[mid];
        if s.beat < beat || (s.beat == beat && s.tag <= tag) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

fn py_time_until(state: PyTimingState, beat: f64, event_tag: PyEventTag) -> f64 {
    let mut time_until = if state.warp {
        0.0
    } else {
        (beat - state.beat) * 60.0 / state.bpm
    };
    let from_pause = matches!(state.tag, PyEventTag::Stop | PyEventTag::Delay);
    let to_end = matches!(event_tag, PyEventTag::StopEnd | PyEventTag::DelayEnd);
    if from_pause && to_end {
        time_until += state.value;
    }
    time_until
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

fn baseline_candidates(root: &Path, md5: &str) -> (PathBuf, PathBuf) {
    let prefix = md5.get(0..2).unwrap_or("00");
    let shard = root.join(prefix);
    (
        shard.join(format!("{md5}.json")),
        shard.join(format!("{md5}.json.zst")),
    )
}

fn read_baseline(path: &Path) -> Result<Vec<u8>, String> {
    let raw =
        fs::read(path).map_err(|e| format!("read baseline {} failed: {e}", path.display()))?;
    let is_zst = path
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("zst"));
    if is_zst {
        zstd::stream::decode_all(Cursor::new(raw))
            .map_err(|e| format!("zstd decode {} failed: {e}", path.display()))
    } else {
        Ok(raw)
    }
}

fn parse_baseline(bytes: Vec<u8>) -> Result<BaselineFixture, String> {
    serde_json::from_slice::<BaselineFixture>(&bytes)
        .map_err(|e| format!("baseline json parse failed: {e}"))
}

fn build_report(root: &Path, baseline: &Path, cases: Vec<ParityCase>) -> ParityReport {
    let total = cases.len();
    let matched = count_status(&cases, "matched");
    let mismatched = count_status(&cases, "mismatch");
    let missing = count_status(&cases, "missing_baseline");
    let invalid = count_status(&cases, "invalid_baseline");
    let read_errors = count_status(&cases, "read_error");
    ParityReport {
        tool: "rnon".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        mode: "parity".to_string(),
        root_path: root.display().to_string(),
        baseline_path: baseline.display().to_string(),
        total_simfiles: total,
        matched,
        mismatched,
        missing_baseline: missing,
        invalid_baseline: invalid,
        read_errors,
        cases,
    }
}

fn count_status(cases: &[ParityCase], status: &str) -> usize {
    cases.iter().filter(|c| c.status == status).count()
}

fn simfile_ext(path: &Path) -> String {
    path.extension()
        .and_then(|s| s.to_str())
        .map_or_else(String::new, |s| s.to_ascii_lowercase())
}

#[derive(Debug, Deserialize)]
struct BaselineFixture {
    #[cfg(test)]
    #[serde(default)]
    simfile_path: Option<String>,
    #[serde(default)]
    music: String,
    #[serde(default)]
    params: BaselineParams,
    #[serde(default)]
    charts: Vec<BaselineChart>,
}

#[derive(Debug, Deserialize)]
struct BaselineParams {
    #[serde(default = "default_true")]
    consider_null: bool,
    #[serde(default = "default_true")]
    consider_p9ms: bool,
    #[serde(default = "default_fingerprint_ms")]
    fingerprint_ms: f64,
    #[serde(default)]
    full_spectrogram: bool,
    #[serde(default = "default_kernel_target")]
    kernel_target: String,
    #[serde(default = "default_kernel_type")]
    kernel_type: String,
    #[serde(default)]
    magic_offset_ms: f64,
    #[serde(default = "default_step_ms")]
    step_ms: f64,
    #[serde(default = "default_tolerance")]
    tolerance: f64,
    #[serde(default = "default_window_ms")]
    window_ms: f64,
}

impl Default for BaselineParams {
    fn default() -> Self {
        Self {
            consider_null: true,
            consider_p9ms: true,
            fingerprint_ms: default_fingerprint_ms(),
            full_spectrogram: false,
            kernel_target: default_kernel_target(),
            kernel_type: default_kernel_type(),
            magic_offset_ms: 0.0,
            step_ms: default_step_ms(),
            tolerance: default_tolerance(),
            window_ms: default_window_ms(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct BaselineChart {
    #[serde(default)]
    chart_index: Option<usize>,
    #[serde(default)]
    steps_type: Option<String>,
    #[serde(default)]
    difficulty: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    slot: Option<String>,
    #[serde(default)]
    slot_null: Option<String>,
    #[serde(default)]
    slot_p9ms: Option<String>,
    #[serde(default)]
    chart_has_own_timing: Option<bool>,
    #[serde(default)]
    music: Option<String>,
    #[serde(default)]
    sample_rate: Option<u32>,
    #[serde(default)]
    bias_ms: Option<f64>,
    #[serde(default)]
    confidence: Option<f64>,
    #[serde(default)]
    conv_quint: Option<f64>,
    #[serde(default)]
    conv_stdev: Option<f64>,
    #[serde(default)]
    paradigm: Option<String>,
}

#[derive(Clone)]
struct AudioCacheEntry {
    path: PathBuf,
    decode: Result<OggDecode, String>,
}

const fn default_true() -> bool {
    true
}

const fn default_fingerprint_ms() -> f64 {
    50.0
}

const fn default_step_ms() -> f64 {
    0.2
}

const fn default_window_ms() -> f64 {
    10.0
}

const fn default_tolerance() -> f64 {
    4.0
}

fn default_kernel_target() -> String {
    "digest".to_string()
}

fn default_kernel_type() -> String {
    "rising".to_string()
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::path::Path;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::audio::OggDecode;
    use crate::cli::ParityCmd;
    use crate::fs_scan::md5_hex;
    use walkdir::WalkDir;

    use super::{
        BaselineChart, chart_music_tag, compare_baseline_verbose, decode_cached_with,
        non_empty_trim, parse_baseline, read_baseline, run,
    };

    #[test]
    fn parity_matches_existing_baseline_file() {
        let temp = temp_root("parity-pass");
        let root = temp.join("packs");
        let song = root.join("PackA").join("SongA");
        fs::create_dir_all(&song).expect("mkdir song");
        let simfile = song.join("chart.sm");
        let bytes =
            b"#TITLE:Test;#BPMS:0.000=120.000;#NOTES:dance-single:desc:Easy:1:0,0,0,0:0000\n;";
        fs::write(&simfile, bytes).expect("write simfile");

        let md5 = md5_hex(bytes);
        let baseline = temp.join("baseline");
        let shard = baseline.join(&md5[0..2]);
        fs::create_dir_all(&shard).expect("mkdir shard");
        fs::write(shard.join(format!("{md5}.json")), "{}").expect("write baseline");

        let args = ParityCmd {
            root_path: PathBuf::from(&root),
            baseline_path: PathBuf::from(&baseline),
            output: None,
            fail_on_missing: true,
            fail_on_mismatch: true,
        };
        let report = run(&args).expect("run parity");
        assert_eq!(report.total_simfiles, 1);
        assert_eq!(report.matched, 1);
        assert_eq!(report.mismatched, 0);
        assert_eq!(report.missing_baseline, 0);
        assert_eq!(report.invalid_baseline, 0);
        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn parity_reports_missing_baseline() {
        let temp = temp_root("parity-missing");
        let root = temp.join("packs");
        let song = root.join("PackA").join("SongA");
        fs::create_dir_all(&song).expect("mkdir song");
        fs::write(song.join("chart.sm"), "#TITLE:Missing;").expect("write simfile");
        let baseline = temp.join("baseline");
        fs::create_dir_all(&baseline).expect("mkdir baseline");

        let args = ParityCmd {
            root_path: root,
            baseline_path: baseline,
            output: None,
            fail_on_missing: false,
            fail_on_mismatch: false,
        };
        let report = run(&args).expect("run parity");
        assert_eq!(report.total_simfiles, 1);
        assert_eq!(report.matched, 0);
        assert_eq!(report.missing_baseline, 1);
        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn chart_music_prefers_row_then_root_then_summary() {
        let row_with = BaselineChart {
            chart_index: Some(2),
            steps_type: None,
            difficulty: None,
            description: None,
            slot: None,
            slot_null: None,
            slot_p9ms: None,
            chart_has_own_timing: None,
            music: Some("split.ogg".to_string()),
            sample_rate: None,
            bias_ms: None,
            confidence: None,
            conv_quint: None,
            conv_stdev: None,
            paradigm: None,
        };
        let row_without = BaselineChart {
            chart_index: Some(3),
            steps_type: None,
            difficulty: None,
            description: None,
            slot: None,
            slot_null: None,
            slot_p9ms: None,
            chart_has_own_timing: None,
            music: None,
            sample_rate: None,
            bias_ms: None,
            confidence: None,
            conv_quint: None,
            conv_stdev: None,
            paradigm: None,
        };
        assert_eq!(
            chart_music_tag(&row_with, "base.ogg", Some("chart.ogg"), "summary.ogg"),
            Some("split.ogg")
        );
        assert_eq!(
            chart_music_tag(&row_without, "base.ogg", Some("chart.ogg"), "summary.ogg"),
            Some("chart.ogg")
        );
        assert_eq!(
            chart_music_tag(&row_without, "base.ogg", None, "summary.ogg"),
            Some("base.ogg")
        );
        assert_eq!(
            chart_music_tag(&row_without, " ", None, "summary.ogg"),
            Some("summary.ogg")
        );
    }

    #[test]
    fn decode_cache_hits_same_path_once() {
        let mut cache = Vec::new();
        let mut calls = 0usize;
        let mut fake = |_: &Path| -> Result<OggDecode, String> {
            calls += 1;
            Ok(OggDecode {
                sample_rate_hz: 44100,
                mono: vec![0.0],
            })
        };
        let p = Path::new("/tmp/same.ogg");
        let r1 = decode_cached_with(p, &mut cache, &mut fake);
        let r2 = decode_cached_with(p, &mut cache, &mut fake);
        let r3 = decode_cached_with(Path::new("/tmp/other.ogg"), &mut cache, &mut fake);
        assert!(r1.is_ok());
        assert!(r2.is_ok());
        assert!(r3.is_ok());
        assert_eq!(calls, 2);
    }

    #[test]
    fn parity_detects_metadata_mismatch() {
        let temp = temp_root("parity-meta");
        let root = temp.join("packs");
        let song = root.join("PackA").join("SongA");
        fs::create_dir_all(&song).expect("mkdir song");
        let simfile = song.join("chart.sm");
        let bytes =
            b"#TITLE:Meta;#BPMS:0.000=120.000;#NOTES:dance-single:desc:Easy:1:0,0,0,0:0000\n;";
        fs::write(&simfile, bytes).expect("write simfile");

        let md5 = md5_hex(bytes);
        let baseline = temp.join("baseline");
        let shard = baseline.join(&md5[0..2]);
        fs::create_dir_all(&shard).expect("mkdir shard");
        fs::write(
            shard.join(format!("{md5}.json")),
            r#"{"charts":[{"chart_index":0,"steps_type":"dance-double"}]}"#,
        )
        .expect("write baseline");

        let args = ParityCmd {
            root_path: PathBuf::from(&root),
            baseline_path: PathBuf::from(&baseline),
            output: None,
            fail_on_missing: false,
            fail_on_mismatch: false,
        };
        let report = run(&args).expect("run parity");
        assert_eq!(report.total_simfiles, 1);
        assert_eq!(report.mismatched, 1);
        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    #[ignore = "slow parity sweep over all baseline fixtures; requires local simfile paths to exist"]
    fn parity_matches_all_available_baselines() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let baseline_root = manifest_dir.join("tests/baseline");
        assert!(
            baseline_root.exists(),
            "baseline directory missing: {}",
            baseline_root.display()
        );

        let mut fixtures = WalkDir::new(&baseline_root)
            .into_iter()
            .filter_map(Result::ok)
            .map(|entry| entry.into_path())
            .filter(|path| is_baseline_fixture(path))
            .collect::<Vec<_>>();
        fixtures.sort();

        let mut checked = 0usize;
        let mut skipped = 0usize;
        let mut failed = Vec::new();
        for fixture_path in fixtures {
            let rel = fixture_path
                .strip_prefix(&baseline_root)
                .unwrap_or(&fixture_path)
                .display()
                .to_string();
            let baseline = match read_baseline(&fixture_path).and_then(parse_baseline) {
                Ok(v) => v,
                Err(err) => {
                    failed.push(format!("{rel}: parse failed: {err}"));
                    continue;
                }
            };
            let Some(simfile_raw) = baseline.simfile_path.as_deref().and_then(non_empty_trim)
            else {
                skipped += 1;
                continue;
            };
            let simfile_path = PathBuf::from(simfile_raw);
            if !simfile_path.exists() {
                skipped += 1;
                continue;
            }
            let simfile_bytes = match fs::read(&simfile_path) {
                Ok(bytes) => bytes,
                Err(err) => {
                    failed.push(format!("{rel}: read simfile failed: {err}"));
                    continue;
                }
            };
            checked += 1;
            match compare_baseline_verbose(&simfile_path, &simfile_bytes, &baseline) {
                Ok(outcome) => {
                    for line in outcome.logs {
                        println!("{line}");
                    }
                    if outcome.mismatches.is_empty() {
                        println!("  result: ....ok");
                    } else {
                        println!("  result: ....MISMATCH");
                        for msg in &outcome.mismatches {
                            println!("    {msg}");
                        }
                        failed.push(format!(
                            "{rel}: mismatch: {}",
                            outcome.mismatches.join("; ")
                        ));
                    }
                }
                Err(err) => failed.push(format!("{rel}: compare failed: {err}")),
            }
        }

        println!(
            "baseline parity sweep: checked={checked} skipped={skipped} failed={}",
            failed.len()
        );
        assert!(
            checked > 0,
            "no baseline fixtures could be checked; ensure baseline simfile_path values exist locally"
        );
        assert!(
            failed.is_empty(),
            "baseline parity sweep failures (showing up to 20):\n{}",
            failed.into_iter().take(20).collect::<Vec<_>>().join("\n")
        );
    }

    fn is_baseline_fixture(path: &Path) -> bool {
        if !path.is_file() {
            return false;
        }
        if path
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("zst"))
        {
            return true;
        }
        path.extension()
            .and_then(|s| s.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
    }

    fn temp_root(tag: &str) -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_millis();
        let path = env::temp_dir().join(format!("rnon-{tag}-{ts}-{}", std::process::id()));
        fs::create_dir_all(&path).expect("mkdir temp root");
        path
    }
}
