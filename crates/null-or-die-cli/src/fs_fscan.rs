use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

pub fn discover_simfiles(root: &Path) -> Result<Vec<PathBuf>, String> {
    if root.is_file() {
        if is_simfile_path(root) {
            return Ok(vec![root.to_path_buf()]);
        }
        return Err(format!("not a simfile: {}", root.display()));
    }
    if !root.is_dir() {
        return Err(format!("root path does not exist: {}", root.display()));
    }
    let mut paths = collect_simfile_paths(root);
    paths.sort();
    Ok(pick_by_directory(paths))
}

pub fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .ok()
        .map_or_else(|| path.display().to_string(), |p| p.display().to_string())
}

pub fn md5_hex(bytes: &[u8]) -> String {
    format!("{:x}", md5::compute(bytes))
}

pub fn baseline_rel_for_md5(md5: &str) -> String {
    let prefix = md5.get(0..2).unwrap_or("00");
    format!("{prefix}/{md5}.json")
}

fn collect_simfile_paths(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
        let path = entry.path();
        if path.is_file() && is_simfile_path(path) {
            out.push(path.to_path_buf());
        }
    }
    out
}

fn pick_by_directory(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut picks: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    for path in paths {
        let Some(dir) = path.parent() else {
            continue;
        };
        let entry = picks
            .entry(dir.to_path_buf())
            .or_insert_with(|| path.clone());
        if has_ext(entry, "sm") && has_ext(&path, "ssc") {
            *entry = path;
        }
    }
    picks.into_values().collect()
}

fn is_simfile_path(path: &Path) -> bool {
    has_ext(path, "sm") || has_ext(path, "ssc")
}

fn has_ext(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case(ext))
}

#[cfg(test)]
mod tests {
    use super::baseline_rel_for_md5;

    #[test]
    fn baseline_md5_shard_path_is_stable() {
        let rel = baseline_rel_for_md5("abcdef0123456789abcdef0123456789");
        assert_eq!(rel, "ab/abcdef0123456789abcdef0123456789.json");
    }
}
