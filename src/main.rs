use clap::{Parser, Subcommand};
use kt::global_config::GlobalConfigManager;
use kt::mcp_setup::HarnessType;
use kt::upgrade::Upgrader;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;

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
        /// Alias to use when referring to this codebase
        #[arg(long = "name")]
        name: Option<String>,
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
    /// Manage diagnostics and metrics
    Diagnostics {
        #[command(subcommand)]
        action: DiagnosticsAction,
    },
}

#[derive(Subcommand)]
pub enum DiagnosticsAction {
    /// Show aggregate diagnostic metrics
    Show,
    /// Clear all local diagnostic logs
    Clear,
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
    let runtime_config = if cli.command.requires_runtime_config() {
        Some(kt::config::Config::load()?)
    } else {
        None
    };

    match cli.command {
        Commands::Serve => {
            let config = runtime_config
                .ok_or_else(|| anyhow::anyhow!("runtime config missing for kt serve"))?;
            kt::mcp::run_server(config).await?;
        }
        Commands::Sync {
            directory,
            full,
            name,
        } => {
            let config = runtime_config
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("runtime config missing for kt sync"))?;
            run_sync(config, &directory, full, name.as_deref()).await?;
        }
        Commands::Upgrade { force, version } => {
            run_upgrade(force, version).await?;
        }
        Commands::Mcp { action } => {
            run_mcp_action(action).await?;
        }
        Commands::Diagnostics { action } => {
            run_diagnostics_action(action).await?;
        }
    }

    Ok(())
}

impl Commands {
    fn requires_runtime_config(&self) -> bool {
        matches!(self, Self::Serve | Self::Sync { .. })
    }
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

    fn finish(&mut self, files: usize, chunks: usize) {
        self.ui.finish(files, chunks);
    }
}

async fn run_sync(
    config: &kt::config::Config,
    directory: &std::path::Path,
    full: bool,
    codebase_alias: Option<&str>,
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
    let codebase = storage.register_codebase(directory, codebase_alias).await?;

    let global_manager = kt::global_config::GlobalConfigManager::new()?;
    let diagnostics = Arc::new(kt::diagnostics::Diagnostics::new(
        config.diagnostics.clone(),
        global_manager.get_config_dir(),
    ));

    let engine = Arc::new(kt::embedding::EmbeddingEngine::new(config).await?);

    let discovery_options = config.discovery_options();
    let plan = kt::sync::plan_with_options(
        directory,
        &storage,
        &codebase,
        full,
        &discovery_options,
        diagnostics.clone(),
    )
    .await?;

    if plan.files.is_empty() {
        tracing::info!("No supported files found to sync");
        return Ok(());
    }

    let total_files = plan.files.len();
    let progress: Arc<tokio::sync::Mutex<dyn kt::sync::SyncProgress>> =
        Arc::new(tokio::sync::Mutex::new(CliProgress {
            ui: kt::sync_ui::SyncUI::new(total_files),
        }));

    let strategy = plan.strategy.clone();
    let stats = kt::sync::execute(
        plan,
        &codebase,
        &storage,
        engine,
        progress.clone(),
        diagnostics,
    )
    .await?;
    kt::sync::finalize(directory, &codebase, &strategy, &storage).await?;

    {
        let mut p = progress.lock().await;
        p.finish(stats.total_files, stats.total_chunks);
    }

    Ok(())
}

async fn run_upgrade(force: bool, version: Option<String>) -> anyhow::Result<()> {
    let upgrader = Upgrader::new()?;
    upgrader.upgrade(force, version).await
}

async fn run_mcp_action(action: McpAction) -> anyhow::Result<()> {
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

async fn run_diagnostics_action(action: DiagnosticsAction) -> anyhow::Result<()> {
    let config = kt::Config::from_env();
    let global_manager = kt::global_config::GlobalConfigManager::new()?;
    let diagnostics = kt::diagnostics::Diagnostics::new(
        config.diagnostics.clone(),
        global_manager.get_config_dir(),
    );

    match action {
        DiagnosticsAction::Show => {
            let metrics = diagnostics.get_metrics().await?;
            show_metrics(metrics);
        }
        DiagnosticsAction::Clear => {
            diagnostics.clear().await?;
            println!("{} Diagnostic logs cleared", console::style("✓").green());
        }
    }

    Ok(())
}

fn show_metrics(metrics: kt::diagnostics::MetricsSummary) {
    use console::style;

    println!();
    println!(
        "{}",
        style("╔════════════════════════════════════════════════════════════╗").cyan()
    );
    println!(
        "{}",
        style("║                kt Diagnostics Summary                    ║").cyan()
    );
    println!(
        "{}",
        style("╚════════════════════════════════════════════════════════════╝").cyan()
    );
    println!();

    println!("{}", style("MCP Tool Usage:").cyan().bold());
    let mut tools: Vec<_> = metrics.tool_invocations.iter().collect();
    tools.sort_by_key(|b| std::cmp::Reverse(b.1.count));

    if tools.is_empty() {
        println!("  {}", style("No tool usage data collected yet.").dim());
    }

    for (name, stats) in tools {
        let avg_lat = stats
            .total_duration_ms
            .checked_div(stats.count as u128)
            .unwrap_or(0);
        println!(
            "  {:<15} {:>4} calls ({:>3}% success) avg lat: {:>4}ms",
            style(name).white(),
            stats.count,
            (stats.successes * 100)
                .checked_div(stats.count)
                .unwrap_or(0),
            avg_lat
        );
    }
    println!();

    println!("{}", style("Sync & Indexing:").cyan().bold());
    println!(
        "  Plans:          {}",
        style(metrics.sync_stats.total_plans).white()
    );
    println!(
        "  Files Indexed:  {}",
        style(metrics.indexing_stats.total_files).white()
    );
    println!(
        "  Chunks Created: {}",
        style(metrics.indexing_stats.total_chunks).white()
    );
    let avg_indexing = metrics
        .indexing_stats
        .total_duration_ms
        .checked_div(metrics.indexing_stats.total_files as u128)
        .unwrap_or(0);
    println!("  Avg Index Lat:  {}ms/file", style(avg_indexing).white());
    println!();

    println!("{}", style("Search:").cyan().bold());
    println!(
        "  Total Searches: {}",
        style(metrics.search_stats.total_searches).white()
    );
    println!(
        "  Total Results:  {}",
        style(metrics.search_stats.total_results).white()
    );
    let avg_search = metrics
        .search_stats
        .total_duration_ms
        .checked_div(metrics.search_stats.total_searches as u128)
        .unwrap_or(0);
    println!("  Avg Search Lat: {}ms", style(avg_search).white());
    println!();

    if !metrics.errors.is_empty() {
        println!("{}", style("Common Errors:").red().bold());
        for (category, count) in metrics.errors {
            println!("  {:<20} {}", style(category).white(), count);
        }
        println!();
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_serve_and_sync_require_runtime_config() {
        let commands = [
            (Commands::Serve, true),
            (
                Commands::Sync {
                    directory: PathBuf::from("."),
                    full: false,
                    name: None,
                },
                true,
            ),
            (
                Commands::Upgrade {
                    force: false,
                    version: None,
                },
                false,
            ),
            (
                Commands::Mcp {
                    action: McpAction::Show,
                },
                false,
            ),
        ];

        for (command, expected) in commands {
            assert_eq!(command.requires_runtime_config(), expected);
        }
    }
}
