use std::path::PathBuf;

use clap::{ArgAction, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "rnon")]
#[command(about = "Rust reverse-engineering helper for nine-or-null parity", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Analyze(AnalyzeCmd),
    Parity(ParityCmd),
    Harness(HarnessCmd),
    Bench(BenchCmd),
    Plot(PlotCmd),
}

#[derive(Debug, Parser)]
pub struct AnalyzeCmd {
    pub root_path: PathBuf,

    #[arg(short, long)]
    pub report_path: Option<PathBuf>,

    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,

    #[arg(long)]
    pub to_paradigm: Option<String>,

    #[arg(long = "consider-null", action = ArgAction::SetFalse, default_value_t = true)]
    pub consider_null: bool,

    #[arg(long = "consider-p9ms", action = ArgAction::SetFalse, default_value_t = true)]
    pub consider_p9ms: bool,

    #[arg(short = 't', long, default_value_t = 4.0)]
    pub tolerance: f64,

    #[arg(short = 'c', long = "confidence", default_value_t = 0.80)]
    pub confidence_limit: f64,

    #[arg(short = 'f', long = "fingerprint", default_value_t = 50.0)]
    pub fingerprint_ms: f64,

    #[arg(short = 'w', long = "window", default_value_t = 10.0)]
    pub window_ms: f64,

    #[arg(short = 's', long = "step", default_value_t = 0.2)]
    pub step_ms: f64,

    #[arg(long = "magic-offset", default_value_t = 0.0)]
    pub magic_offset_ms: f64,

    #[arg(long = "kernel-target", default_value = "digest")]
    pub kernel_target: String,

    #[arg(long = "kernel-type", default_value = "rising")]
    pub kernel_type: String,

    #[arg(long = "full-spectrogram")]
    pub full_spectrogram: bool,
}

#[derive(Debug, Parser)]
pub struct ParityCmd {
    pub root_path: PathBuf,

    #[arg(short = 'b', long = "baseline", alias = "baseline-path")]
    pub baseline_path: PathBuf,

    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,

    #[arg(long = "fail-on-missing")]
    pub fail_on_missing: bool,

    #[arg(long = "fail-on-mismatch")]
    pub fail_on_mismatch: bool,
}

#[derive(Debug, Parser)]
pub struct HarnessCmd {
    pub root_path: PathBuf,

    #[arg(short = 'b', long = "baseline", alias = "baseline-path")]
    pub baseline_path: PathBuf,

    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,

    #[arg(long = "python", default_value = "python3")]
    pub python_bin: String,

    #[arg(long = "source-root")]
    pub source_root: Option<PathBuf>,

    #[arg(long = "scratch")]
    pub scratch_path: Option<PathBuf>,

    #[arg(long = "keep-scratch")]
    pub keep_scratch: bool,

    #[arg(long = "overwrite")]
    pub overwrite: bool,

    #[arg(long = "zstd-level", default_value_t = 19)]
    pub zstd_level: i32,

    #[arg(long = "consider-null", action = ArgAction::SetFalse, default_value_t = true)]
    pub consider_null: bool,

    #[arg(long = "consider-p9ms", action = ArgAction::SetFalse, default_value_t = true)]
    pub consider_p9ms: bool,

    #[arg(short = 't', long, default_value_t = 4.0)]
    pub tolerance: f64,

    #[arg(short = 'f', long = "fingerprint", default_value_t = 50.0)]
    pub fingerprint_ms: f64,

    #[arg(short = 'w', long = "window", default_value_t = 10.0)]
    pub window_ms: f64,

    #[arg(short = 's', long = "step", default_value_t = 0.2)]
    pub step_ms: f64,

    #[arg(long = "magic-offset", default_value_t = 0.0)]
    pub magic_offset_ms: f64,

    #[arg(long = "kernel-target", default_value = "digest")]
    pub kernel_target: String,

    #[arg(long = "kernel-type", default_value = "rising")]
    pub kernel_type: String,

    #[arg(long = "full-spectrogram")]
    pub full_spectrogram: bool,
}

#[derive(Debug, Parser)]
pub struct PlotCmd {
    pub input_json: PathBuf,
    pub output_png: PathBuf,

    #[arg(long, default_value_t = 1024)]
    pub width: u32,

    #[arg(long, default_value_t = 256)]
    pub height: u32,

    #[arg(long, default_value_t = 50.0)]
    pub span_ms: f64,
}

#[derive(Debug, Parser)]
pub struct BenchCmd {
    pub simfile_path: PathBuf,

    #[arg(short = 'n', long = "iterations", default_value_t = 20)]
    pub iterations: usize,

    #[arg(long = "warmup", default_value_t = 3)]
    pub warmup: usize,

    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,

    #[arg(short = 'f', long = "fingerprint", default_value_t = 50.0)]
    pub fingerprint_ms: f64,

    #[arg(short = 'w', long = "window", default_value_t = 10.0)]
    pub window_ms: f64,

    #[arg(short = 's', long = "step", default_value_t = 0.2)]
    pub step_ms: f64,

    #[arg(long = "magic-offset", default_value_t = 0.0)]
    pub magic_offset_ms: f64,

    #[arg(long = "kernel-target", default_value = "digest")]
    pub kernel_target: String,

    #[arg(long = "kernel-type", default_value = "rising")]
    pub kernel_type: String,

    #[arg(long = "full-spectrogram")]
    pub full_spectrogram: bool,
}
