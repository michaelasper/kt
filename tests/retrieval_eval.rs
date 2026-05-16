use kt::config::Config;
use kt::embedding::EmbeddingEngine;
use kt::storage::Storage;
use kt::{sync, Language};

#[tokio::test]
#[ignore = "retrieval eval requires Redis Stack and a local embedding model cache"]
async fn abstract_auth_query_finds_expected_symbol_in_top_10() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let src = temp.path().join("src");
    std::fs::create_dir_all(&src)?;
    std::fs::write(
        src.join("auth.rs"),
        r#"
pub struct AuthService {
    token_secret: String,
}

impl AuthService {
    pub fn authenticate_request(&self, authorization_header: &str) -> bool {
        let token = authorization_header.trim_start_matches("Bearer ");
        self.verify_session_token(token)
    }

    fn verify_session_token(&self, token: &str) -> bool {
        !token.is_empty() && token.contains(&self.token_secret)
    }
}
"#,
    )?;
    std::fs::write(
        src.join("billing.rs"),
        r#"
pub fn calculate_invoice_total(line_items: &[u64]) -> u64 {
    line_items.iter().sum()
}
"#,
    )?;

    let config = Config::from_env();
    let storage = Storage::new(&config)?;
    storage.ensure_index().await?;
    let engine = std::sync::Arc::new(EmbeddingEngine::new(&config).await?);

    let codebase = storage.register_codebase(temp.path(), None).await?;
    let diagnostics = std::sync::Arc::new(kt::diagnostics::Diagnostics::new(
        kt::diagnostics::DiagnosticsLevel::Off,
        temp.path(),
    ));
    let plan = sync::plan(temp.path(), &storage, &codebase, true, diagnostics.clone()).await?;
    let strategy = plan.strategy.clone();
    let progress = std::sync::Arc::new(tokio::sync::Mutex::new(sync::NoopProgress));
    let stats = sync::execute(
        plan,
        &codebase,
        &storage,
        engine.clone(),
        progress,
        diagnostics,
    )
    .await?;
    assert_eq!(stats.errors, 0);
    sync::finalize(temp.path(), &codebase, &strategy, &storage).await?;

    let query = "how does auth work";
    let query_embedding = engine.embed(query).await?;
    let results = storage
        .hybrid_search_scoped(
            &query_embedding,
            query,
            Some(&Language::Rust),
            Some(&codebase.codebase_id),
            None,
            10,
        )
        .await?;

    assert!(
        results.iter().any(|result| {
            result.filepath == "src/auth.rs" && result.name == "authenticate_request"
        }),
        "expected authenticate_request in top 10, got {:?}",
        results
            .iter()
            .map(|result| (&result.filepath, &result.name))
            .collect::<Vec<_>>()
    );

    Ok(())
}
