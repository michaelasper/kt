use kt::debug_lsp::{file_uri_to_path, DebugLspManager};
use serde_json::Value;
use std::path::PathBuf;

#[tokio::test]
async fn rust_analyzer_definition_and_references_point_to_existing_files() -> anyhow::Result<()> {
    if !rust_analyzer_available() {
        eprintln!("skipping rust-analyzer integration test; rust-analyzer not found");
        return Ok(());
    }

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let filepath = root.join("src/mcp.rs");
    let source = std::fs::read_to_string(&filepath)?;
    let (line, character) = find_position(&source, "mcp_error(\"directory_path")?;
    let manager = DebugLspManager::new();

    let definition = wait_for_locations(|| {
        let manager = &manager;
        let root = &root;
        let filepath = &filepath;
        async move { manager.definition(root, filepath, line, character).await }
    })
    .await?;
    assert!(
        collect_existing_paths(&definition)
            .iter()
            .any(|path| path.exists()),
        "definition returned no existing file locations: {definition}"
    );

    let references = wait_for_locations(|| {
        let manager = &manager;
        let root = &root;
        let filepath = &filepath;
        async move {
            manager
                .references(root, filepath, line, character, true)
                .await
        }
    })
    .await?;
    assert!(
        collect_existing_paths(&references)
            .iter()
            .any(|path| path.exists()),
        "references returned no existing file locations: {references}"
    );

    Ok(())
}

fn rust_analyzer_available() -> bool {
    let analyzer =
        std::env::var("KT_RUST_ANALYZER").unwrap_or_else(|_| "rust-analyzer".to_string());
    std::process::Command::new(analyzer)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

async fn wait_for_locations<F, Fut>(mut request: F) -> anyhow::Result<Value>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Value>>,
{
    let mut last = Value::Null;

    for _ in 0..20 {
        let value = request().await?;
        if !collect_existing_paths(&value).is_empty() {
            return Ok(value);
        }
        last = value;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    Ok(last)
}

fn find_position(source: &str, needle: &str) -> anyhow::Result<(usize, usize)> {
    for (line, text) in source.lines().enumerate() {
        if let Some(character) = text.find(needle) {
            return Ok((line, character));
        }
    }

    anyhow::bail!("could not find {needle}");
}

fn collect_existing_paths(value: &Value) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    collect_paths(value, &mut paths);
    paths
}

fn collect_paths(value: &Value, paths: &mut Vec<PathBuf>) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_paths(value, paths);
            }
        }
        Value::Object(object) => {
            let uri = object
                .get("uri")
                .or_else(|| object.get("targetUri"))
                .and_then(Value::as_str);
            if let Some(uri) = uri.and_then(|uri| file_uri_to_path(uri).ok()) {
                paths.push(uri);
            }
        }
        _ => {}
    }
}
