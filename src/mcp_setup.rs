use crate::global_config::{GlobalConfig, GlobalConfigManager};
use anyhow::{Context, Result};
use console::style;
use dialoguer::MultiSelect;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HarnessType {
    OpenCode,
    ClaudeDesktop,
    Cline,
    Continue,
    Pi,
}

impl HarnessType {
    fn name(&self) -> &'static str {
        match self {
            Self::OpenCode => "OpenCode",
            Self::ClaudeDesktop => "Claude Desktop",
            Self::Cline => "Cline",
            Self::Continue => "Continue",
            Self::Pi => "Oh My Pi",
        }
    }

    fn config_path(&self) -> Option<PathBuf> {
        match self {
            Self::OpenCode => {
                let config_dir = dirs::config_dir()?;
                Some(config_dir.join("opencode/mcp.json"))
            }
            Self::ClaudeDesktop => {
                let config_dir = if cfg!(target_os = "macos") {
                    dirs::home_dir()?.join("Library/Application Support/Claude")
                } else if cfg!(target_os = "linux") {
                    dirs::config_dir()?.join("Claude")
                } else {
                    return None;
                };
                Some(config_dir.join("claude_desktop_config.json"))
            }
            Self::Cline => {
                let vscode_dir = dirs::home_dir()?.join(".vscode");
                Some(vscode_dir.join("settings.json"))
            }
            Self::Continue => {
                let vscode_dir = dirs::home_dir()?.join(".vscode");
                Some(vscode_dir.join("settings.json"))
            }
            Self::Pi => {
                let config_dir = dirs::home_dir()?.join(".omp/agent");
                Some(config_dir.join("mcp.json"))
            }
        }
    }

    fn config_namespace(&self) -> Option<&'static str> {
        match self {
            Self::OpenCode => Some("mcpServers"),
            Self::ClaudeDesktop => Some("mcpServers"),
            Self::Cline => Some("cline.mcpServers"),
            Self::Continue => Some("continue.mcpServers"),
            Self::Pi => Some("mcpServers"),
        }
    }

    fn requires_schema(&self) -> bool {
        matches!(self, Self::Pi)
    }

    fn schema_url(&self) -> Option<&'static str> {
        if self.requires_schema() {
            Some("https://raw.githubusercontent.com/can1357/oh-my-pi/main/packages/coding-agent/src/config/mcp-schema.json")
        } else {
            None
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Config directory not found: {0}")]
    ConfigDirNotFound(String),
    #[error("No config path for harness: {0}")]
    NoConfigPath(String),
    #[error("User cancelled")]
    Cancelled,
}

pub struct McpSetup {
    detected_harnesses: Vec<HarnessType>,
}

impl Default for McpSetup {
    fn default() -> Self {
        Self::new()
    }
}

impl McpSetup {
    pub fn new() -> Self {
        Self {
            detected_harnesses: Vec::new(),
        }
    }

    pub fn detect_harnesses(&mut self) -> &Vec<HarnessType> {
        self.detected_harnesses.clear();

        for harness in [
            HarnessType::OpenCode,
            HarnessType::ClaudeDesktop,
            HarnessType::Cline,
            HarnessType::Continue,
            HarnessType::Pi,
        ] {
            if self.is_harness_installed(&harness) {
                self.detected_harnesses.push(harness);
            }
        }

        &self.detected_harnesses
    }

    fn is_harness_installed(&self, harness: &HarnessType) -> bool {
        match harness {
            HarnessType::ClaudeDesktop => {
                if cfg!(target_os = "macos") {
                    let app_path = dirs::home_dir()
                        .unwrap_or_default()
                        .join("Applications/Claude.app");
                    app_path.exists()
                } else {
                    cfg!(target_os = "linux")
                }
            }
            HarnessType::Cline | HarnessType::Continue => {
                let vscode_settings = dirs::home_dir()
                    .unwrap_or_default()
                    .join(".vscode/settings.json");
                vscode_settings.exists()
            }
            HarnessType::OpenCode => {
                let config_path = dirs::config_dir().unwrap_or_default().join("opencode");
                config_path.exists()
            }
            HarnessType::Pi => {
                let config_path = dirs::home_dir().unwrap_or_default().join(".omp");
                config_path.exists()
            }
        }
    }

    pub async fn setup_harnesses(&self, harnesses: Vec<HarnessType>, global: bool) -> Result<()> {
        println!();
        println!(
            "{}",
            style("╔════════════════════════════════════════════════════════════╗").cyan()
        );
        println!(
            "{}",
            style("║                  MCP Setup Wizard                        ║").cyan()
        );
        println!(
            "{}",
            style("╚════════════════════════════════════════════════════════════╝").cyan()
        );
        println!();

        if harnesses.is_empty() {
            println!("{} No harnesses selected.", style("✗").red());
            return Ok(());
        }

        println!(
            "{} Configuring kt for {} harness(es):",
            style("→").cyan(),
            style(harnesses.len()).yellow()
        );
        for harness in &harnesses {
            println!("  • {}", style(harness.name()).white());
        }
        println!();

        let kt_config = self.generate_kt_config(global).await?;

        for harness in &harnesses {
            self.setup_single_harness(harness, &kt_config).await?;
        }

        println!();
        println!(
            "{} {}",
            style("✓").green(),
            style("MCP setup complete!").green().bold()
        );
        println!();
        println!("{}", style("Next steps:").yellow());
        println!("  1. Restart your AI harness (if running)");
        println!("  2. kt MCP tools should now be available");
        println!();

        Ok(())
    }

    async fn setup_single_harness(&self, harness: &HarnessType, kt_config: &Value) -> Result<()> {
        let config_path = harness
            .config_path()
            .ok_or_else(|| SetupError::NoConfigPath(harness.name().to_string()))?;

        println!(
            "{} Configuring {}...",
            style("→").cyan(),
            style(harness.name()).white()
        );

        let config_dir = config_path
            .parent()
            .ok_or_else(|| SetupError::ConfigDirNotFound(config_path.display().to_string()))?;

        fs::create_dir_all(config_dir).context(format!(
            "Failed to create config directory: {}",
            config_dir.display()
        ))?;

        let formatted_config = self.format_config_for_harness(harness, kt_config);

        let existing_config = if config_path.exists() {
            fs::read_to_string(&config_path)?
                .parse::<Value>()
                .unwrap_or(json!({}))
        } else {
            json!({})
        };

        let merged_config = self.merge_configs(&existing_config, &formatted_config, harness);

        fs::write(&config_path, serde_json::to_string_pretty(&merged_config)?).context(format!(
            "Failed to write config to {}",
            config_path.display()
        ))?;

        debug!("Wrote config to: {}", config_path.display());

        println!(
            "  {} {}",
            style("✓").green(),
            style(format!("Updated {}", config_path.display())).dim()
        );

        Ok(())
    }

    async fn generate_kt_config(&self, global: bool) -> Result<Value> {
        let (redis_url, redis_timeout_seconds) = if global {
            let global_manager = GlobalConfigManager::new()?;
            let global_config = global_manager.load_or_create()?;
            (
                self.resolve_redis_url(&global_config).await,
                global_config.redis.timeout_seconds.max(1),
            )
        } else {
            ("redis://localhost:6379".to_string(), 5)
        };

        Ok(json!({
            "command": "kt",
            "args": ["serve"],
            "env": {
                "KT_REDIS_URL": redis_url,
                "KT_REDIS_TIMEOUT_SECONDS": redis_timeout_seconds.to_string()
            }
        }))
    }

    async fn resolve_redis_url(&self, config: &GlobalConfig) -> String {
        if config.mcp.auto_detect_redis && config.redis.auto_detect {
            self.detect_redis_url(config.redis.timeout_seconds.max(1))
                .await
                .unwrap_or_else(|| config.redis.url.clone())
        } else {
            config.redis.url.clone()
        }
    }

    async fn detect_redis_url(&self, timeout_seconds: u64) -> Option<String> {
        info!("Attempting to detect Redis instance...");

        let common_urls = vec![
            "redis://localhost:6379",
            "redis://127.0.0.1:6379",
            "redis://localhost:6380",
            "redis://127.0.0.1:6380",
        ];

        for url in &common_urls {
            if self.test_redis_connection(url, timeout_seconds).await {
                info!("Found Redis at: {}", url);
                return Some(url.to_string());
            }
        }

        warn!("Could not auto-detect Redis");
        None
    }

    async fn test_redis_connection(&self, url: &str, timeout_seconds: u64) -> bool {
        let client = match redis::Client::open(url) {
            Ok(c) => c,
            Err(_) => return false,
        };

        matches!(
            tokio::time::timeout(
                std::time::Duration::from_secs(timeout_seconds),
                async move {
                    let mut conn = client.get_multiplexed_async_connection().await?;
                    let _: String = redis::cmd("PING").query_async(&mut conn).await?;
                    Ok::<_, redis::RedisError>(())
                },
            )
            .await,
            Ok(Ok(()))
        )
    }

    fn format_config_for_harness(&self, harness: &HarnessType, kt_config: &Value) -> Value {
        if harness.requires_schema() {
            let schema_url = harness.schema_url().unwrap();
            json!({
                "$schema": schema_url,
                "mcpServers": {
                    "kt": kt_config
                }
            })
        } else if let Some(namespace) = harness.config_namespace() {
            let mut config = json!({});
            let parts: Vec<&str> = namespace.split('.').collect();

            let mut current = &mut config;
            for (i, part) in parts.iter().enumerate() {
                if i == parts.len() - 1 {
                    current[part] = json!({
                        "kt": kt_config
                    });
                } else {
                    if !current[part].is_object() {
                        current[part] = json!({});
                    }
                    let temp = current[part].take();
                    current[part] = temp;
                    current = &mut current[part];
                }
            }

            config
        } else {
            json!({
                "mcpServers": {
                    "kt": kt_config
                }
            })
        }
    }

    fn merge_configs(&self, existing: &Value, new: &Value, harness: &HarnessType) -> Value {
        let mut merged = existing.clone();

        if let Some(namespace) = harness.config_namespace() {
            let parts: Vec<&str> = namespace.split('.').collect();
            let mut current = &mut merged;

            for (i, part) in parts.iter().enumerate() {
                if !current[part].is_object() {
                    current[part] = json!({});
                }

                if i == parts.len() - 1 {
                    if let Some(obj) = current[part].as_object_mut() {
                        if let Some(new_obj) = new.get(part).and_then(|v| v.as_object()) {
                            for (key, value) in new_obj {
                                obj.insert(key.clone(), value.clone());
                            }
                        }
                    }
                } else {
                    let temp = current[part].take();
                    current[part] = temp;
                    current = &mut current[part];
                }
            }
        } else if let Some(mcp_servers) = new.get("mcpServers") {
            if !merged["mcpServers"].is_object() {
                merged["mcpServers"] = json!({});
            }

            if let Some(obj) = merged["mcpServers"].as_object_mut() {
                if let Some(new_obj) = mcp_servers.as_object() {
                    for (key, value) in new_obj {
                        obj.insert(key.clone(), value.clone());
                    }
                }
            }
        }

        if let Some(schema) = new.get("$schema") {
            merged["$schema"] = schema.clone();
        }

        merged
    }

    pub async fn interactive_setup(&mut self, global: bool) -> Result<()> {
        self.detect_harnesses();

        if self.detected_harnesses.is_empty() {
            println!(
                "{} No MCP harnesses detected. Please install one of the following:",
                style("✗").red()
            );
            println!("  • {}", style("Claude Desktop").white());
            println!("  • {}", style("Cline (VS Code extension)").white());
            println!("  • {}", style("Continue (VS Code extension)").white());
            println!("  • {}", style("Oh My Pi").white());
            return Ok(());
        }

        println!(
            "{} Detected {} MCP harness(es):",
            style("✓").green(),
            style(self.detected_harnesses.len()).yellow()
        );

        let items: Vec<String> = self
            .detected_harnesses
            .iter()
            .map(|h| h.name().to_string())
            .collect();

        for item in &items {
            println!("  • {}", style(item).white());
        }
        println!();

        let selection = MultiSelect::new()
            .with_prompt("Select harnesses to configure (Space to select, Enter to continue)")
            .items(&items)
            .defaults(&vec![true; items.len()])
            .interact()?;

        if selection.is_empty() {
            println!("{} No harnesses selected.", style("✗").red());
            return Ok(());
        }

        let selected_harnesses: Vec<HarnessType> = selection
            .into_iter()
            .map(|i| self.detected_harnesses[i])
            .collect();

        self.setup_harnesses(selected_harnesses, global).await?;

        Ok(())
    }

    pub fn list_harnesses(&self) {
        let detected = self.detect_harnesses_internal();

        println!();
        println!("{}", style("Detected MCP Harnesses:").cyan().bold());
        println!();

        if detected.is_empty() {
            println!("  {} No harnesses detected", style("✗").red());
        } else {
            for harness in &detected {
                let config_path = harness.config_path();
                let status = if config_path.as_ref().is_some_and(|p| p.exists()) {
                    style("Configured").green()
                } else {
                    style("Not configured").yellow()
                };

                println!("  {} {}", style("•").cyan(), style(harness.name()).white());
                if let Some(path) = config_path {
                    println!("    Path: {}", style(path.display()).dim());
                }
                println!("    Status: {}", status);
                println!();
            }
        }
    }

    fn detect_harnesses_internal(&self) -> Vec<HarnessType> {
        let mut harnesses = Vec::new();

        for harness in [
            HarnessType::OpenCode,
            HarnessType::ClaudeDesktop,
            HarnessType::Cline,
            HarnessType::Continue,
            HarnessType::Pi,
        ] {
            if self.is_harness_installed(&harness) {
                harnesses.push(harness);
            }
        }

        harnesses
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_harness_type_names() {
        assert_eq!(HarnessType::OpenCode.name(), "OpenCode");
        assert_eq!(HarnessType::ClaudeDesktop.name(), "Claude Desktop");
    }

    #[test]
    fn test_config_namespace() {
        assert_eq!(HarnessType::OpenCode.config_namespace(), Some("mcpServers"));
        assert_eq!(
            HarnessType::Cline.config_namespace(),
            Some("cline.mcpServers")
        );
    }

    #[tokio::test]
    async fn resolve_redis_url_uses_configured_url_when_auto_detect_is_disabled() {
        let setup = McpSetup::new();
        let mut config = GlobalConfig::default();
        config.redis.url = "redis://redis.example:6379".to_string();
        config.redis.auto_detect = false;

        let redis_url = setup.resolve_redis_url(&config).await;

        assert_eq!(redis_url, "redis://redis.example:6379");
    }

    #[tokio::test]
    async fn test_redis_connection_returns_false_when_ping_fails() {
        let setup = McpSetup::new();

        let detected = setup.test_redis_connection("redis://127.0.0.1:1", 1).await;

        assert!(!detected);
    }
}
