use clap::{Parser, Subcommand};
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
