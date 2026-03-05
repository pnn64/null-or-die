use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum KernelTarget {
    Digest,
    Accumulator,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum BiasKernel {
    Rising,
    Loudest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzeParams {
    pub root_path: String,
    pub report_path: String,
    pub consider_null: bool,
    pub consider_p9ms: bool,
    pub tolerance: f64,
    pub confidence_limit: f64,
    pub fingerprint_ms: f64,
    pub window_ms: f64,
    pub step_ms: f64,
    pub magic_offset_ms: f64,
    pub kernel_target: KernelTarget,
    pub kernel_type: BiasKernel,
    pub full_spectrogram: bool,
    pub to_paradigm: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChartScan {
    pub chart_index: usize,
    pub steps_type: String,
    pub difficulty: String,
    pub description: String,
    pub slot_null: String,
    pub slot_p9ms: String,
    pub chart_has_own_timing: bool,
    pub status: String,
    pub bias_ms: Option<f64>,
    pub confidence: Option<f64>,
    pub conv_quint: Option<f64>,
    pub conv_stdev: Option<f64>,
    pub paradigm: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimfileScan {
    pub simfile_path: String,
    pub simfile_rel: String,
    pub simfile_md5: String,
    pub extension: String,
    pub status: String,
    pub error: Option<String>,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub artist: Option<String>,
    pub offset_seconds: Option<f64>,
    pub music_tag: Option<String>,
    pub charts: Vec<ChartScan>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzeReport {
    pub tool: String,
    pub version: String,
    pub mode: String,
    pub params: AnalyzeParams,
    pub simfile_count: usize,
    pub simfiles: Vec<SimfileScan>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParityCase {
    pub simfile_rel: String,
    pub simfile_md5: String,
    pub baseline_rel: Option<String>,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParityReport {
    pub tool: String,
    pub version: String,
    pub mode: String,
    pub root_path: String,
    pub baseline_path: String,
    pub total_simfiles: usize,
    pub matched: usize,
    pub mismatched: usize,
    pub missing_baseline: usize,
    pub invalid_baseline: usize,
    pub read_errors: usize,
    pub cases: Vec<ParityCase>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessCase {
    pub simfile_rel: String,
    pub simfile_md5: String,
    pub baseline_rel: String,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessReport {
    pub tool: String,
    pub version: String,
    pub mode: String,
    pub root_path: String,
    pub baseline_path: String,
    pub python_bin: String,
    pub source_root: Option<String>,
    pub scratch_path: String,
    pub total_simfiles: usize,
    pub written: usize,
    pub skipped_existing: usize,
    pub failed: usize,
    pub cases: Vec<HarnessCase>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlotReport {
    pub tool: String,
    pub version: String,
    pub input_json: String,
    pub output_png: String,
    pub width: u32,
    pub height: u32,
    pub span_ms: f64,
    pub bias_count: usize,
}
