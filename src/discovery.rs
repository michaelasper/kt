use crate::Language;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const IGNORED_DIRS: &[&str] = &[
    "target",
    "vendor",
    ".git",
    "node_modules",
    "__pycache__",
    ".idea",
    ".vscode",
    "build",
    "dist",
    ".next",
    ".cache",
    "pkg",
    "bin",
];

#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    pub path: PathBuf,
    pub relative_path: String,
    pub language: Language,
}

pub fn discover_files(root: &Path) -> Vec<DiscoveredFile> {
    let root = match root.canonicalize() {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };

    WalkDir::new(&root)
        .into_iter()
        .filter_entry(|entry| {
            if entry.file_type().is_dir() {
                let name = entry.file_name().to_string_lossy();
                return !IGNORED_DIRS.contains(&name.as_ref());
            }
            true
        })
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.into_path();
            let ext = path.extension()?.to_str()?;

            let language = Language::from_extension(ext)?;
            let relative_path = path.strip_prefix(&root).ok()?.to_string_lossy().to_string();

            Some(DiscoveredFile {
                path,
                relative_path,
                language,
            })
        })
        .collect()
}

pub fn discover_modified_files(
    root: &Path,
    known_mtimes: &std::collections::HashMap<String, String>,
) -> Vec<DiscoveredFile> {
    let all_files = discover_files(root);
    all_files
        .into_iter()
        .filter(|f| {
            let known_mtime = known_mtimes.get(&f.relative_path);
            match known_mtime {
                None => true,
                Some(stored) => {
                    let current_mtime = get_file_mtime(&f.path);
                    current_mtime.as_deref() != Some(stored.as_str())
                }
            }
        })
        .collect()
}

pub fn get_file_mtime(path: &Path) -> Option<String> {
    use std::time::UNIX_EPOCH;
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let duration = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(duration.as_millis().to_string())
}
