use clap::{Parser, Subcommand};
use kt::global_config::GlobalConfigManager;
use kt::mcp_setup::HarnessType;
use kt::sync::SyncProgress;
use kt::upgrade::Upgrader;
use std::io::IsTerminal;
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
        /// Force full re-index instead of partial sync
        #[arg(short, long)]
        full: bool,
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
        Commands::Sync { directory, full } => {
            run_sync(&config, &directory, full).await?;
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

struct CliProgress {
    ui: kt::sync_ui::SyncUI,
}

impl kt::sync::SyncProgress for CliProgress {
    fn start_file(&mut self, path: &str, index: usize) {
        self.ui.start_file(path, index);
    }

    fn finish_file(&mut self, path: &str, chunks: usize) {
        self.ui.finish_file(path, chunks);
    }

    fn finish(self, files: usize, chunks: usize) {
        self.ui.finish(files, chunks);
    }
}

async fn run_sync(
    config: &kt::config::Config,
    directory: &std::path::Path,
    full: bool,
) -> anyhow::Result<()> {
    let is_tty = std::io::stdout().is_terminal();

    let default_level = if is_tty { "kt=warn" } else { "kt=info" };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::default()
            .add_directive(
                default_level
                    .parse()
                    .expect("Failed to parse default_level tracing directive"),
            )
            .add_directive(
                "ort=warn"
                    .parse()
                    .expect("Failed to parse ort=warn tracing directive"),
            )
    });

    tracing_subscriber::fmt().with_env_filter(filter).init();

    if !directory.exists() {
        anyhow::bail!("Directory not found: {}", directory.display());
    }

    let storage = kt::storage::Storage::new(config)?;
    storage.ensure_index().await?;

    let engine = kt::embedding::EmbeddingEngine::new(config).await?;

    let plan = kt::sync::plan(directory, &storage, full).await?;

    if plan.files.is_empty() {
        tracing::info!("No supported files found to sync");
        return Ok(());
    }

    let mut progress = CliProgress {
        ui: kt::sync_ui::SyncUI::new(plan.files.len()),
    };

    let stats = kt::sync::execute(&plan, &storage, &engine, &mut progress).await?;
    kt::sync::finalize(directory, &plan.strategy, &storage).await?;

    progress.finish(stats.total_files, stats.total_chunks);

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
            println!(
                "{}",
                console::style("⚠ Remove feature not yet implemented").yellow()
            );
            println!("To manually remove kt from a harness config:");
            println!("  1. Open the config file");
            println!("  2. Remove the \"kt\" entry from mcpServers");
            println!("  3. Save the file");
            println!();
            println!(
                "Run {} to see config locations",
                console::style("kt mcp list").cyan()
            );
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
