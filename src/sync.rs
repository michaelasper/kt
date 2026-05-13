use crate::discovery::{self, DiscoveredFile, DiscoveryOptions};
use crate::git;
use crate::storage::Storage;
use crate::Codebase;
use futures::StreamExt;
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncStrategy {
    Full,
    PartialGit {
        prev_commit: String,
        current_commit: String,
    },
    PartialMtime,
}

#[derive(Debug, Clone)]
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
    fn finish(&mut self, files: usize, chunks: usize);
}

pub struct NoopProgress;

impl SyncProgress for NoopProgress {
    fn start_file(&mut self, _path: &str, _index: usize) {}
    fn finish_file(&mut self, _path: &str, _chunks: usize) {}
    fn finish(&mut self, _files: usize, _chunks: usize) {}
}

pub async fn plan(
    root: &Path,
    storage: &Storage,
    codebase: &Codebase,
    full: bool,
) -> anyhow::Result<SyncPlan> {
    plan_with_options(root, storage, codebase, full, &DiscoveryOptions::default()).await
}

pub async fn plan_with_options(
    root: &Path,
    storage: &Storage,
    codebase: &Codebase,
    full: bool,
    discovery_options: &DiscoveryOptions,
) -> anyhow::Result<SyncPlan> {
    if full {
        tracing::info!("Full sync requested (--full flag)");
        return Ok(SyncPlan {
            files: discovery::discover_files_with_options(root, discovery_options),
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
                    files: discovery::discover_files_with_options(root, discovery_options),
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
                    files: discovery::discover_files_with_options(root, discovery_options),
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

                        let all_files =
                            discovery::discover_files_with_options(root, discovery_options);
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
                            files: discovery::discover_files_with_options(root, discovery_options),
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
            files: discovery::discover_modified_files_with_options(
                root,
                &known_mtimes,
                discovery_options,
            ),
            strategy: SyncStrategy::PartialMtime,
            deleted_paths: Vec::new(),
        })
    }
}

pub async fn execute(
    plan: SyncPlan,
    codebase: &Codebase,
    storage: &Storage,
    engine: Arc<crate::embedding::EmbeddingEngine>,
    progress: Arc<Mutex<dyn SyncProgress>>,
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

    let total_chunks = Arc::new(AtomicUsize::new(0));
    let total_files = Arc::new(AtomicUsize::new(0));
    let errors = Arc::new(AtomicUsize::new(0));

    let concurrency = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(4);
    let semaphore = Arc::new(Semaphore::new(concurrency));

    let files_to_sync = plan.files;

    futures::stream::iter(files_to_sync.into_iter().enumerate())
        .for_each_concurrent(concurrency, |(i, file)| {
            let storage = storage.clone();
            let engine = engine.clone();
            let codebase_id = codebase.codebase_id.clone();
            let total_chunks = total_chunks.clone();
            let total_files = total_files.clone();
            let errors = errors.clone();
            let semaphore = semaphore.clone();
            let progress = progress.clone();

            async move {
                let _permit = match semaphore.acquire().await {
                    Ok(p) => p,
                    Err(_) => return,
                };

                let chunks = crate::indexing::parse_file_async(
                    file.path.clone(),
                    file.relative_path.clone(),
                    file.language,
                    codebase_id.clone(),
                )
                .await;

                if chunks.is_empty() {
                    return;
                }

                {
                    let mut p = progress.lock().await;
                    p.start_file(&file.relative_path, i);
                }

                if let Err(e) = storage
                    .remove_file_chunks_scoped(&codebase_id, &file.relative_path)
                    .await
                {
                    tracing::warn!("Failed to clean old chunks for {}: {e}", file.relative_path);
                }

                let texts: Vec<String> = chunks
                    .iter()
                    .map(crate::embedding::chunk_embedding_text)
                    .collect();

                let engine_clone = engine.clone();
                let embeddings_result = tokio::task::spawn_blocking(move || {
                    let text_refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                    engine_clone.embed_batch(&text_refs)
                })
                .await;

                match embeddings_result {
                    Ok(Ok(embeddings)) => {
                        let mtime = discovery::get_file_mtime(&file.path).unwrap_or_default();
                        let mtimes = vec![mtime; chunks.len()];

                        if let Err(e) = storage
                            .store_chunks_batch(&chunks, &embeddings, Some(&mtimes))
                            .await
                        {
                            tracing::warn!("Failed to store chunks for {}: {e}", file.relative_path);
                            errors.fetch_add(1, Ordering::SeqCst);
                        } else {
                            total_chunks.fetch_add(chunks.len(), Ordering::SeqCst);
                            total_files.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                    Ok(Err(e)) => {
                        tracing::warn!("Failed to embed chunks for {}: {e}", file.relative_path);
                        errors.fetch_add(1, Ordering::SeqCst);
                    }
                    Err(e) => {
                        tracing::warn!("Task join error during embedding: {e}");
                        errors.fetch_add(1, Ordering::SeqCst);
                    }
                }

                {
                    let mut p = progress.lock().await;
                    p.finish_file(&file.relative_path, chunks.len());
                }
            }
        })
        .await;

    Ok(SyncStats {
        total_files: total_files.load(Ordering::SeqCst),
        total_chunks: total_chunks.load(Ordering::SeqCst),
        errors: errors.load(Ordering::SeqCst),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::path::PathBuf;
    use std::time::Duration;

    #[tokio::test]
    async fn full_plan_uses_configured_discovery_options() {
        let temp = tempfile::tempdir().unwrap();
        let src_dir = temp.path().join("src");
        let generated_dir = temp.path().join("generated");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(&generated_dir).unwrap();
        std::fs::write(src_dir.join("lib.rs"), "fn kept() {}\n").unwrap();
        std::fs::write(generated_dir.join("ignored.rs"), "fn ignored() {}\n").unwrap();

        let config = Config {
            redis_url: "redis://localhost:6379".to_string(),
            redis_timeout: Duration::from_secs(1),
            model_cache_dir: PathBuf::from("."),
            exclude_patterns: vec!["generated".to_string()],
        };
        let storage = Storage::new(&config).unwrap();
        let codebase = Codebase::from_root(temp.path(), None).unwrap();
        let discovery_options = config.discovery_options();

        let plan = plan_with_options(temp.path(), &storage, &codebase, true, &discovery_options)
            .await
            .unwrap();

        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.files[0].relative_path, "src/lib.rs");
    }
}
