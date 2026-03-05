mod analyze;
mod cli;
mod compat;
mod fs_scan;
mod harness;
mod model;
mod parity;
mod plot;

use std::fs;
use std::path::Path;

use clap::Parser;
use serde::Serialize;

pub fn run() -> Result<(), String> {
    let cli = cli::Cli::parse();
    match cli.command {
        cli::Command::Analyze(args) => {
            let report = analyze::run(&args)?;
            write_json(&report, args.output.as_deref())
        }
        cli::Command::Parity(args) => {
            let report = parity::run(&args)?;
            write_json(&report, args.output.as_deref())?;
            if args.fail_on_missing && report.missing_baseline > 0 {
                Err(format!(
                    "missing {} baseline fixture(s)",
                    report.missing_baseline
                ))
            } else {
                Ok(())
            }
        }
        cli::Command::Harness(args) => {
            let report = harness::run(&args)?;
            write_json(&report, args.output.as_deref())
        }
        cli::Command::Plot(args) => {
            let report = plot::run(&args)?;
            write_json(&report, None)
        }
    }
}

fn write_json<T: Serialize>(value: &T, out_path: Option<&Path>) -> Result<(), String> {
    let json =
        serde_json::to_string_pretty(value).map_err(|e| format!("serialize json failed: {e}"))?;
    if let Some(path) = out_path {
        fs::write(path, json).map_err(|e| format!("write {} failed: {e}", path.display()))
    } else {
        println!("{json}");
        Ok(())
    }
}

pub use compat::{guess_paradigm, slot_abbreviation, slot_expansion};
