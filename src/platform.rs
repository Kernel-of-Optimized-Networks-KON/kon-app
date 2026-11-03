//! The only module allowed to know about OS conventions. Paths are always `Path`/`PathBuf`, never strings.
use directories::ProjectDirs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Per-user application directories, resolved by OS convention.
pub struct AppDirs(ProjectDirs);

impl AppDirs {
    pub fn new() -> Option<Self> {
        ProjectDirs::from("org", "KON", "KON").map(Self)
    }
    pub fn config_dir(&self) -> &Path {
        self.0.config_dir()
    }
    pub fn data_dir(&self) -> &Path {
        self.0.data_dir()
    }
    pub fn cache_dir(&self) -> &Path {
        self.0.cache_dir()
    }
}

/// Native open/save dialogs. The trait is the seam: the GUI step supplies the implementation (likely `rfd`, which needs
/// GTK3 or xdg-desktop-portal on Linux). Nothing in core depends on a dialog being available.
pub trait FilePicker {
    fn pick_files(&self) -> Vec<PathBuf>;
    fn pick_folder(&self) -> Option<PathBuf>;
}

/// `<dir>/<input stem>.<ext>`, or `<stem> (2).<ext>` etc. if taken. Never overwrites an existing file.
pub fn unique_output_path(dir: &Path, input: &Path, ext: &str) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".into());
    let first = dir.join(format!("{stem}.{ext}"));
    if !first.exists() {
        return first;
    }
    (2u32..)
        .map(|n| dir.join(format!("{stem} ({n}).{ext}")))
        .find(|p| !p.exists())
        .expect("u32 range exhausted")
}

/// Write via a temp file in the destination directory, then rename into place, so a crash or cancel never leaves a
/// truncated output. Refuses to replace an existing file.
pub fn write_new_file(dest: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let tmp = dir.join(format!(".kon-{}.tmp", std::process::id()));
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        // rename() replaces silently on Unix but errors on Windows; this check makes behavior uniform.
        if dest.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "destination exists",
            ));
        }
        std::fs::rename(&tmp, dest)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_path_never_collides_and_handles_spaces_and_unicode() {
        let d = std::env::temp_dir().join(format!("kon-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let input = Path::new("some dir").join("фото 写真.png");
        let a = unique_output_path(&d, &input, "pkon");
        write_new_file(&a, b"1").unwrap();
        let b = unique_output_path(&d, &input, "pkon");
        assert_ne!(a, b);
        assert!(write_new_file(&a, b"2").is_err(), "must not overwrite");
        assert_eq!(std::fs::read(&a).unwrap(), b"1");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
