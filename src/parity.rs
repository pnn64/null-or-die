use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::cli::ParityCmd;
use crate::fs_scan::{baseline_rel_for_md5, discover_simfiles, md5_hex, rel_path};
use crate::model::{ParityCase, ParityReport};

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
            return ParityCase {
                simfile_rel,
                simfile_md5: String::new(),
                baseline_rel: None,
                status: "read_error".to_string(),
                error: Some(format!("read simfile failed: {err}")),
            };
        }
    };
    let digest = md5_hex(&bytes);
    let baseline_rel = baseline_rel_for_md5(&digest);
    let (candidate_json, candidate_zst) = baseline_candidates(baseline_root, &digest);
    if candidate_json.exists() {
        return load_baseline_file(&candidate_json, &simfile_rel, &digest, &baseline_rel);
    }
    if candidate_zst.exists() {
        return load_baseline_file(&candidate_zst, &simfile_rel, &digest, &baseline_rel);
    }
    ParityCase {
        simfile_rel,
        simfile_md5: digest,
        baseline_rel: Some(baseline_rel),
        status: "missing_baseline".to_string(),
        error: None,
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

fn load_baseline_file(
    path: &Path,
    simfile_rel: &str,
    digest: &str,
    baseline_rel: &str,
) -> ParityCase {
    match read_baseline(path).and_then(parse_baseline) {
        Ok(()) => ParityCase {
            simfile_rel: simfile_rel.to_string(),
            simfile_md5: digest.to_string(),
            baseline_rel: Some(baseline_rel.to_string()),
            status: "matched".to_string(),
            error: None,
        },
        Err(err) => ParityCase {
            simfile_rel: simfile_rel.to_string(),
            simfile_md5: digest.to_string(),
            baseline_rel: Some(baseline_rel.to_string()),
            status: "invalid_baseline".to_string(),
            error: Some(err),
        },
    }
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

fn parse_baseline(bytes: Vec<u8>) -> Result<(), String> {
    serde_json::from_slice::<BaselineFixture>(&bytes)
        .map(|_| ())
        .map_err(|e| format!("baseline json parse failed: {e}"))
}

fn build_report(root: &Path, baseline: &Path, cases: Vec<ParityCase>) -> ParityReport {
    let total = cases.len();
    let matched = count_status(&cases, "matched");
    let missing = count_status(&cases, "missing_baseline");
    let invalid = count_status(&cases, "invalid_baseline");
    let read_errors = count_status(&cases, "read_error");
    ParityReport {
        tool: "rnon".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        mode: "baseline-mapping".to_string(),
        root_path: root.display().to_string(),
        baseline_path: baseline.display().to_string(),
        total_simfiles: total,
        matched,
        missing_baseline: missing,
        invalid_baseline: invalid,
        read_errors,
        cases,
    }
}

fn count_status(cases: &[ParityCase], status: &str) -> usize {
    cases.iter().filter(|c| c.status == status).count()
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct BaselineFixture {
    #[serde(default)]
    charts: Vec<BaselineChart>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct BaselineChart {
    #[serde(default)]
    chart_index: Option<usize>,
    #[serde(default)]
    music: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::cli::ParityCmd;
    use crate::fs_scan::md5_hex;

    use super::run;

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
        };
        let report = run(&args).expect("run parity");
        assert_eq!(report.total_simfiles, 1);
        assert_eq!(report.matched, 1);
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
        };
        let report = run(&args).expect("run parity");
        assert_eq!(report.total_simfiles, 1);
        assert_eq!(report.matched, 0);
        assert_eq!(report.missing_baseline, 1);
        let _ = fs::remove_dir_all(temp);
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
