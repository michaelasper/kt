use kt::config::Config;
use kt::embedding::EmbeddingEngine;
use kt::eval::{self, EvalFixture, EvalResult, EvalStatus};
use kt::storage::Storage;
use kt::sync;
use std::sync::Arc;

#[tokio::test]
#[ignore = "agentic eval requires Redis Stack and a local embedding model cache. Run with: cargo test --test agentic_eval -- --ignored"]
async fn run_agentic_eval_suite() -> anyhow::Result<()> {
    let fixtures = vec![
        eval::create_auth_fixture(),
        eval::create_storage_fixture(),
        eval::create_sync_fixture(),
    ];

    let mut results = Vec::new();
    for fixture in fixtures {
        let fixture_results = run_fixture_eval(fixture).await?;
        results.extend(fixture_results);
    }

    print_eval_summary(&results);

    let failed = results
        .iter()
        .filter(|r| r.status == EvalStatus::Fail)
        .count();
    if failed > 0 {
        // We don't necessarily want to fail the build yet since these are research evals
        // but we should log it.
        println!("WARNING: {}/{} eval queries failed", failed, results.len());
    }

    Ok(())
}

async fn run_fixture_eval(fixture: EvalFixture) -> anyhow::Result<Vec<EvalResult>> {
    println!("Running eval fixture: {}", fixture.name);
    let temp = tempfile::tempdir()?;

    for (path, content) in &fixture.files {
        let full_path = temp.path().join(path);
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(full_path, content)?;
    }

    let config = Config::from_env();
    let storage = Storage::new(&config)?;
    storage.ensure_index().await?;
    let engine = Arc::new(EmbeddingEngine::new(&config).await?);

    let codebase = storage
        .register_codebase(temp.path(), Some(&fixture.name))
        .await?;
    let diagnostics = Arc::new(kt::diagnostics::Diagnostics::new(
        kt::diagnostics::DiagnosticsLevel::Off,
        temp.path(),
    ));

    let plan = sync::plan(temp.path(), &storage, &codebase, true, diagnostics.clone()).await?;
    let strategy = plan.strategy.clone();
    let progress = Arc::new(tokio::sync::Mutex::new(sync::NoopProgress));

    sync::execute(
        plan,
        &codebase,
        &storage,
        engine.clone(),
        progress,
        diagnostics,
    )
    .await?;

    sync::finalize(temp.path(), &codebase, &strategy, &storage).await?;

    let mut results = Vec::new();
    for eval_query in fixture.queries {
        let query_embedding = engine.embed(&eval_query.query).await?;
        let search_results = storage
            .hybrid_search_scoped(
                &query_embedding,
                &eval_query.query,
                None, // Auto-detect
                Some(&codebase.codebase_id),
                None,
                10,
            )
            .await?;

        let mut found = Vec::new();
        let mut missing = Vec::new();

        for expected in &eval_query.expected_evidence {
            let is_found = search_results.iter().any(|r| {
                r.filepath == expected.filepath
                    && (expected.symbol.is_none()
                        || Some(r.name.as_str()) == expected.symbol.as_deref())
            });

            if is_found {
                found.push(format!("{}:{:?}", expected.filepath, expected.symbol));
            } else {
                missing.push(format!("{}:{:?}", expected.filepath, expected.symbol));
            }
        }

        let recall_at_10 = if eval_query.expected_evidence.is_empty() {
            1.0
        } else {
            found.len() as f64 / eval_query.expected_evidence.len() as f64
        };

        results.push(EvalResult {
            fixture_name: fixture.name.clone(),
            query: eval_query.query.clone(),
            status: if recall_at_10 >= 1.0 {
                EvalStatus::Pass
            } else {
                EvalStatus::Fail
            },
            recall_at_10,
            found_evidence: found,
            missing_evidence: missing,
        });
    }

    Ok(results)
}

fn print_eval_summary(results: &[EvalResult]) {
    use console::style;

    println!(
        "\n{}",
        style("Agentic RAG Evaluation Summary").bold().underlined()
    );

    let mut current_fixture = "";
    for r in results {
        if r.fixture_name != current_fixture {
            println!("\nFixture: {}", style(&r.fixture_name).cyan());
            current_fixture = &r.fixture_name;
        }

        let status_style = match r.status {
            EvalStatus::Pass => style("PASS").green(),
            EvalStatus::Fail => style("FAIL").red(),
        };

        println!(
            "  [{}] Query: \"{}\" (Recall@10: {:.2})",
            status_style, r.query, r.recall_at_10
        );

        if !r.missing_evidence.is_empty() {
            println!("    Missing: {}", r.missing_evidence.join(", "));
        }
    }
    println!();
}
