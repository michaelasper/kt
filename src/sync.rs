use crate::diagnostics::{DiagnosticEvent, DiagnosticsArc};
use crate::discovery::{self, DiscoveredFile, DiscoveryOptions};
use crate::git;
use crate::storage::Storage;
use crate::Codebase;
use futures::StreamExt;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
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
    pub failed_paths: Vec<String>,
}

pub trait SyncProgress: Send + Sync {
    fn start_file(&mut self, path: &str, index: usize);
    fn finish_file(&mut self, path: &str, chunks: usize);
    fn finish(&mut self, files: usize, chunks: usize, errors: usize, failed_paths: Vec<String>);
}

pub struct NoopProgress;

impl SyncProgress for NoopProgress {
    fn start_file(&mut self, _path: &str, _index: usize) {}
    fn finish_file(&mut self, _path: &str, _chunks: usize) {}
    fn finish(
        &mut self,
        _files: usize,
        _chunks: usize,
        _errors: usize,
        _failed_paths: Vec<String>,
    ) {
    }
}

pub async fn plan(
    root: &Path,
    storage: &Storage,
    codebase: &Codebase,
    full: bool,
    diagnostics: DiagnosticsArc,
) -> anyhow::Result<SyncPlan> {
    plan_with_options(
        root,
        storage,
        codebase,
        full,
        &DiscoveryOptions::default(),
        diagnostics,
    )
    .await
}

pub async fn plan_with_options(
    root: &Path,
    storage: &Storage,
    codebase: &Codebase,
    full: bool,
    discovery_options: &DiscoveryOptions,
    diagnostics: DiagnosticsArc,
) -> anyhow::Result<SyncPlan> {
    let start = std::time::Instant::now();
    let root_buf = root.to_path_buf();

    let plan = if full {
        tracing::info!("Full sync requested (--full flag)");
        SyncPlan {
            files: discovery::discover_files_with_options_async(
                root_buf.clone(),
                discovery_options.clone(),
            )
            .await?,
            strategy: SyncStrategy::Full,
            deleted_paths: Vec::new(),
        }
    } else {
        let is_git = {
            let root_buf = root_buf.clone();
            tokio::task::spawn_blocking(move || git2::Repository::discover(&root_buf).is_ok())
                .await
                .unwrap_or(false)
        };

        if is_git {
            plan_git_aware(root_buf, storage, codebase, discovery_options).await?
        } else {
            tracing::info!("Not a git repository, using mtime-based partial sync");
            let known_mtimes = storage.get_file_mtimes(Some(&codebase.codebase_id)).await?;
            SyncPlan {
                files: discovery::discover_modified_files_with_options_async(
                    root_buf.clone(),
                    known_mtimes,
                    discovery_options.clone(),
                )
                .await?,
                strategy: SyncStrategy::PartialMtime,
                deleted_paths: Vec::new(),
            }
        }
    };

    emit_sync_plan_diagnostic(&diagnostics, &plan, start).await;
    Ok(plan)
}

async fn plan_git_aware(
    root_buf: PathBuf,
    storage: &Storage,
    codebase: &Codebase,
    discovery_options: &DiscoveryOptions,
) -> anyhow::Result<SyncPlan> {
    tracing::info!("Git repository detected, using git-aware partial sync");

    let git_info = {
        let root_buf = root_buf.clone();
        tokio::task::spawn_blocking(move || git::get_git_info(&root_buf)).await??
    };

    let current_commit = match git_info.commit_sha {
        Some(sha) => sha,
        None => {
            tracing::warn!("No commit SHA found (detached HEAD?), falling back to full sync");
            return Ok(SyncPlan {
                files: discovery::discover_files_with_options_async(
                    root_buf,
                    discovery_options.clone(),
                )
                .await?,
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
                files: discovery::discover_files_with_options_async(
                    root_buf,
                    discovery_options.clone(),
                )
                .await?,
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

            let changed_paths = {
                let root_buf = root_buf.clone();
                let last = last.clone();
                tokio::task::spawn_blocking(move || git::get_diff_files(&root_buf, &last)).await??
            };

            let mut deleted_paths = Vec::new();
            for path in &changed_paths {
                if !root_buf.join(path).exists() {
                    tracing::info!("Deleted file detected: {}", path);
                    deleted_paths.push(path.clone());
                }
            }

            let changed_set: HashSet<_> = changed_paths.into_iter().collect();

            let all_files =
                discovery::discover_files_with_options_async(root_buf, discovery_options.clone())
                    .await?;
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
    }
}

async fn emit_sync_plan_diagnostic(
    diagnostics: &DiagnosticsArc,
    plan: &SyncPlan,
    start: std::time::Instant,
) {
    diagnostics
        .emit(DiagnosticEvent::SyncPlan {
            strategy: match plan.strategy {
                SyncStrategy::Full => "full".to_string(),
                SyncStrategy::PartialGit { .. } => "partial_git".to_string(),
                SyncStrategy::PartialMtime => "partial_mtime".to_string(),
            },
            files_to_sync: plan.files.len(),
            deleted_paths: plan.deleted_paths.len(),
            duration_ms: start.elapsed().as_millis(),
        })
        .await;
}

pub async fn execute(
    plan: SyncPlan,
    codebase: &Codebase,
    storage: &Storage,
    engine: Arc<crate::embedding::EmbeddingEngine>,
    progress: Arc<Mutex<dyn SyncProgress>>,
    diagnostics: DiagnosticsArc,
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
    let failed_paths = Arc::new(Mutex::new(Vec::new()));

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
            let failed_paths = failed_paths.clone();
            let semaphore = semaphore.clone();
            let progress = progress.clone();
            let diagnostics = diagnostics.clone();

            async move {
                let _permit = match semaphore.acquire().await {
                    Ok(p) => p,
                    Err(_) => return,
                };

                {
                    let mut p = progress.lock().await;
                    p.start_file(&file.relative_path, i);
                }

                let parse_start = std::time::Instant::now();
                let chunks = match crate::indexing::parse_file_async(
                    file.path.clone(),
                    file.relative_path.clone(),
                    file.language,
                    codebase_id.clone(),
                )
                .await
                {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("Failed to parse {}: {e}", file.relative_path);
                        errors.fetch_add(1, Ordering::SeqCst);
                        {
                            let mut fps = failed_paths.lock().await;
                            fps.push(file.relative_path.clone());
                        }
                        let mut p = progress.lock().await;
                        p.finish_file(&file.relative_path, 0);
                        return;
                    }
                };

                if chunks.is_empty() {
                    tracing::debug!("No chunks produced for {}", file.relative_path);
                    let mut p = progress.lock().await;
                    p.finish_file(&file.relative_path, 0);
                    return;
                }

                diagnostics
                    .emit(DiagnosticEvent::IndexingFile {
                        language: format!("{:?}", file.language),
                        chunks: chunks.len(),
                        duration_ms: parse_start.elapsed().as_millis(),
                    })
                    .await;

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

                let file_path = file.path.clone();
                let embed_start = std::time::Instant::now();

                let text_refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                let embeddings_result = engine.embed_batch(&text_refs).await;

                let result = match embeddings_result {
                    Ok(embeddings) => {
                        let mtime = discovery::get_file_mtime(&file_path).unwrap_or_default();
                        Ok((embeddings, mtime))
                    }
                    Err(e) => Err(e),
                };

                match result {
                    Ok((embeddings, mtime)) => {
                        diagnostics
                            .emit(DiagnosticEvent::EmbeddingBatch {
                                size: chunks.len(),
                                duration_ms: embed_start.elapsed().as_millis(),
                            })
                            .await;

                        let mtimes = vec![mtime; chunks.len()];

                        if let Err(e) = storage
                            .store_chunks_batch(&chunks, &embeddings, Some(&mtimes))
                            .await
                        {
                            tracing::warn!(
                                "Failed to store chunks for {}: {e}",
                                file.relative_path
                            );
                            errors.fetch_add(1, Ordering::SeqCst);
                        } else {
                            total_chunks.fetch_add(chunks.len(), Ordering::SeqCst);
                            total_files.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to process chunks for {}: {e}", file.relative_path);
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

    let final_failed_paths = failed_paths.lock().await.clone();
    Ok(SyncStats {
        total_files: total_files.load(Ordering::SeqCst),
        total_chunks: total_chunks.load(Ordering::SeqCst),
        errors: errors.load(Ordering::SeqCst),
        failed_paths: final_failed_paths,
    })
}

pub async fn finalize(
    root: &Path,
    codebase: &Codebase,
    strategy: &SyncStrategy,
    storage: &Storage,
) -> anyhow::Result<()> {
    let root_buf = root.to_path_buf();
    match strategy {
        SyncStrategy::Full | SyncStrategy::PartialGit { .. } => {
            let codebase_id = codebase.codebase_id.clone();
            let commit_result = tokio::task::spawn_blocking(move || git::get_git_info(&root_buf))
                .await
                .map_err(anyhow::Error::from)?;

            let commit = match commit_result {
                Ok(info) => info.commit_sha,
                Err(e) => {
                    tracing::warn!("Failed to retrieve git info during sync finalize: {e}");
                    // Preserve the previous commit if possible to avoid full re-index next time
                    storage
                        .get_last_synced_commit(&codebase_id)
                        .await
                        .ok()
                        .flatten()
                }
            };

            storage
                .mark_codebase_indexed(&codebase.codebase_id, commit.as_deref())
                .await?;

            if let Some(commit) = commit {
                tracing::debug!("Saved last synced commit to {}", &commit[..8]);
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
            diagnostics: crate::diagnostics::DiagnosticsLevel::Off,
        };
        let storage = Storage::new(&config).unwrap();
        let codebase = Codebase::from_root(temp.path(), None).unwrap();
        let discovery_options = config.discovery_options();
        let diagnostics = Arc::new(crate::diagnostics::Diagnostics::new(
            crate::diagnostics::DiagnosticsLevel::Off,
            temp.path(),
        ));

        let plan = plan_with_options(
            temp.path(),
            &storage,
            &codebase,
            true,
            &discovery_options,
            diagnostics,
        )
        .await
        .unwrap();

        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.files[0].relative_path, "src/lib.rs");
    }
}
