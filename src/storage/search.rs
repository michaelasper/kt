use crate::{Language, SearchResult};
use redis::cmd;
use tracing::{debug, warn};

pub fn parse_search_results(value: redis::Value) -> anyhow::Result<Vec<SearchResult>> {
    let arr = match value {
        redis::Value::Array(a) => a,
        other => anyhow::bail!("FT.SEARCH expected array response, got {:?}", other),
    };

    if arr.is_empty() {
        return Ok(Vec::new());
    }

    let total_count = match &arr[0] {
        redis::Value::Int(n) => {
            if *n < 0 {
                anyhow::bail!(
                    "Invalid FT.SEARCH count: expected non-negative integer, got {}",
                    n
                );
            }
            *n as usize
        }
        invalid => anyhow::bail!(
            "Invalid FT.SEARCH count: expected integer, got {:?}",
            invalid
        ),
    };

    if total_count == 0 {
        return Ok(Vec::new());
    }

    let mut results = Vec::new();
    let mut i = 1;
    while i + 1 < arr.len() {
        let fields = &arr[i + 1];

        let mut chunk_id = String::new();
        let mut filepath = String::new();
        let mut language = Language::Rust;
        let mut node_type = String::new();
        let mut name = String::new();
        let mut signature = String::new();
        let mut content = String::new();
        let mut parent_context: Option<String> = None;
        let mut score = 0.0f64;

        if let redis::Value::Array(field_pairs) = fields {
            let mut j = 0;
            while j + 1 < field_pairs.len() {
                if let (redis::Value::BulkString(key_bytes), redis::Value::BulkString(val_bytes)) =
                    (&field_pairs[j], &field_pairs[j + 1])
                {
                    let key = String::from_utf8_lossy(key_bytes);
                    let val = String::from_utf8_lossy(val_bytes);
                    match key.as_ref() {
                        "chunk_id" => chunk_id = val.into_owned(),
                        "filepath" => filepath = val.into_owned(),
                        "language" => language = parse_language(&val),
                        "node_type" => node_type = val.into_owned(),
                        "name" => name = val.into_owned(),
                        "signature" => signature = val.into_owned(),
                        "content" => content = val.into_owned(),
                        "parent_context" if !val.is_empty() => {
                            parent_context = Some(val.into_owned());
                        }
                        "vector_score" => {
                            score = val.parse().unwrap_or(0.0);
                        }
                        _ => {}
                    }
                }
                j += 2;
            }
        }

        if !filepath.is_empty() {
            results.push(SearchResult {
                chunk_id,
                filepath,
                language,
                node_type,
                name,
                signature,
                content,
                parent_context,
                score,
            });
        }

        i += 2;
    }

    Ok(results)
}

pub(crate) fn escape_exact_match(path: &str) -> String {
    path.replace('\\', "\\\\").replace('"', "\\\"")
}

pub(crate) fn extract_doc_keys(value: &redis::Value) -> Vec<String> {
    let arr = match value {
        redis::Value::Array(a) => a,
        _ => return Vec::new(),
    };

    if arr.len() < 3 {
        return Vec::new();
    }

    let mut keys = Vec::new();
    let mut i = 1;
    while i < arr.len() {
        if let redis::Value::BulkString(bs) = &arr[i] {
            let key = String::from_utf8_lossy(bs);
            if key.starts_with(super::index::KEY_PREFIX) {
                keys.push(key.into_owned());
            }
        }
        i += 2;
    }

    keys
}

fn parse_language(s: &str) -> Language {
    match s {
        "go" => Language::Go,
        "java" => Language::Java,
        "rust" => Language::Rust,
        other => {
            warn!("Unknown language '{other}', defaulting to Rust");
            Language::Rust
        }
    }
}

fn escape_fts_query(query: &str) -> String {
    let mut result = String::with_capacity(query.len());
    for ch in query.chars() {
        if ch.is_control() {
            continue;
        }
        match ch {
            '\\' | '"' | '\'' | '(' | ')' | ':' | '{' | '}' | '|' | '@' | '!' | '-' | '*' | '['
            | ']' | ';' | ',' | '.' | '~' | '%' | '^' | '&' | '#' | '<' | '>' | '/' | '$' => {
                result.push('\\');
                result.push(ch);
            }
            _ => result.push(ch),
        }
    }
    if result.is_empty() {
        warn!(
            "FTS query collapsed to empty after escaping, falling back to wildcard: {:?}",
            query
        );
        result = "*".to_string();
    }
    result
}

fn build_hybrid_query(query_text: &str, language: Option<&Language>, top_k: usize) -> String {
    let effective_query = query_text.trim();
    if effective_query.is_empty() {
        match language {
            Some(lang) => {
                format!(
                    "@language:{{{}}}=>[KNN {top_k} @embedding $query_vec AS vector_score]",
                    lang.as_str()
                )
            }
            None => {
                format!("*=>[KNN {top_k} @embedding $query_vec AS vector_score]")
            }
        }
    } else {
        match language {
            Some(lang) => {
                format!(
                    "(@language:{{{}}} ({}))=>[KNN {} @embedding $query_vec AS vector_score]",
                    lang.as_str(),
                    escape_fts_query(effective_query),
                    top_k
                )
            }
            None => {
                format!(
                    "({})=>[KNN {} @embedding $query_vec AS vector_score]",
                    escape_fts_query(effective_query),
                    top_k
                )
            }
        }
    }
}

pub(super) async fn hybrid_search_impl(
    conn: &mut redis::aio::MultiplexedConnection,
    index_name: &str,
    query_embedding: &[f32],
    query_text: &str,
    language: Option<&Language>,
    top_k: usize,
) -> anyhow::Result<Vec<SearchResult>> {
    let embedding_bytes: Vec<u8> = query_embedding
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();

    let query_str = build_hybrid_query(query_text, language, top_k);
    debug!("Hybrid search query: {}", query_str);

    let result: redis::Value = cmd("FT.SEARCH")
        .arg(index_name)
        .arg(&query_str)
        .arg("PARAMS")
        .arg(2)
        .arg("query_vec")
        .arg(&embedding_bytes)
        .arg("DIALECT")
        .arg(2)
        .arg("SORTBY")
        .arg("vector_score")
        .arg("ASC")
        .arg("LIMIT")
        .arg(0)
        .arg(top_k)
        .query_async(conn)
        .await?;

    parse_search_results(result)
}

pub(super) async fn read_file_chunks_impl(
    conn: &mut redis::aio::MultiplexedConnection,
    index_name: &str,
    filepath: &str,
) -> anyhow::Result<Vec<SearchResult>> {
    if filepath.trim().is_empty() {
        return Ok(Vec::new());
    }

    let query_str = format!("@filepath:\"{}\"", escape_exact_match(filepath));
    debug!("Reading file chunks: {}", query_str);

    let result: redis::Value = cmd("FT.SEARCH")
        .arg(index_name)
        .arg(&query_str)
        .arg("DIALECT")
        .arg(2)
        .arg("LIMIT")
        .arg(0)
        .arg(1000)
        .arg("RETURN")
        .arg(8)
        .arg("chunk_id")
        .arg("filepath")
        .arg("language")
        .arg("node_type")
        .arg("name")
        .arg("signature")
        .arg("content")
        .arg("parent_context")
        .query_async(conn)
        .await?;

    parse_search_results(result)
}

pub(super) async fn lookup_chunks_by_name_impl(
    conn: &mut redis::aio::MultiplexedConnection,
    index_name: &str,
    names: &[String],
) -> anyhow::Result<Vec<SearchResult>> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let mut results = Vec::new();

    let mut pipe = redis::pipe();
    for name in names {
        let query_str = format!("@name:\"{}\"", escape_exact_match(name));
        pipe.cmd("FT.SEARCH")
            .arg(index_name)
            .arg(&query_str)
            .arg("DIALECT")
            .arg(2)
            .arg("LIMIT")
            .arg(0)
            .arg(3)
            .arg("RETURN")
            .arg(8)
            .arg("chunk_id")
            .arg("filepath")
            .arg("language")
            .arg("node_type")
            .arg("name")
            .arg("signature")
            .arg("content")
            .arg("parent_context");
    }

    let pipe_results: Vec<redis::Value> = pipe.query_async(conn).await?;
    for value in pipe_results {
        results.extend(parse_search_results(value)?);
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::parse_search_results;
    use crate::storage::index::is_index_not_found_error;
    use crate::Language as Language;
    use redis::RedisError;
    use redis::Value;

    #[test]
    fn index_not_found_error_matches_common_variants() {
        let err1 = RedisError::from((redis::ErrorKind::ResponseError, "Unknown: index name"));
        let err2 = RedisError::from((redis::ErrorKind::ResponseError, "Unknown Index name"));
        let err3 = RedisError::from((redis::ErrorKind::ResponseError, "index name not found"));
        let err4 = RedisError::from((redis::ErrorKind::ResponseError, "no such index"));

        assert!(is_index_not_found_error(&err1));
        assert!(is_index_not_found_error(&err2));
        assert!(is_index_not_found_error(&err3));
        assert!(is_index_not_found_error(&err4));
    }

    #[test]
    fn index_not_found_error_rejects_other_messages() {
        let err = RedisError::from((redis::ErrorKind::ResponseError, "syntax error"));
        assert!(!is_index_not_found_error(&err));
    }

    #[test]
    fn parse_search_results_returns_error_for_non_array() {
        let value = Value::Okay;
        let result = parse_search_results(value);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("expected array response"));
    }

    #[test]
    fn parse_search_results_returns_empty_for_empty_array() {
        let value = Value::Array(vec![]);
        let result = parse_search_results(value).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_search_results_returns_error_for_bulk_string_count() {
        let value = Value::Array(vec![Value::BulkString(b"2".to_vec())]);
        let result = parse_search_results(value);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("expected integer"));
    }

    #[test]
    fn parse_search_results_returns_error_for_negative_count() {
        let value = Value::Array(vec![Value::Int(-1)]);
        let result = parse_search_results(value);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("non-negative"));
    }

    #[test]
    fn parse_search_results_returns_error_for_unexpected_type_count() {
        let value = Value::Array(vec![Value::Nil]);
        let result = parse_search_results(value);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("expected integer"));
    }

    #[test]
    fn parse_search_results_parses_valid_response() {
        let value = Value::Array(vec![
            Value::Int(1),
            Value::BulkString(b"doc1".to_vec()),
            Value::Array(vec![
                Value::BulkString(b"chunk_id".to_vec()),
                Value::BulkString(b"chunk1".to_vec()),
                Value::BulkString(b"filepath".to_vec()),
                Value::BulkString(b"src/main.rs".to_vec()),
                Value::BulkString(b"language".to_vec()),
                Value::BulkString(b"rust".to_vec()),
                Value::BulkString(b"node_type".to_vec()),
                Value::BulkString(b"function".to_vec()),
                Value::BulkString(b"name".to_vec()),
                Value::BulkString(b"main".to_vec()),
                Value::BulkString(b"content".to_vec()),
                Value::BulkString(b"fn main() {}".to_vec()),
                Value::BulkString(b"vector_score".to_vec()),
                Value::BulkString(b"0.95".to_vec()),
            ]),
        ]);
        let result = parse_search_results(value).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].chunk_id, "chunk1");
        assert_eq!(result[0].filepath, "src/main.rs");
        assert!(matches!(result[0].language, Language::Rust));
        assert_eq!(result[0].node_type, "function");
        assert_eq!(result[0].name, "main");
        assert_eq!(result[0].content, "fn main() {}");
        assert_eq!(result[0].score, 0.95);
    }

    #[test]
    fn parse_search_results_returns_empty_when_count_is_zero() {
        let value = Value::Array(vec![Value::Int(0)]);
        let result = parse_search_results(value).unwrap();
        assert!(result.is_empty());
    }
}
