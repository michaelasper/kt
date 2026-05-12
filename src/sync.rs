use crate::discovery::{self, DiscoveredFile};
use crate::git;
use crate::storage::Storage;
use crate::Codebase;
use std::collections::HashSet;
use std::path::Path;

pub enum SyncStrategy {
    Full,
    PartialGit {
        prev_commit: String,
        current_commit: String,
    },
    PartialMtime,
}

pub struct SyncPlan {
    pub files: Vec<DiscoveredFile>,
    pub strategy: SyncStrategy,
    pub deleted_paths: Vec<String>,
}

pub struct SyncStats {
    pub total_files: usize,
    pub total_chunks: usize,
    pub errors: usize,
}

pub trait SyncProgress: Send + Sync {
    fn start_file(&mut self, path: &str, index: usize);
    fn finish_file(&mut self, path: &str, chunks: usize);
    fn finish(self, files: usize, chunks: usize)
    where
        Self: Sized;
}

pub struct NoopProgress;

impl SyncProgress for NoopProgress {
    fn start_file(&mut self, _path: &str, _index: usize) {}
    fn finish_file(&mut self, _path: &str, _chunks: usize) {}
    fn finish(self, _files: usize, _chunks: usize) {}
}

pub async fn plan(
    root: &Path,
    storage: &Storage,
    codebase: &Codebase,
    full: bool,
) -> anyhow::Result<SyncPlan> {
    if full {
        tracing::info!("Full sync requested (--full flag)");
        return Ok(SyncPlan {
            files: discovery::discover_files(root),
            strategy: SyncStrategy::Full,
            deleted_paths: Vec::new(),
        });
    }

    if git2::Repository::discover(root).is_ok() {
        tracing::info!("Git repository detected, using git-aware partial sync");

        let git_info = git::get_git_info(root)?;
        let current_commit = match git_info.commit_sha {
            Some(sha) => sha,
            None => {
                tracing::warn!("No commit SHA found (detached HEAD?), falling back to full sync");
                return Ok(SyncPlan {
                    files: discovery::discover_files(root),
                    strategy: SyncStrategy::Full,
                    deleted_paths: Vec::new(),
                });
            }
        };

        let last_commit = storage
            .get_last_synced_commit(&codebase.codebase_id)
            .await?;

        match last_commit {
            None => {
                tracing::info!("No previous sync found, performing full sync");
                Ok(SyncPlan {
                    files: discovery::discover_files(root),
                    strategy: SyncStrategy::Full,
                    deleted_paths: Vec::new(),
                })
            }
            Some(last) if last == current_commit => {
                tracing::info!("Already up to date (commit: {})", current_commit);
                Ok(SyncPlan {
                    files: vec![],
                    strategy: SyncStrategy::PartialGit {
                        prev_commit: last,
                        current_commit,
                    },
                    deleted_paths: Vec::new(),
                })
            }
            Some(last) => {
                tracing::info!(
                    "Changes detected ({} -> {}), performing partial sync",
                    &last[..8],
                    &current_commit[..8]
                );

                match git::get_diff_files(root, &last) {
                    Ok(changed_paths) => {
                        let mut deleted_paths = Vec::new();
                        for path in &changed_paths {
                            if !root.join(path).exists() {
                                tracing::info!("Deleted file detected: {}", path);
                                deleted_paths.push(path.clone());
                            }
                        }

                        let changed_set: HashSet<_> = changed_paths.into_iter().collect();

                        let all_files = discovery::discover_files(root);
                        let changed_files: Vec<_> = all_files
                            .into_iter()
                            .filter(|f| changed_set.contains(&f.relative_path))
                            .collect();

                        if changed_files.is_empty() {
                            tracing::info!("No supported files in changed set");
                        } else {
                            tracing::info!("Found {} changed files to index", changed_files.len());
                        }

                        Ok(SyncPlan {
                            files: changed_files,
                            strategy: SyncStrategy::PartialGit {
                                prev_commit: last,
                                current_commit,
                            },
                            deleted_paths,
                        })
                    }
                    Err(e) => {
                        tracing::warn!("Failed to compute diff ({e}), falling back to full sync");
                        Ok(SyncPlan {
                            files: discovery::discover_files(root),
                            strategy: SyncStrategy::Full,
                            deleted_paths: Vec::new(),
                        })
                    }
                }
            }
        }
    } else {
        tracing::info!("Not a git repository, using mtime-based partial sync");

        let known_mtimes = storage.get_file_mtimes(Some(&codebase.codebase_id)).await?;
        Ok(SyncPlan {
            files: discovery::discover_modified_files(root, &known_mtimes),
            strategy: SyncStrategy::PartialMtime,
            deleted_paths: Vec::new(),
        })
    }
}

pub async fn execute(
    plan: &SyncPlan,
    codebase: &Codebase,
    storage: &Storage,
    engine: &crate::embedding::EmbeddingEngine,
    progress: &mut dyn SyncProgress,
) -> anyhow::Result<SyncStats> {
    for path in &plan.deleted_paths {
        tracing::info!("Removing deleted file from index: {}", path);
        if let Err(e) = storage
            .remove_file_chunks_scoped(&codebase.codebase_id, path)
            .await
        {
            tracing::warn!("Failed to remove chunks for deleted file {}: {e}", path);
        }
    }

    let mut total_chunks = 0usize;
    let mut total_files = 0usize;
    let mut errors = 0usize;

    for (i, file) in plan.files.iter().enumerate() {
        let chunks = crate::indexing::parse_file(
            &file.path,
            &file.relative_path,
            file.language,
            &codebase.codebase_id,
        );
        if chunks.is_empty() {
            continue;
        }

        progress.start_file(&file.relative_path, i);

        if let Err(e) = storage
            .remove_file_chunks_scoped(&codebase.codebase_id, &file.relative_path)
            .await
        {
            tracing::warn!("Failed to clean old chunks for {}: {e}", file.relative_path);
        }

        let texts: Vec<String> = chunks
            .iter()
            .map(crate::embedding::chunk_embedding_text)
            .collect();
        let text_refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        match engine.embed_batch(&text_refs) {
            Ok(embeddings) => {
                let mtime = discovery::get_file_mtime(&file.path).unwrap_or_default();
                let mtimes = vec![mtime; chunks.len()];

                if let Err(e) = storage
                    .store_chunks_batch(&chunks, &embeddings, Some(&mtimes))
                    .await
                {
                    tracing::warn!("Failed to store chunks for {}: {e}", file.relative_path);
                    errors += 1;
                    continue;
                }
                total_chunks += chunks.len();
                total_files += 1;
            }
            Err(e) => {
                tracing::warn!("Failed to embed chunks for {}: {e}", file.relative_path);
                errors += 1;
            }
        }

        progress.finish_file(&file.relative_path, chunks.len());
    }

    Ok(SyncStats {
        total_files,
        total_chunks,
        errors,
    })
}

pub async fn finalize(
    root: &Path,
    codebase: &Codebase,
    strategy: &SyncStrategy,
    storage: &Storage,
) -> anyhow::Result<()> {
    match strategy {
        SyncStrategy::Full => {
            let commit = git::get_git_info(root)
                .ok()
                .and_then(|info| info.commit_sha);
            storage
                .mark_codebase_indexed(&codebase.codebase_id, commit.as_deref())
                .await?;
            if let Some(commit) = commit {
                tracing::debug!(
                    "Saved last synced commit after full sync to {}",
                    &commit[..8]
                );
            }
        }
        SyncStrategy::PartialGit { .. } => {
            let commit = git::get_git_info(root)
                .ok()
                .and_then(|info| info.commit_sha);
            storage
                .mark_codebase_indexed(&codebase.codebase_id, commit.as_deref())
                .await?;
            if let Some(commit) = commit {
                tracing::debug!("Updated last synced commit to {}", &commit[..8]);
            }
        }
        SyncStrategy::PartialMtime => {
            storage
                .mark_codebase_indexed(&codebase.codebase_id, None)
                .await?;
        }
    }

    Ok(())
}
