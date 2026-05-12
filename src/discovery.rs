use crate::Language;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub const DEFAULT_EXCLUDE_PATTERNS: &[&str] = &[
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryOptions {
    exclude_patterns: Vec<String>,
}

impl DiscoveryOptions {
    pub fn new(exclude_patterns: Vec<String>) -> Self {
        let exclude_patterns = exclude_patterns
            .into_iter()
            .filter_map(|pattern| normalize_pattern(&pattern))
            .collect();
        Self { exclude_patterns }
    }

    pub fn exclude_patterns(&self) -> &[String] {
        &self.exclude_patterns
    }

    pub fn is_excluded_relative_path(&self, relative_path: &Path) -> bool {
        let relative_path = normalize_relative_path(relative_path);
        !relative_path.is_empty()
            && self
                .exclude_patterns
                .iter()
                .any(|pattern| pattern_matches_path(pattern, &relative_path))
    }
}

impl Default for DiscoveryOptions {
    fn default() -> Self {
        Self::new(default_exclude_patterns())
    }
}

pub fn default_exclude_patterns() -> Vec<String> {
    DEFAULT_EXCLUDE_PATTERNS
        .iter()
        .map(|pattern| (*pattern).to_string())
        .collect()
}

#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    pub path: PathBuf,
    pub relative_path: String,
    pub language: Language,
}

pub fn discover_files(root: &Path) -> Vec<DiscoveredFile> {
    discover_files_with_options(root, &DiscoveryOptions::default())
}

pub fn discover_files_with_options(root: &Path, options: &DiscoveryOptions) -> Vec<DiscoveredFile> {
    let root = match root.canonicalize() {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    let gitignore = GitIgnoreFilter::new(&root);

    WalkDir::new(&root)
        .into_iter()
        .filter_entry(|entry| {
            if gitignore
                .as_ref()
                .is_some_and(|filter| filter.is_ignored(entry.path()))
            {
                return false;
            }

            if entry.file_type().is_dir() {
                let relative_path = match entry.path().strip_prefix(&root) {
                    Ok(path) => path,
                    Err(_) => return true,
                };
                return !options.is_excluded_relative_path(relative_path);
            }
            true
        })
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.into_path();
            let relative_path = path.strip_prefix(&root).ok()?;
            if gitignore
                .as_ref()
                .is_some_and(|filter| filter.is_ignored(&path))
            {
                return None;
            }
            if options.is_excluded_relative_path(relative_path) {
                return None;
            }

            let ext = path.extension()?.to_str()?;

            let language = Language::from_extension(ext)?;
            let relative_path = relative_path.to_string_lossy().to_string();

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
    discover_modified_files_with_options(root, known_mtimes, &DiscoveryOptions::default())
}

pub fn discover_modified_files_with_options(
    root: &Path,
    known_mtimes: &std::collections::HashMap<String, String>,
    options: &DiscoveryOptions,
) -> Vec<DiscoveredFile> {
    let all_files = discover_files_with_options(root, options);
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

fn normalize_pattern(pattern: &str) -> Option<String> {
    let pattern = pattern.trim().replace('\\', "/");
    let pattern = pattern.trim_matches('/').trim();
    if pattern.is_empty() {
        None
    } else {
        Some(pattern.to_string())
    }
}

fn normalize_relative_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn pattern_matches_path(pattern: &str, relative_path: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return path_is_or_under(relative_path, prefix);
    }

    if pattern.contains('/') {
        return path_is_or_under(relative_path, pattern);
    }

    relative_path
        .split('/')
        .any(|component| component == pattern)
}

fn path_is_or_under(relative_path: &str, prefix: &str) -> bool {
    relative_path == prefix
        || relative_path
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

struct GitIgnoreFilter {
    repo: git2::Repository,
    workdir: PathBuf,
}

impl GitIgnoreFilter {
    fn new(root: &Path) -> Option<Self> {
        let repo = git2::Repository::discover(root).ok()?;
        let workdir = repo.workdir()?.canonicalize().ok()?;
        Some(Self { repo, workdir })
    }

    fn is_ignored(&self, path: &Path) -> bool {
        let Ok(relative_path) = path.strip_prefix(&self.workdir) else {
            return false;
        };
        self.repo
            .status_should_ignore(relative_path)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_files_uses_configured_exclude_patterns() {
        let temp = tempfile::tempdir().unwrap();
        let src_dir = temp.path().join("src");
        let generated_dir = temp.path().join("generated");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(&generated_dir).unwrap();
        std::fs::write(src_dir.join("lib.rs"), "fn kept() {}\n").unwrap();
        std::fs::write(generated_dir.join("ignored.rs"), "fn ignored() {}\n").unwrap();

        let options = DiscoveryOptions::new(vec!["generated".to_string()]);
        let files = discover_files_with_options(temp.path(), &options);

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "src/lib.rs");
    }

    #[test]
    fn discover_files_respects_gitignore() {
        let temp = tempfile::tempdir().unwrap();
        git2::Repository::init(temp.path()).unwrap();
        let src_dir = temp.path().join("src");
        let ignored_dir = temp.path().join("ignored");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(&ignored_dir).unwrap();
        std::fs::write(temp.path().join(".gitignore"), "ignored/\n").unwrap();
        std::fs::write(src_dir.join("lib.rs"), "fn kept() {}\n").unwrap();
        std::fs::write(ignored_dir.join("generated.rs"), "fn ignored() {}\n").unwrap();

        let files = discover_files(temp.path());

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "src/lib.rs");
    }
}
