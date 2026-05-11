use git2::{ErrorCode, Repository, Status};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitError {
    #[error("not a git repository: {0}")]
    NotARepository(String),
    #[error("base ref not found: {base_ref}")]
    BaseRefNotFound { base_ref: String },
    #[error("base ref does not resolve to a commit: {base_ref}")]
    BaseRefNotCommit { base_ref: String },
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

fn resolve_base_commit<'repo>(
    repo: &'repo Repository,
    base_ref: &str,
) -> Result<git2::Commit<'repo>, GitError> {
    if let Ok(oid) = git2::Oid::from_str(base_ref) {
        return repo
            .find_object(oid, None)
            .map_err(|err| {
                if is_ref_resolution_miss(&err) {
                    GitError::BaseRefNotFound {
                        base_ref: base_ref.to_string(),
                    }
                } else {
                    GitError::Git2(err)
                }
            })
            .and_then(|object| peel_to_commit(object, base_ref));
    }

    if let Some(commit) = resolve_branch_commit(repo, base_ref)? {
        return Ok(commit);
    }

    for candidate in base_ref_candidates(base_ref) {
        match repo.revparse_single(&candidate) {
            Ok(object) => return peel_to_commit(object, base_ref),
            Err(err) if is_ref_resolution_miss(&err) => {}
            Err(err) => return Err(GitError::Git2(err)),
        }
    }

    Err(GitError::BaseRefNotFound {
        base_ref: base_ref.to_string(),
    })
}

fn resolve_branch_commit<'repo>(
    repo: &'repo Repository,
    base_ref: &str,
) -> Result<Option<git2::Commit<'repo>>, GitError> {
    if let Some(branch_name) = base_ref.strip_prefix("refs/heads/") {
        return find_branch_commit(repo, branch_name, git2::BranchType::Local, base_ref);
    }

    if let Some(branch_name) = base_ref.strip_prefix("refs/remotes/") {
        return find_branch_commit(repo, branch_name, git2::BranchType::Remote, base_ref);
    }

    if base_ref.starts_with("origin/") {
        return find_branch_commit(repo, base_ref, git2::BranchType::Remote, base_ref);
    }

    if !base_ref.starts_with("refs/") {
        if let Some(commit) = find_branch_commit(repo, base_ref, git2::BranchType::Local, base_ref)?
        {
            return Ok(Some(commit));
        }

        return find_branch_commit(
            repo,
            &format!("origin/{base_ref}"),
            git2::BranchType::Remote,
            base_ref,
        );
    }

    Ok(None)
}

fn find_branch_commit<'repo>(
    repo: &'repo Repository,
    branch_name: &str,
    branch_type: git2::BranchType,
    base_ref: &str,
) -> Result<Option<git2::Commit<'repo>>, GitError> {
    match repo.find_branch(branch_name, branch_type) {
        Ok(branch) => {
            branch
                .get()
                .peel_to_commit()
                .map(Some)
                .map_err(|_| GitError::BaseRefNotCommit {
                    base_ref: base_ref.to_string(),
                })
        }
        Err(err) if is_ref_resolution_miss(&err) => Ok(None),
        Err(err) => Err(GitError::Git2(err)),
    }
}

fn peel_to_commit<'repo>(
    object: git2::Object<'repo>,
    base_ref: &str,
) -> Result<git2::Commit<'repo>, GitError> {
    object
        .peel_to_commit()
        .map_err(|_| GitError::BaseRefNotCommit {
            base_ref: base_ref.to_string(),
        })
}

fn base_ref_candidates(base_ref: &str) -> Vec<String> {
    let mut candidates = vec![base_ref.to_string()];

    if !base_ref.starts_with("refs/") && !base_ref.starts_with("origin/") {
        candidates.push(format!("origin/{base_ref}"));
        candidates.push(format!("refs/remotes/origin/{base_ref}"));
    }

    candidates
}

fn is_ref_resolution_miss(err: &git2::Error) -> bool {
    matches!(err.code(), ErrorCode::NotFound | ErrorCode::InvalidSpec)
}

pub fn get_diff_files(directory: &Path, base_ref: &str) -> Result<Vec<String>, GitError> {
    let repo = Repository::discover(directory)
        .map_err(|_| GitError::NotARepository(directory.display().to_string()))?;

    let head_oid = repo.head()?.target().ok_or(GitError::DetachedHead)?;
    let base_commit = resolve_base_commit(&repo, base_ref)?;
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
        let temp_dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(temp_dir.path()).unwrap();
        f(&repo);
    }

    fn create_origin_main(repo: &Repository, commit: &git2::Commit<'_>) {
        repo.reference(
            "refs/remotes/origin/main",
            commit.id(),
            true,
            "create test origin/main",
        )
        .unwrap();
    }

    fn create_lightweight_tag(repo: &Repository, name: &str, commit: &git2::Commit<'_>) {
        let object = repo.find_object(commit.id(), None).unwrap();
        repo.tag_lightweight(name, &object, false).unwrap();
    }

    fn switch_to_feature_branch(repo: &Repository, base_commit: &git2::Commit<'_>) {
        if repo.find_branch("feature", BranchType::Local).is_err() {
            repo.branch("feature", base_commit, false).unwrap();
        }
        repo.set_head("refs/heads/feature").unwrap();
    }

    fn delete_local_branch_if_exists(repo: &Repository, name: &str) {
        if let Ok(mut branch) = repo.find_branch(name, BranchType::Local) {
            branch.delete().unwrap();
        }
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

    #[test]
    fn test_get_diff_files_prefers_local_branch_over_same_named_tag() {
        with_temp_repo(|repo| {
            let repo_path = repo.workdir().unwrap();
            commit_file(repo, "base.rs", "pub fn base() {}\n", "base");

            let base_commit = repo.head().unwrap().peel_to_commit().unwrap();
            if repo.find_branch("main", BranchType::Local).is_err() {
                repo.branch("main", &base_commit, false).unwrap();
            }
            switch_to_feature_branch(repo, &base_commit);

            commit_file(repo, "tagged.rs", "pub fn tagged() {}\n", "tagged");
            let tag_commit = repo.head().unwrap().peel_to_commit().unwrap();
            create_lightweight_tag(repo, "main", &tag_commit);

            commit_file(repo, "feature.rs", "pub fn feature() {}\n", "feature");

            let mut changed = get_diff_files(repo_path, "main").unwrap();
            changed.sort();
            assert_eq!(
                changed,
                vec!["feature.rs".to_string(), "tagged.rs".to_string()]
            );
        });
    }

    #[test]
    fn test_get_diff_files_with_commit_sha() {
        with_temp_repo(|repo| {
            let repo_path = repo.workdir().unwrap();
            commit_file(repo, "base.rs", "pub fn base() {}\n", "base");

            let base_sha = repo.head().unwrap().target().unwrap().to_string();

            commit_file(repo, "new.rs", "pub fn new() {}\n", "new commit");

            let changed = get_diff_files(repo_path, &base_sha).unwrap();
            assert_eq!(changed, vec!["new.rs".to_string()]);
        });
    }

    #[test]
    fn test_get_diff_files_with_explicit_origin_main() {
        with_temp_repo(|repo| {
            let repo_path = repo.workdir().unwrap();
            commit_file(repo, "base.rs", "pub fn base() {}\n", "base");

            let base_commit = repo.head().unwrap().peel_to_commit().unwrap();
            create_origin_main(repo, &base_commit);
            switch_to_feature_branch(repo, &base_commit);
            delete_local_branch_if_exists(repo, "main");

            commit_file(repo, "feature.rs", "pub fn feature() {}\n", "feature");

            assert!(repo.find_branch("main", BranchType::Local).is_err());
            let changed = get_diff_files(repo_path, "origin/main").unwrap();
            assert_eq!(changed, vec!["feature.rs".to_string()]);
        });
    }

    #[test]
    fn test_get_diff_files_prefers_remote_branch_over_same_named_tag() {
        with_temp_repo(|repo| {
            let repo_path = repo.workdir().unwrap();
            commit_file(repo, "base.rs", "pub fn base() {}\n", "base");

            let base_commit = repo.head().unwrap().peel_to_commit().unwrap();
            create_origin_main(repo, &base_commit);
            switch_to_feature_branch(repo, &base_commit);
            delete_local_branch_if_exists(repo, "main");

            commit_file(repo, "tagged.rs", "pub fn tagged() {}\n", "tagged");
            let tag_commit = repo.head().unwrap().peel_to_commit().unwrap();
            create_lightweight_tag(repo, "origin/main", &tag_commit);

            commit_file(repo, "feature.rs", "pub fn feature() {}\n", "feature");

            let mut changed = get_diff_files(repo_path, "origin/main").unwrap();
            changed.sort();
            assert_eq!(
                changed,
                vec!["feature.rs".to_string(), "tagged.rs".to_string()]
            );
        });
    }

    #[test]
    fn test_get_diff_files_falls_back_to_origin_main() {
        with_temp_repo(|repo| {
            let repo_path = repo.workdir().unwrap();
            commit_file(repo, "base.rs", "pub fn base() {}\n", "base");

            let base_commit = repo.head().unwrap().peel_to_commit().unwrap();
            create_origin_main(repo, &base_commit);
            switch_to_feature_branch(repo, &base_commit);
            delete_local_branch_if_exists(repo, "main");

            commit_file(repo, "feature.rs", "pub fn feature() {}\n", "feature");

            assert!(repo.find_branch("main", BranchType::Local).is_err());
            let changed = get_diff_files(repo_path, "main").unwrap();
            assert_eq!(changed, vec!["feature.rs".to_string()]);
        });
    }

    #[test]
    fn test_get_diff_files_reports_missing_base_ref() {
        with_temp_repo(|repo| {
            let repo_path = repo.workdir().unwrap();
            commit_file(repo, "base.rs", "pub fn base() {}\n", "base");

            let err = get_diff_files(repo_path, "missing-base").unwrap_err();
            match err {
                GitError::BaseRefNotFound { base_ref } => {
                    assert_eq!(base_ref, "missing-base");
                }
                other => panic!("expected BaseRefNotFound, got {other:?}"),
            }
        });
    }

    #[test]
    fn test_get_diff_files_reports_base_ref_not_commit() {
        with_temp_repo(|repo| {
            let repo_path = repo.workdir().unwrap();
            commit_file(repo, "base.rs", "pub fn base() {}\n", "base");

            let blob_oid = repo.blob(b"not a commit").unwrap();
            let blob_ref = blob_oid.to_string();
            let err = get_diff_files(repo_path, &blob_ref).unwrap_err();
            match err {
                GitError::BaseRefNotCommit { base_ref } => {
                    assert_eq!(base_ref, blob_ref);
                }
                other => panic!("expected BaseRefNotCommit, got {other:?}"),
            }
        });
    }
}
