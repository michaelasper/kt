use git2::{Repository, Status};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitError {
    #[error("not a git repository: {0}")]
    NotARepository(String),
    #[error("git2 error: {0}")]
    Git2(#[from] git2::Error),
    #[error("no current branch (detached HEAD)")]
    DetachedHead,
}

#[derive(Debug, Clone)]
pub struct GitInfo {
    pub branch: Option<String>,
    pub commit_sha: Option<String>,
    pub is_dirty: bool,
    pub changed_files: Vec<ChangedFile>,
}

#[derive(Debug, Clone)]
pub struct ChangedFile {
    pub path: String,
    pub status: FileStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Other(String),
}

impl std::fmt::Display for FileStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileStatus::Modified => write!(f, "modified"),
            FileStatus::Added => write!(f, "added"),
            FileStatus::Deleted => write!(f, "deleted"),
            FileStatus::Renamed => write!(f, "renamed"),
            FileStatus::Untracked => write!(f, "untracked"),
            FileStatus::Other(s) => write!(f, "{}", s),
        }
    }
}

pub fn get_git_info(directory: &Path) -> Result<GitInfo, GitError> {
    let repo = Repository::discover(directory)
        .map_err(|_| GitError::NotARepository(directory.display().to_string()))?;

    let head = repo.head()?;
    let branch = head.shorthand().map(|s| s.to_string());
    let commit_sha = head.target().map(|oid| format!("{oid}"));

    let mut changed_files = Vec::new();
    let is_dirty = if let Ok(statuses) = repo.statuses(None) {
        for entry in statuses.iter() {
            let status = entry.status();
            if let Some(path) = entry.path() {
                let file_status = git_status_to_file_status(status);
                changed_files.push(ChangedFile {
                    path: path.to_string(),
                    status: file_status.clone(),
                });
            }
        }
        !changed_files.is_empty()
    } else {
        false
    };

    Ok(GitInfo {
        branch,
        commit_sha,
        is_dirty,
        changed_files,
    })
}

pub fn get_diff_files(directory: &Path, base_branch: &str) -> Result<Vec<String>, GitError> {
    let repo = Repository::discover(directory)
        .map_err(|_| GitError::NotARepository(directory.display().to_string()))?;

    let base = repo.find_branch(base_branch, git2::BranchType::Local)?;
    let base_oid = base.get().target().ok_or(GitError::DetachedHead)?;

    let head_oid = repo.head()?.target().ok_or(GitError::DetachedHead)?;
    let base_commit = repo.find_commit(base_oid)?;
    let head_commit = repo.find_commit(head_oid)?;

    let diff =
        repo.diff_tree_to_tree(Some(&base_commit.tree()?), Some(&head_commit.tree()?), None)?;

    let mut changed_files = Vec::new();
    diff.foreach(
        &mut |delta, _progress| {
            if let Some(path) = delta.new_file().path() {
                changed_files.push(path.to_string_lossy().to_string());
            } else if let Some(path) = delta.old_file().path() {
                changed_files.push(path.to_string_lossy().to_string());
            }
            true
        },
        None,
        None,
        None,
    )?;

    Ok(changed_files)
}

fn git_status_to_file_status(status: Status) -> FileStatus {
    if status.is_index_new() {
        FileStatus::Added
    } else if status.is_wt_new() {
        FileStatus::Untracked
    } else if status.is_index_deleted() || status.is_wt_deleted() {
        FileStatus::Deleted
    } else if status.is_index_renamed() || status.is_wt_renamed() {
        FileStatus::Renamed
    } else if status.is_index_modified() || status.is_wt_modified() {
        FileStatus::Modified
    } else {
        FileStatus::Other(format!("{status:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{BranchType, Oid, Signature};
    use std::fs::{self, write};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn make_temp_repo_path() -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock valid")
            .as_nanos();
        let pid = std::process::id();
        path.push(format!("kt-git-tests-{}-{}", pid, nanos));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn commit_file(repo: &Repository, relative_path: &str, contents: &str, message: &str) -> Oid {
        let repo_path = repo.workdir().expect("repository has working directory");
        let file_path = repo_path.join(relative_path);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        write(file_path, contents).unwrap();

        let mut index = repo.index().unwrap();
        index.add_path(Path::new(relative_path)).unwrap();
        index.write().unwrap();

        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = Signature::now("kt", "kt@example.com").unwrap();

        let parent = repo
            .head()
            .ok()
            .and_then(|h| h.target())
            .and_then(|oid| repo.find_commit(oid).ok());

        let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .unwrap()
    }

    fn with_temp_repo<F>(f: F)
    where
        F: FnOnce(&Repository),
    {
        let path = make_temp_repo_path();
        let repo = Repository::init(&path).unwrap();
        let repo_path = path.clone();
        f(&repo);
        fs::remove_dir_all(repo_path).unwrap();
    }

    #[test]
    fn test_get_git_info_reports_branch_and_status() {
        with_temp_repo(|repo| {
            let repo_path = repo.workdir().unwrap();
            commit_file(repo, "initial.rs", "pub fn base() {}\n", "initial");

            let clean = get_git_info(repo_path).unwrap();
            assert!(clean.branch.is_some());
            assert!(clean.commit_sha.is_some());
            assert!(!clean.is_dirty);

            write(repo_path.join("initial.rs"), "pub fn changed() {}\n").unwrap();
            let dirty = get_git_info(repo_path).unwrap();
            assert!(dirty.is_dirty);
            assert!(!dirty.changed_files.is_empty());
            assert!(dirty
                .changed_files
                .iter()
                .any(|f| f.path == "initial.rs" && f.status == FileStatus::Modified));
        });
    }

    #[test]
    fn test_get_diff_files_detects_changes_vs_base_branch() {
        with_temp_repo(|repo| {
            let repo_path = repo.workdir().unwrap();
            commit_file(repo, "base.rs", "pub fn base() {}\n", "base");

            let base_commit = repo.head().unwrap().peel_to_commit().unwrap();
            if repo.find_branch("main", BranchType::Local).is_err() {
                repo.branch("main", &base_commit, false).unwrap();
            }

            if repo.find_branch("feature", BranchType::Local).is_err() {
                repo.branch("feature", &base_commit, false).unwrap();
            }
            repo.set_head("refs/heads/feature").unwrap();

            commit_file(repo, "feature.rs", "pub fn feature() {}\n", "feature");

            let changed = get_diff_files(repo_path, "main").unwrap();
            assert_eq!(changed, vec!["feature.rs".to_string()]);
        });
    }
}
