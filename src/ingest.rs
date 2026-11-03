//! Turns user-dropped paths (files and folders) into a flat, ordered list of files. Platform-independent.
use std::path::{Path, PathBuf};

/// Expand files and folders (recursively) into files. Directory symlinks are not followed, so cycles cannot occur.
/// Unreadable entries are skipped. Result is sorted per directory and de-duplicated.
pub fn scan(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in paths {
        walk(p, &mut out);
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out
}

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    let Ok(meta) = std::fs::symlink_metadata(p) else {
        return;
    };
    if meta.is_dir() {
        let Ok(rd) = std::fs::read_dir(p) else { return };
        let mut children: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        children.sort();
        for c in children {
            walk(&c, out);
        }
    } else if std::fs::metadata(p).is_ok_and(|m| m.is_file()) {
        out.push(p.to_path_buf());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_folders_recursively_and_dedups() {
        let d = std::env::temp_dir().join(format!("kon-ingest-{}", std::process::id()));
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("a.png"), b"1").unwrap();
        std::fs::write(d.join("sub").join("b.png"), b"2").unwrap();
        let got = scan(&[d.clone(), d.join("a.png"), d.join("missing")]);
        assert_eq!(got, vec![d.join("a.png"), d.join("sub").join("b.png")]);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
