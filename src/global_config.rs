use anyhow::{Context, Result};
use console::style;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info};

const AGENTS_TEMPLATE: &str = r#"# Knowledge Transfer (kt) MCP Integration

This repository is configured to use **kt** (Knowledge Transfer) for semantic code search and retrieval.

## Available MCP Tools

### 🔍 `kt_search`
Search the indexed codebase using semantic and keyword search.
- **Usage**: "How do we handle authentication in the Rust backend?"
- **Returns**: Ranked code chunks with context

### 📄 `kt_read_file`
Read full file contents by repository-relative path.
- **Usage**: "Read the contents of `src/auth.rs`"
- **Returns**: All indexed chunks for the file

### 🔄 `kt_sync`
Index/update a directory into the knowledge base.
- **Usage**: "Run kt_sync on the current directory"
- **Effect**: Parses and embeds code for search

### 🌿 `kt_git_status`
Get git status including branch, commit, and changed files.
- **Usage**: "What files have changed in my working tree?"
- **Returns**: Branch, commit SHA, dirty state, and changed files

### 🔀 `kt_index_pr`
Index changed files into a temporary shadow index for PR review.
- **Usage**: "Index changes vs main branch"
- **Effect**: Creates ephemeral index for uncommitted/draft changes

## Quick Start

1. **Index your codebase**:
   ```bash
   kt sync .
   ```

2. **Search semantically**:
   ```
   "How does the Go service authenticate with the Java backend?"
   ```

3. **Read specific files**:
   ```
   "Read src/auth/Authenticator.java"
   ```

4. **Update during development**:
   ```bash
   kt sync .
   ```

## Configuration

- **Redis URL**: `redis://localhost:6379`
- **Languages Supported**: Rust, Go, Java
- **Index Location**: Redis Stack (local)

## Best Practices

- Run `kt sync` after significant code changes
- Use `kt_index_pr` when working on feature branches
- Leverage `kt_search` for semantic code understanding
- Use `kt_read_file` when you need complete file context

## Need Help?

Run `kt --help` or visit: https://github.com/michaelasper/kt
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalConfig {
    pub version: String,
    pub mcp: McpSettings,
    pub redis: RedisSettings,
    pub indexing: IndexingSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpSettings {
    pub auto_detect_redis: bool,
    pub default_harnesses: Vec<String>,
    pub prompt_for_global_config: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisSettings {
    pub url: String,
    pub auto_detect: bool,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexingSettings {
    pub auto_sync_on_start: bool,
    pub default_paths: Vec<String>,
    pub exclude_patterns: Vec<String>,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            version: "1.0.0".to_string(),
            mcp: McpSettings {
                auto_detect_redis: true,
                default_harnesses: vec!["OpenCode".to_string(), "ClaudeDesktop".to_string()],
                prompt_for_global_config: true,
            },
            redis: RedisSettings {
                url: "redis://localhost:6379".to_string(),
                auto_detect: true,
                timeout_seconds: 5,
            },
            indexing: IndexingSettings {
                auto_sync_on_start: false,
                default_paths: vec![".".to_string()],
                exclude_patterns: vec![
                    "node_modules".to_string(),
                    "target".to_string(),
                    ".git".to_string(),
                    "vendor".to_string(),
                ],
            },
        }
    }
}

pub struct GlobalConfigManager {
    config_dir: PathBuf,
    config_file: PathBuf,
    agents_template_file: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Config directory not found: {0}")]
    ConfigDirNotFound(String),
    #[error("Invalid config: {0}")]
    InvalidConfig(String),
}

impl GlobalConfigManager {
    pub fn new() -> Result<Self> {
        let config_dir = dirs::config_dir()
            .ok_or_else(|| {
                ConfigError::ConfigDirNotFound("Could not find config directory".to_string())
            })?
            .join("kt");

        Ok(Self {
            config_dir: config_dir.clone(),
            config_file: config_dir.join("config.json"),
            agents_template_file: config_dir.join("AGENTS.md.template"),
        })
    }

    pub fn ensure_config_dir(&self) -> Result<()> {
        fs::create_dir_all(&self.config_dir).context(format!(
            "Failed to create config directory: {}",
            self.config_dir.display()
        ))?;
        Ok(())
    }

    pub fn load_or_create(&self) -> Result<GlobalConfig> {
        self.ensure_config_dir()?;

        if self.config_file.exists() {
            let content = fs::read_to_string(&self.config_file)?;
            let config: GlobalConfig = serde_json::from_str(&content)?;

            info!("Loaded global config from: {}", self.config_file.display());
            Ok(config)
        } else {
            let config = GlobalConfig::default();
            self.save(&config)?;

            info!(
                "Created default global config at: {}",
                self.config_file.display()
            );
            Ok(config)
        }
    }

    pub fn save(&self, config: &GlobalConfig) -> Result<()> {
        self.ensure_config_dir()?;

        let content = serde_json::to_string_pretty(config)?;
        fs::write(&self.config_file, content).context(format!(
            "Failed to write config to {}",
            self.config_file.display()
        ))?;

        debug!("Saved global config to: {}", self.config_file.display());
        Ok(())
    }

    pub fn create_agents_template(&self) -> Result<PathBuf> {
        self.ensure_config_dir()?;

        if !self.agents_template_file.exists() {
            fs::write(&self.agents_template_file, AGENTS_TEMPLATE)?;
            debug!(
                "Created AGENTS.md template at: {}",
                self.agents_template_file.display()
            );
        }

        Ok(self.agents_template_file.clone())
    }

    pub fn copy_agents_template_to(&self, target_dir: &Path) -> Result<PathBuf> {
        let target_path = target_dir.join("AGENTS.md");

        if target_path.exists() {
            info!("AGENTS.md already exists at: {}", target_path.display());
            return Ok(target_path);
        }

        let template = self.create_agents_template()?;
        let content = fs::read_to_string(&template)?;
        fs::write(&target_path, content)?;

        info!("Copied AGENTS.md template to: {}", target_path.display());
        Ok(target_path)
    }

    pub fn get_config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn get_config_file(&self) -> &Path {
        &self.config_file
    }

    pub fn config_exists(&self) -> bool {
        self.config_file.exists()
    }

    pub fn show_config(&self) -> Result<()> {
        if !self.config_exists() {
            println!(
                "{} No global config found. Run {} to create one.",
                style("✗").red(),
                style("kt mcp setup --global").cyan()
            );
            return Ok(());
        }

        let config = self.load_or_create()?;

        println!();
        println!(
            "{}",
            style("╔════════════════════════════════════════════════════════════╗").cyan()
        );
        println!(
            "{}",
            style("║              Global kt Configuration                     ║").cyan()
        );
        println!(
            "{}",
            style("╚════════════════════════════════════════════════════════════╝").cyan()
        );
        println!();

        println!("{}", style("Configuration:").cyan().bold());
        println!("  File: {}", style(self.config_file.display()).dim());
        println!("  Version: {}", style(&config.version).white());
        println!();

        println!("{}", style("MCP Settings:").cyan().bold());
        println!(
            "  Auto-detect Redis: {}",
            if config.mcp.auto_detect_redis {
                style("Yes").green()
            } else {
                style("No").yellow()
            }
        );
        println!(
            "  Default Harnesses: {}",
            style(config.mcp.default_harnesses.join(", ")).white()
        );
        println!(
            "  Prompt for Global Config: {}",
            if config.mcp.prompt_for_global_config {
                style("Yes").green()
            } else {
                style("No").yellow()
            }
        );
        println!();

        println!("{}", style("Redis Settings:").cyan().bold());
        println!("  URL: {}", style(&config.redis.url).white());
        println!(
            "  Auto-detect: {}",
            if config.redis.auto_detect {
                style("Yes").green()
            } else {
                style("No").yellow()
            }
        );
        println!(
            "  Timeout: {}s",
            style(config.redis.timeout_seconds).white()
        );
        println!();

        println!("{}", style("Indexing Settings:").cyan().bold());
        println!(
            "  Auto-sync on Start: {}",
            if config.indexing.auto_sync_on_start {
                style("Yes").green()
            } else {
                style("No").yellow()
            }
        );
        println!(
            "  Default Paths: {}",
            style(config.indexing.default_paths.join(", ")).white()
        );
        println!(
            "  Exclude Patterns: {}",
            style(config.indexing.exclude_patterns.join(", ")).dim()
        );
        println!();

        Ok(())
    }

    pub fn show_welcome_message(&self) -> Result<()> {
        println!();
        println!(
            "{}",
            style("╔════════════════════════════════════════════════════════════╗").cyan()
        );
        println!(
            "{}",
            style("║                                                          ║").cyan()
        );
        println!(
            "║ {} {:^54} ║",
            style("✨").green(),
            style("kt Installed Successfully!").green().bold()
        );
        println!(
            "{}",
            style("║                                                          ║").cyan()
        );
        println!(
            "{}",
            style("╚════════════════════════════════════════════════════════════╝").cyan()
        );
        println!();

        println!("{}", style("🚀 Quick Start:").yellow().bold());
        println!();
        println!("  1. {}", style("Start Redis:").cyan());
        println!("     {}", style("docker compose up -d").white());
        println!();
        println!("  2. {}", style("Index your codebase:").cyan());
        println!("     {}", style("kt sync .").white());
        println!();
        println!(
            "  3. {}",
            style("Configure MCP (optional but recommended):").cyan()
        );
        println!("     {}", style("kt mcp setup").white());
        println!();

        println!("{}", style("💡 Pro Tips:").yellow().bold());
        println!();
        println!(
            "  • {}",
            style("Use global config for consistent settings across repos").white()
        );
        println!("    {}", style("kt mcp setup --global").cyan());
        println!();
        println!(
            "  • {}",
            style("Auto-detect Redis and harnesses for quick setup").white()
        );
        println!();
        println!(
            "  • {}",
            style("Create AGENTS.md in your repo for AI assistant context").white()
        );
        println!("    {}", style("kt mcp setup --create-agents").cyan());
        println!();

        println!("{}", style("📚 Learn More:").yellow().bold());
        println!(
            "  • Documentation: {}",
            style("https://github.com/michaelasper/kt")
                .blue()
                .underlined()
        );
        println!("  • Run: {}", style("kt --help").cyan());
        println!();

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = GlobalConfig::default();
        assert_eq!(config.version, "1.0.0");
        assert!(config.mcp.auto_detect_redis);
        assert_eq!(config.redis.url, "redis://localhost:6379");
    }

    #[test]
    fn test_config_serialization() {
        let config = GlobalConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: GlobalConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.version, deserialized.version);
    }
}
