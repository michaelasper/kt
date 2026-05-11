use crate::discovery::{self, DiscoveredFile};
use crate::git;
use crate::storage::Storage;
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
}

pub struct SyncStats {
    pub total_files: usize,
    pub total_chunks: usize,
    pub errors: usize,
}

pub trait SyncProgress: Send + Sync {
    fn start_file(&mut self, path: &str, index: usize);
    fn finish_file(&mut self, path: &str, chunks: usize);
    fn finish(&mut self, files: usize, chunks: usize);
}

pub struct NoopProgress;

impl SyncProgress for NoopProgress {
    fn start_file(&mut self, _path: &str, _index: usize) {}
    fn finish_file(&mut self, _path: &str, _chunks: usize) {}
    fn finish(&mut self, _files: usize, _chunks: usize) {}
}

pub async fn plan(root: &Path, storage: &Storage, full: bool) -> anyhow::Result<SyncPlan> {
    if full {
        tracing::info!("Full sync requested (--full flag)");
        return Ok(SyncPlan {
            files: discovery::discover_files(root),
            strategy: SyncStrategy::Full,
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
                });
            }
        };

        let dir_str = root
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid UTF-8 in directory path"))?;
        let last_commit = storage.get_last_synced_commit(dir_str).await?;

        match last_commit {
            None => {
                tracing::info!("No previous sync found, performing full sync");
                Ok(SyncPlan {
                    files: discovery::discover_files(root),
                    strategy: SyncStrategy::Full,
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
                        for path in &changed_paths {
                            if !root.join(path).exists() {
                                tracing::info!("Removing deleted file from index: {}", path);
                                if let Err(e) = storage.remove_file_chunks(path).await {
                                    tracing::warn!(
                                        "Failed to remove chunks for deleted file {}: {e}",
                                        path
                                    );
                                }
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
                            tracing::info!(
                                "Found {} changed files to index",
                                changed_files.len()
                            );
                        }

                        Ok(SyncPlan {
                            files: changed_files,
                            strategy: SyncStrategy::PartialGit {
                                prev_commit: last,
                                current_commit,
                            },
                        })
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to compute diff ({e}), falling back to full sync"
                        );
                        Ok(SyncPlan {
                            files: discovery::discover_files(root),
                            strategy: SyncStrategy::Full,
                        })
                    }
                }
            }
        }
    } else {
        tracing::info!("Not a git repository, using mtime-based partial sync");

        let known_mtimes = storage.get_file_mtimes().await?;
        Ok(SyncPlan {
            files: discovery::discover_modified_files(root, &known_mtimes),
            strategy: SyncStrategy::PartialMtime,
        })
    }
}

pub async fn execute(
    files: &[DiscoveredFile],
    storage: &Storage,
    engine: &crate::embedding::EmbeddingEngine,
    progress: &mut dyn SyncProgress,
) -> anyhow::Result<SyncStats> {
    let mut total_chunks = 0usize;
    let mut total_files = 0usize;
    let mut errors = 0usize;

    for (i, file) in files.iter().enumerate() {
        let chunks = crate::indexing::parse_file(&file.path, &file.relative_path, file.language);
        if chunks.is_empty() {
            continue;
        }

        progress.start_file(&file.relative_path, i);

        if let Err(e) = storage.remove_file_chunks(&file.relative_path).await {
            tracing::warn!("Failed to clean old chunks for {}: {e}", file.relative_path);
        }

        let texts: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
        match engine.embed_batch(&texts) {
            Ok(embeddings) => {
                let mtime = discovery::get_file_mtime(&file.path).unwrap_or_default();
                let mtimes = vec![mtime; chunks.len()];

                if let Err(e) = storage
                    .store_chunks_batch_with_mtimes(&chunks, &embeddings, &mtimes)
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
    strategy: &SyncStrategy,
    storage: &Storage,
) -> anyhow::Result<()> {
    let dir_str = root
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid UTF-8 in directory path"))?;

    match strategy {
        SyncStrategy::Full => {
            storage.clear_sync_state(dir_str).await?;
            tracing::debug!("Cleared sync state after full sync");
        }
        SyncStrategy::PartialGit { .. } => {
            if let Ok(git_info) = git::get_git_info(root) {
                if let Some(commit) = git_info.commit_sha {
                    storage.set_last_synced_commit(dir_str, &commit).await?;
                    tracing::debug!("Updated last synced commit to {}", &commit[..8]);
                }
            }
        }
        SyncStrategy::PartialMtime => {}
    }

    Ok(())
}
