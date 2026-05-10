use clap::{Parser, Subcommand};
use kt::global_config::GlobalConfigManager;
use kt::mcp_setup::HarnessType;
use kt::upgrade::Upgrader;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "kt",
    version,
    about = "A local, privacy-first polyglot codebase RAG via MCP"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the MCP server (stdio transport)
    Serve,
    /// Index a directory into the knowledge base
    Sync {
        /// Path to the directory to index
        directory: PathBuf,
    },
    /// Upgrade kt to the latest version
    Upgrade {
        /// Force upgrade even if already up-to-date
        #[arg(short, long)]
        force: bool,
        /// Specific version to install (default: latest)
        #[arg(short, long)]
        version: Option<String>,
    },
    /// Configure kt for MCP harnesses
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
}

#[derive(Subcommand)]
enum McpAction {
    /// Setup kt for one or more MCP harnesses
    Setup {
        /// Specific harnesses to configure (default: auto-detect)
        #[arg(short, long, value_name = "HARNESS")]
        harness: Vec<String>,
        /// Use global configuration
        #[arg(short, long)]
        global: bool,
        /// Create AGENTS.md in current directory
        #[arg(long)]
        create_agents: bool,
    },
    /// List detected MCP harnesses
    List,
    /// Show current global configuration
    Show,
    /// Remove kt from harness configuration
    Remove {
        /// Harnesses to remove kt from
        #[arg(required = true)]
        harness: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = kt::config::Config::from_env();

    match cli.command {
        Commands::Serve => {
            kt::mcp::run_server(config).await?;
        }
        Commands::Sync { directory } => {
            run_sync(&config, &directory).await?;
        }
        Commands::Upgrade { force, version } => {
            run_upgrade(force, version).await?;
        }
        Commands::Mcp { action } => {
            run_mcp_action(action, &config).await?;
        }
    }

    Ok(())
}

async fn run_sync(config: &kt::config::Config, directory: &std::path::Path) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    if !directory.exists() {
        anyhow::bail!("Directory not found: {}", directory.display());
    }

    let storage = kt::storage::Storage::new(config)?;
    storage.ensure_index().await?;

    let engine = kt::embedding::EmbeddingEngine::new(config).await?;

    let files = kt::discovery::discover_files(directory);
    if files.is_empty() {
        tracing::info!("No supported files found");
        return Ok(());
    }

    tracing::info!("Found {} files to index", files.len());

    let mut total_chunks = 0usize;
    let mut total_files = 0usize;

    for file in &files {
        let chunks = kt::indexing::parse_file(&file.path, &file.relative_path, file.language);
        if chunks.is_empty() {
            continue;
        }

        if let Err(e) = storage.remove_file_chunks(&file.relative_path).await {
            tracing::warn!("Failed to clean old chunks for {}: {e}", file.relative_path);
        }

        let texts: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
        let embeddings = engine.embed_batch(&texts)?;

        storage.store_chunks_batch(&chunks, &embeddings).await?;
        total_chunks += chunks.len();
        total_files += 1;
        tracing::info!("Indexed {} ({} chunks)", file.relative_path, chunks.len());
    }

    tracing::info!(
        "Sync complete: {} files, {} chunks indexed",
        total_files,
        total_chunks
    );

    Ok(())
}

async fn run_upgrade(force: bool, version: Option<String>) -> anyhow::Result<()> {
    let upgrader = Upgrader::new()?;
    upgrader.upgrade(force, version).await
}

async fn run_mcp_action(action: McpAction, _config: &kt::config::Config) -> anyhow::Result<()> {
    match action {
        McpAction::Setup {
            harness,
            global,
            create_agents,
        } => {
            let mut mcp_setup = kt::mcp_setup::McpSetup::new();

            if create_agents {
                let global_manager = GlobalConfigManager::new()?;
                let current_dir = std::env::current_dir()?;
                global_manager.copy_agents_template_to(&current_dir)?;
                println!(
                    "{} Created AGENTS.md in current directory",
                    console::style("✓").green()
                );
            }

            if harness.is_empty() {
                mcp_setup.interactive_setup(global).await?;
            } else {
                let harnesses: Result<Vec<_>, _> =
                    harness.iter().map(|h| parse_harness(h)).collect();
                let harnesses = harnesses?;
                mcp_setup.setup_harnesses(harnesses, global).await?;
            }

            if global {
                let global_manager = GlobalConfigManager::new()?;
                let _global_config = global_manager.load_or_create()?;
                println!();
                println!(
                    "{} Global configuration saved to: {}",
                    console::style("✓").green(),
                    console::style(global_manager.get_config_file().display()).dim()
                );
                println!(
                    "{} Run {} to view or modify global settings",
                    console::style("💡").yellow(),
                    console::style("kt mcp show").cyan()
                );
            }
        }
        McpAction::List => {
            let mcp_setup = kt::mcp_setup::McpSetup::new();
            mcp_setup.list_harnesses();
        }
        McpAction::Show => {
            let global_manager = GlobalConfigManager::new()?;
            global_manager.show_config()?;
        }
        McpAction::Remove { harness: _ } => {
            println!("{}", console::style("⚠ Remove feature not yet implemented").yellow());
            println!("To manually remove kt from a harness config:");
            println!("  1. Open the config file");
            println!("  2. Remove the \"kt\" entry from mcpServers");
            println!("  3. Save the file");
            println!();
            println!("Run {} to see config locations", console::style("kt mcp list").cyan());
        }
    }

    Ok(())
}

fn parse_harness(name: &str) -> anyhow::Result<HarnessType> {
    match name.to_lowercase().as_str() {
        "opencode" => Ok(HarnessType::OpenCode),
        "claude" | "claude-desktop" => Ok(HarnessType::ClaudeDesktop),
        "cline" => Ok(HarnessType::Cline),
        "continue" => Ok(HarnessType::Continue),
        "pi" | "oh-my-pi" => Ok(HarnessType::Pi),
        _ => anyhow::bail!(
            "Unknown harness: {}. Valid options: opencode, claude-desktop, cline, continue, pi",
            name
        ),
    }
}
