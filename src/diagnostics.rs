use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tracing::{debug, warn};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticsLevel {
    #[default]
    Off,
    Local,
    Verbose,
}

impl From<String> for DiagnosticsLevel {
    fn from(s: String) -> Self {
        match s.to_lowercase().as_str() {
            "local" => Self::Local,
            "verbose" => Self::Verbose,
            _ => Self::Off,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DiagnosticEvent {
    ToolInvoke {
        name: String,
        duration_ms: u128,
        success: bool,
    },
    SyncPlan {
        strategy: String,
        files_to_sync: usize,
        deleted_paths: usize,
        duration_ms: u128,
    },
    IndexingFile {
        language: String,
        chunks: usize,
        duration_ms: u128,
    },
    EmbeddingBatch {
        size: usize,
        duration_ms: u128,
    },
    Search {
        query_len: usize,
        results_count: usize,
        duration_ms: u128,
        source: String, // "main", "shadow", "hybrid"
    },
    ShadowIndexUpdate {
        files: usize,
        chunks: usize,
        duration_ms: u128,
    },
    Error {
        category: String,
        message: String,
    },
}

#[derive(Debug)]
pub struct Diagnostics {
    level: DiagnosticsLevel,
    log_file: Option<PathBuf>,
}

impl Diagnostics {
    pub fn new(level: DiagnosticsLevel, config_dir: &Path) -> Self {
        let log_file = if level != DiagnosticsLevel::Off {
            Some(config_dir.join("diagnostics.jsonl"))
        } else {
            None
        };

        Self { level, log_file }
    }

    pub async fn emit(&self, event: DiagnosticEvent) {
        if self.level == DiagnosticsLevel::Off {
            return;
        }

        let timestamp = chrono::Utc::now().to_rfc3339();

        #[derive(Serialize)]
        struct LogEntry {
            timestamp: String,
            #[serde(flatten)]
            event: DiagnosticEvent,
        }

        let entry = LogEntry { timestamp, event };
        let line = match serde_json::to_string(&entry) {
            Ok(s) => s,
            Err(e) => {
                warn!("Failed to serialize diagnostic event: {e}");
                return;
            }
        };

        if let Some(ref path) = self.log_file {
            if let Err(e) = append_to_file(path, &line).await {
                warn!(
                    "Failed to write diagnostic event to {}: {e}",
                    path.display()
                );
            }
        }

        if self.level == DiagnosticsLevel::Verbose {
            debug!("Diagnostic event: {}", line);
        }
    }

    pub async fn get_metrics(&self) -> anyhow::Result<MetricsSummary> {
        let path = self
            .log_file
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Diagnostics are disabled"))?;
        if !path.exists() {
            return Ok(MetricsSummary::default());
        }

        let content = tokio::fs::read_to_string(path).await?;
        let mut summary = MetricsSummary::default();

        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }

            #[derive(Deserialize)]
            struct LogEntry {
                #[serde(flatten)]
                event: DiagnosticEvent,
            }

            if let Ok(entry) = serde_json::from_str::<LogEntry>(line) {
                summary.update(entry.event);
            }
        }

        Ok(summary)
    }

    pub async fn clear(&self) -> anyhow::Result<()> {
        if let Some(ref path) = self.log_file {
            if path.exists() {
                tokio::fs::remove_file(path).await?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Default, Serialize)]
pub struct MetricsSummary {
    pub tool_invocations: std::collections::HashMap<String, ToolStats>,
    pub sync_stats: SyncMetrics,
    pub indexing_stats: IndexingMetrics,
    pub search_stats: SearchMetrics,
    pub errors: std::collections::HashMap<String, usize>,
}

#[derive(Debug, Default, Serialize)]
pub struct ToolStats {
    pub count: usize,
    pub successes: usize,
    pub total_duration_ms: u128,
}

#[derive(Debug, Default, Serialize)]
pub struct SyncMetrics {
    pub total_plans: usize,
    pub total_files_synced: usize,
    pub total_duration_ms: u128,
}

#[derive(Debug, Default, Serialize)]
pub struct IndexingMetrics {
    pub total_files: usize,
    pub total_chunks: usize,
    pub total_duration_ms: u128,
}

#[derive(Debug, Default, Serialize)]
pub struct SearchMetrics {
    pub total_searches: usize,
    pub total_results: usize,
    pub total_duration_ms: u128,
}

impl MetricsSummary {
    fn update(&mut self, event: DiagnosticEvent) {
        match event {
            DiagnosticEvent::ToolInvoke {
                name,
                duration_ms,
                success,
            } => {
                let stats = self.tool_invocations.entry(name).or_default();
                stats.count += 1;
                if success {
                    stats.successes += 1;
                }
                stats.total_duration_ms += duration_ms;
            }
            DiagnosticEvent::SyncPlan {
                files_to_sync,
                duration_ms,
                ..
            } => {
                self.sync_stats.total_plans += 1;
                self.sync_stats.total_files_synced += files_to_sync;
                self.sync_stats.total_duration_ms += duration_ms;
            }
            DiagnosticEvent::IndexingFile {
                chunks,
                duration_ms,
                ..
            } => {
                self.indexing_stats.total_files += 1;
                self.indexing_stats.total_chunks += chunks;
                self.indexing_stats.total_duration_ms += duration_ms;
            }
            DiagnosticEvent::EmbeddingBatch { .. } => {}
            DiagnosticEvent::Search {
                results_count,
                duration_ms,
                ..
            } => {
                self.search_stats.total_searches += 1;
                self.search_stats.total_results += results_count;
                self.search_stats.total_duration_ms += duration_ms;
            }
            DiagnosticEvent::ShadowIndexUpdate { .. } => {}
            DiagnosticEvent::Error { category, .. } => {
                *self.errors.entry(category).or_default() += 1;
            }
        }
    }
}

async fn append_to_file(path: &Path, line: &str) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    file.write_all(line.as_bytes()).await?;
    file.write_all(b"\n").await?;
    Ok(())
}

pub type DiagnosticsArc = Arc<Diagnostics>;
