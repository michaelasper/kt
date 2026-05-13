use crate::diagnostics::DiagnosticsLevel;
use crate::discovery::{default_exclude_patterns, DiscoveryOptions};
use crate::global_config::{GlobalConfig, GlobalConfigManager};
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::Duration;

const DEFAULT_REDIS_URL: &str = "redis://localhost:6379";
const DEFAULT_REDIS_TIMEOUT_SECONDS: u64 = 5;

#[derive(Debug, Clone)]
pub struct Config {
    pub redis_url: String,
    pub redis_timeout: Duration,
    pub model_cache_dir: PathBuf,
    pub exclude_patterns: Vec<String>,
    pub diagnostics: DiagnosticsLevel,
}

impl Config {
    pub fn load() -> Result<Self> {
        let global_config = load_global_config()?;
        Self::from_sources(
            std::env::var("KT_REDIS_URL").ok(),
            std::env::var("KT_REDIS_TIMEOUT_SECONDS").ok(),
            std::env::var("KT_MODEL_CACHE_DIR").ok().map(PathBuf::from),
            std::env::var("KT_DIAGNOSTICS").ok(),
            global_config.as_ref(),
        )
    }

    pub fn from_env() -> Self {
        Self::load().unwrap_or_else(|error| {
            tracing::warn!("Failed to load kt config, using environment/defaults only: {error}");
            Self::from_sources(
                std::env::var("KT_REDIS_URL").ok(),
                std::env::var("KT_REDIS_TIMEOUT_SECONDS").ok(),
                std::env::var("KT_MODEL_CACHE_DIR").ok().map(PathBuf::from),
                std::env::var("KT_DIAGNOSTICS").ok(),
                None,
            )
            .unwrap_or_else(|fallback_error| {
                tracing::warn!(
                    "Failed to load environment config, using built-in defaults: {fallback_error}"
                );
                Self {
                    redis_url: DEFAULT_REDIS_URL.to_string(),
                    redis_timeout: Duration::from_secs(DEFAULT_REDIS_TIMEOUT_SECONDS),
                    model_cache_dir: default_model_cache_dir(),
                    exclude_patterns: default_exclude_patterns(),
                    diagnostics: DiagnosticsLevel::Off,
                }
            })
        })
    }

    pub fn model_path(&self) -> PathBuf {
        self.model_cache_dir.join("model.onnx")
    }

    pub fn tokenizer_path(&self) -> PathBuf {
        self.model_cache_dir.join("tokenizer.json")
    }

    pub fn discovery_options(&self) -> DiscoveryOptions {
        DiscoveryOptions::new(self.exclude_patterns.clone())
    }

    fn from_sources(
        redis_url_env: Option<String>,
        redis_timeout_seconds_env: Option<String>,
        model_cache_dir_env: Option<PathBuf>,
        diagnostics_env: Option<String>,
        global_config: Option<&GlobalConfig>,
    ) -> Result<Self> {
        let redis_url = redis_url_env
            .or_else(|| global_config.map(|config| config.redis.url.clone()))
            .unwrap_or_else(|| DEFAULT_REDIS_URL.to_string());

        let redis_timeout_seconds = match redis_timeout_seconds_env {
            Some(raw) => raw.parse::<u64>().with_context(|| {
                format!("KT_REDIS_TIMEOUT_SECONDS must be an integer, got `{raw}`")
            })?,
            None => global_config
                .map(|config| config.redis.timeout_seconds)
                .unwrap_or(DEFAULT_REDIS_TIMEOUT_SECONDS),
        };

        anyhow::ensure!(
            redis_timeout_seconds > 0,
            "Redis timeout must be greater than zero seconds"
        );

        let model_cache_dir = model_cache_dir_env.unwrap_or_else(default_model_cache_dir);
        let exclude_patterns = merge_exclude_patterns(global_config);

        let diagnostics = diagnostics_env
            .map(DiagnosticsLevel::from)
            .or_else(|| global_config.map(|config| config.diagnostics.clone()))
            .unwrap_or_default();

        Ok(Self {
            redis_url,
            redis_timeout: Duration::from_secs(redis_timeout_seconds),
            model_cache_dir,
            exclude_patterns,
            diagnostics,
        })
    }
}

fn load_global_config() -> Result<Option<GlobalConfig>> {
    let manager = match GlobalConfigManager::new() {
        Ok(manager) => manager,
        Err(error) => {
            tracing::debug!("Global config unavailable: {error}");
            return Ok(None);
        }
    };

    manager
        .load_existing()
        .context("Failed to load global kt config")
}

fn default_model_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("kt")
}

fn merge_exclude_patterns(global_config: Option<&GlobalConfig>) -> Vec<String> {
    let mut patterns = default_exclude_patterns();
    if let Some(config) = global_config {
        for pattern in &config.indexing.exclude_patterns {
            if !patterns.contains(pattern) {
                patterns.push(pattern.clone());
            }
        }
    }
    patterns
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::global_config::GlobalConfig;
#[test]
fn config_prefers_global_redis_settings_when_env_is_absent() {
    let mut global = GlobalConfig::default();
    global.redis.url = "redis://127.0.0.1:6380".to_string();
    global.redis.timeout_seconds = 12;

    let config = Config::from_sources(None, None, None, None, Some(&global)).unwrap();

    assert_eq!(config.redis_url, "redis://127.0.0.1:6380");
    assert_eq!(config.redis_timeout, Duration::from_secs(12));
}

#[test]
fn config_env_redis_settings_override_global_config() {
    let mut global = GlobalConfig::default();
    global.redis.url = "redis://127.0.0.1:6380".to_string();
    global.redis.timeout_seconds = 12;

    let config = Config::from_sources(
        Some("redis://redis.example:6379".to_string()),
        Some("30".to_string()),
        None,
        None,
        Some(&global),
    )
    .unwrap();

    assert_eq!(config.redis_url, "redis://redis.example:6379");
    assert_eq!(config.redis_timeout, Duration::from_secs(30));
}

#[test]
fn config_merges_default_and_global_indexing_exclude_patterns() {
    let mut global = GlobalConfig::default();
    global.indexing.exclude_patterns = vec!["generated".to_string(), "fixtures/**".to_string()];

    let config = Config::from_sources(None, None, None, None, Some(&global)).unwrap();

    assert!(config.exclude_patterns.contains(&"target".to_string()));
    assert!(config.exclude_patterns.contains(&".git".to_string()));
    assert!(config.exclude_patterns.contains(&"generated".to_string()));
    assert!(config.exclude_patterns.contains(&"fixtures/**".to_string()));
    }
}
