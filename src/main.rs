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

async fn run_sync(config: &kt::config::Config, directory: &std::path::Path, full: bool) -> anyhow::Result<()> {
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

    let files = if full {
        tracing::info!("Full sync requested (--full flag)");
        kt::discovery::discover_files(directory)
    } else if git2::Repository::discover(directory).is_ok() {
        tracing::info!("Git repository detected, using git-aware partial sync");

        let git_info = kt::git::get_git_info(directory)?;

        match git_info.commit_sha {
            None => {
                tracing::warn!("No commit SHA found (detached HEAD?), falling back to full sync");
                kt::discovery::discover_files(directory)
            }
            Some(current_commit) => {
                let dir_str = directory
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("Invalid UTF-8 in directory path"))?;
                let last_commit = storage.get_last_synced_commit(dir_str).await?;

                match last_commit {
                    None => {
                        tracing::info!("No previous sync found, performing full sync");
                        kt::discovery::discover_files(directory)
                    }
                    Some(last) if last == current_commit => {
                        tracing::info!("Already up to date (commit: {})", current_commit);
                        return Ok(());
                    }
                    Some(last) => {
                        tracing::info!(
                            "Changes detected ({} -> {}), performing partial sync",
                            &last[..8],
                            &current_commit[..8]
                        );

                        let changed_paths = kt::git::get_diff_files(directory, &last)?;

                        for path in &changed_paths {
                            if !directory.join(path).exists() {
                                tracing::info!("Removing deleted file from index: {}", path);
                                if let Err(e) = storage.remove_file_chunks(path).await {
                                    tracing::warn!("Failed to remove chunks for deleted file {}: {e}", path);
                                }
                            }
                        }

                        let changed_set: std::collections::HashSet<_> =
                            changed_paths.into_iter().collect();

                        let all_files = kt::discovery::discover_files(directory);
                        let changed_files: Vec<_> = all_files
                            .into_iter()
                            .filter(|f| changed_set.contains(&f.relative_path))
                            .collect();

                        if changed_files.is_empty() {
                            tracing::info!("No supported files in changed set");
                            return Ok(());
                        }

                        tracing::info!("Found {} changed files to index", changed_files.len());
                        changed_files
                    }
                }
            }
        }
    } else {
        tracing::info!("Not a git repository, using mtime-based partial sync");

        let known_mtimes = storage.get_file_mtimes().await?;
        kt::discovery::discover_modified_files(directory, &known_mtimes)
    };

    if files.is_empty() {
        tracing::info!("No supported files found to sync");
        return Ok(());
    }

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

        let mtime = kt::discovery::get_file_mtime(&file.path).unwrap_or_default();
        let mtimes = vec![mtime; chunks.len()];

        storage
            .store_chunks_batch_with_mtimes(&chunks, &embeddings, &mtimes)
            .await?;
        total_chunks += chunks.len();
        total_files += 1;
        tracing::info!("Indexed {} ({} chunks)", file.relative_path, chunks.len());
    }

    let dir_str = directory
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid UTF-8 in directory path"))?;

    if !full {
        if let Some(git_info) = kt::git::get_git_info(directory).ok() {
            if let Some(commit) = git_info.commit_sha {
                storage
                    .set_last_synced_commit(dir_str, &commit)
                    .await?;
                tracing::debug!("Updated last synced commit to {}", &commit[..8]);
            }
        }
    } else {
        storage
            .clear_sync_state(dir_str)
            .await?;
        tracing::debug!("Cleared sync state after full sync");
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
