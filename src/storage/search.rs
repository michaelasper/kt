use crate::{Language, SearchResult};
use redis::{cmd, Cmd, Pipeline};
use std::cmp::Ordering;
use tracing::{debug, warn};

const SEARCH_RETURN_FIELDS: [&str; 11] = [
    "chunk_id",
    "codebase_id",
    "filepath",
    "language",
    "node_type",
    "name",
    "signature",
    "content",
    "parent_context",
    "start_line",
    "end_line",
];
const READ_FILE_PAGE_SIZE: usize = 1000;

struct SearchPage {
    total_count: usize,
    results: Vec<SearchResult>,
}

pub fn parse_search_results(value: redis::Value) -> anyhow::Result<Vec<SearchResult>> {
    Ok(parse_search_page(value)?.results)
}

fn parse_search_page(value: redis::Value) -> anyhow::Result<SearchPage> {
    let arr = match value {
        redis::Value::Array(a) => a,
        other => anyhow::bail!("FT.SEARCH expected array response, got {:?}", other),
    };

    if arr.is_empty() {
        return Ok(SearchPage {
            total_count: 0,
            results: Vec::new(),
        });
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
        return Ok(SearchPage {
            total_count,
            results: Vec::new(),
        });
    }

    let mut results = Vec::new();
    let mut i = 1;
    while i + 1 < arr.len() {
        let fields = &arr[i + 1];

        let mut chunk_id = String::new();
        let mut codebase_id = String::new();
        let mut filepath = String::new();
        let mut language = Language::Rust;
        let mut node_type = String::new();
        let mut name = String::new();
        let mut signature = String::new();
        let mut content = String::new();
        let mut parent_context: Option<String> = None;
        let mut score = 0.0f64;
        let mut start_line: Option<usize> = None;
        let mut end_line: Option<usize> = None;

        if let redis::Value::Array(field_pairs) = fields {
            let mut j = 0;
            while j + 1 < field_pairs.len() {
                if let redis::Value::BulkString(key_bytes) = &field_pairs[j] {
                    let key = String::from_utf8_lossy(key_bytes);
                    match key.as_ref() {
                        "chunk_id" => {
                            if let Some(val) = parse_string_value(&field_pairs[j + 1]) {
                                chunk_id = val;
                            }
                        }
                        "codebase_id" => {
                            if let Some(val) = parse_string_value(&field_pairs[j + 1]) {
                                codebase_id = val;
                            }
                        }
                        "filepath" => {
                            if let Some(val) = parse_string_value(&field_pairs[j + 1]) {
                                filepath = val;
                            }
                        }
                        "language" => {
                            if let Some(val) = parse_string_value(&field_pairs[j + 1]) {
                                language = parse_language(&val);
                            }
                        }
                        "node_type" => {
                            if let Some(val) = parse_string_value(&field_pairs[j + 1]) {
                                node_type = val;
                            }
                        }
                        "name" => {
                            if let Some(val) = parse_string_value(&field_pairs[j + 1]) {
                                name = val;
                            }
                        }
                        "signature" => {
                            if let Some(val) = parse_string_value(&field_pairs[j + 1]) {
                                signature = val;
                            }
                        }
                        "content" => {
                            if let Some(val) = parse_string_value(&field_pairs[j + 1]) {
                                content = val;
                            }
                        }
                        "parent_context" => {
                            if let Some(val) = parse_string_value(&field_pairs[j + 1]) {
                                if !val.is_empty() {
                                    parent_context = Some(val);
                                }
                            }
                        }
                        "vector_score" => {
                            if let Some(val) = parse_string_value(&field_pairs[j + 1]) {
                                score = val.parse().unwrap_or(0.0);
                            }
                        }
                        "start_line" => {
                            start_line =
                                Some(parse_line_number("start_line", &field_pairs[j + 1])?);
                        }
                        "end_line" => {
                            end_line = Some(parse_line_number("end_line", &field_pairs[j + 1])?);
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
                codebase_id,
                codebase_alias: None,
                root_path: String::new(),
                filepath,
                language,
                node_type,
                name,
                signature,
                content,
                parent_context,
                score,
                start_line,
                end_line,
            });
        }

        i += 2;
    }

    Ok(SearchPage {
        total_count,
        results,
    })
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

fn parse_string_value(value: &redis::Value) -> Option<String> {
    match value {
        redis::Value::BulkString(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        redis::Value::SimpleString(s) => Some(s.clone()),
        redis::Value::Int(n) => Some(n.to_string()),
        redis::Value::Double(n) => Some(n.to_string()),
        _ => None,
    }
}

fn parse_line_number(field_name: &str, value: &redis::Value) -> anyhow::Result<usize> {
    match value {
        redis::Value::Int(n) if *n >= 0 => Ok(*n as usize),
        redis::Value::Int(n) => anyhow::bail!("{field_name} must be non-negative, got {n}"),
        redis::Value::BulkString(bytes) => {
            let raw = String::from_utf8_lossy(bytes);
            raw.parse::<usize>()
                .map_err(|_| anyhow::anyhow!("Invalid {field_name}: {raw:?}"))
        }
        redis::Value::SimpleString(raw) => raw
            .parse::<usize>()
            .map_err(|_| anyhow::anyhow!("Invalid {field_name}: {raw:?}")),
        other => anyhow::bail!("Invalid {field_name}: expected integer, got {:?}", other),
    }
}

fn append_search_return_fields(cmd: &mut Cmd) {
    cmd.arg("RETURN").arg(SEARCH_RETURN_FIELDS.len());
    for field in SEARCH_RETURN_FIELDS {
        cmd.arg(field);
    }
}

fn append_search_return_fields_to_pipeline(pipe: &mut Pipeline) {
    pipe.arg("RETURN").arg(SEARCH_RETURN_FIELDS.len());
    for field in SEARCH_RETURN_FIELDS {
        pipe.arg(field);
    }
}

fn compare_source_order(a: &SearchResult, b: &SearchResult) -> Ordering {
    (a.start_line, a.end_line, &a.chunk_id).cmp(&(b.start_line, b.end_line, &b.chunk_id))
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

fn tag_filter(field: &str, value: &str) -> String {
    format!("@{}:{{{}}}", field, escape_tag_value(value))
}

pub(crate) fn build_codebase_filter(codebase_id: &str) -> String {
    tag_filter("codebase_id", codebase_id)
}

fn escape_tag_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            escaped.push(ch);
        } else {
            escaped.push('\\');
            escaped.push(ch);
        }
    }
    escaped
}

fn build_scope_filters(language: Option<&Language>, codebase_id: Option<&str>) -> Vec<String> {
    let mut filters = Vec::new();
    if let Some(id) = codebase_id {
        filters.push(build_codebase_filter(id));
    }
    if let Some(lang) = language {
        filters.push(tag_filter("language", lang.as_str()));
    }
    filters
}

fn combine_filters_and_text(filters: &[String], text_query: Option<String>) -> String {
    match (filters.is_empty(), text_query) {
        (true, None) => "*".to_string(),
        (true, Some(text)) => format!("({text})"),
        (false, None) if filters.len() == 1 => filters[0].clone(),
        (false, None) => format!("({})", filters.join(" ")),
        (false, Some(text)) => format!("({} ({text}))", filters.join(" ")),
    }
}

pub(crate) fn build_hybrid_query(
    query_text: &str,
    language: Option<&Language>,
    codebase_id: Option<&str>,
    top_k: usize,
) -> String {
    let effective_query = query_text.trim();
    let filters = build_scope_filters(language, codebase_id);
    let text_query = if effective_query.is_empty() {
        None
    } else {
        Some(escape_fts_query(effective_query))
    };
    let base = combine_filters_and_text(&filters, text_query);
    format!("{base}=>[KNN {top_k} @embedding $query_vec AS vector_score]")
}

pub(crate) fn build_file_query(filepath: &str, codebase_id: Option<&str>) -> String {
    let filepath_query = format!("@filepath:\"{}\"", escape_exact_match(filepath));
    if let Some(id) = codebase_id {
        format!("({} {})", build_codebase_filter(id), filepath_query)
    } else {
        filepath_query
    }
}

fn build_name_query(name: &str, codebase_id: Option<&str>) -> String {
    let name_query = format!("@name:\"{}\"", escape_exact_match(name));
    if let Some(id) = codebase_id {
        format!("({} {})", build_codebase_filter(id), name_query)
    } else {
        name_query
    }
}

pub(super) async fn hybrid_search_impl(
    conn: &mut redis::aio::MultiplexedConnection,
    index_name: &str,
    query_embedding: &[f32],
    query_text: &str,
    language: Option<&Language>,
    codebase_id: Option<&str>,
    top_k: usize,
) -> anyhow::Result<Vec<SearchResult>> {
    let embedding_bytes: Vec<u8> = query_embedding
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();

    let query_str = build_hybrid_query(query_text, language, codebase_id, top_k);
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
    codebase_id: Option<&str>,
) -> anyhow::Result<Vec<SearchResult>> {
    if filepath.trim().is_empty() {
        return Ok(Vec::new());
    }

    let query_str = build_file_query(filepath, codebase_id);
    debug!("Reading file chunks: {}", query_str);

    match read_file_chunks_paged(conn, index_name, &query_str, true).await {
        Ok(results) => Ok(results),
        Err(err) if is_sortby_start_line_error(&err) => {
            debug!(
                "SORTBY start_line unavailable for {index_name}, retrying without SORTBY: {err}"
            );
            read_file_chunks_paged(conn, index_name, &query_str, false).await
        }
        Err(err) => Err(err),
    }
}

async fn read_file_chunks_paged(
    conn: &mut redis::aio::MultiplexedConnection,
    index_name: &str,
    query_str: &str,
    sort_by_start_line: bool,
) -> anyhow::Result<Vec<SearchResult>> {
    let mut results = Vec::new();
    let mut offset = 0usize;

    loop {
        let page = read_file_chunks_page(
            conn,
            index_name,
            query_str,
            offset,
            READ_FILE_PAGE_SIZE,
            sort_by_start_line,
        )
        .await?;

        let page_count = page.results.len();
        let total_count = page.total_count;
        results.extend(page.results);

        if total_count == 0 || page_count == 0 || results.len() >= total_count {
            break;
        }

        offset += READ_FILE_PAGE_SIZE;
    }

    results.sort_by(compare_source_order);
    Ok(results)
}

async fn read_file_chunks_page(
    conn: &mut redis::aio::MultiplexedConnection,
    index_name: &str,
    query_str: &str,
    offset: usize,
    page_size: usize,
    sort_by_start_line: bool,
) -> anyhow::Result<SearchPage> {
    let mut command = cmd("FT.SEARCH");
    command.arg(index_name).arg(query_str).arg("DIALECT").arg(2);
    if sort_by_start_line {
        command.arg("SORTBY").arg("start_line").arg("ASC");
    }
    command.arg("LIMIT").arg(offset).arg(page_size);
    append_search_return_fields(&mut command);

    let result: redis::Value = command.query_async(conn).await?;
    parse_search_page(result)
}

fn is_sortby_start_line_error(err: &anyhow::Error) -> bool {
    let msg = err.to_string().to_lowercase();
    msg.contains("sortby") || (msg.contains("start_line") && msg.contains("sortable"))
}

pub(super) async fn lookup_chunks_by_name_impl(
    conn: &mut redis::aio::MultiplexedConnection,
    index_name: &str,
    names: &[String],
    codebase_id: Option<&str>,
) -> anyhow::Result<Vec<SearchResult>> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let mut results = Vec::new();

    let mut pipe = redis::pipe();
    for name in names {
        let query_str = build_name_query(name, codebase_id);
        pipe.cmd("FT.SEARCH")
            .arg(index_name)
            .arg(&query_str)
            .arg("DIALECT")
            .arg(2)
            .arg("LIMIT")
            .arg(0)
            .arg(3);
        append_search_return_fields_to_pipeline(&mut pipe);
    }

    let pipe_results: Vec<redis::Value> = pipe.query_async(conn).await?;
    for value in pipe_results {
        results.extend(parse_search_results(value)?);
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::{build_file_query, build_hybrid_query, parse_search_results};
    use crate::storage::index::is_index_not_found_error;
    use crate::Language;
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
        assert_eq!(result[0].codebase_id, "");
        assert_eq!(result[0].filepath, "src/main.rs");
        assert!(matches!(result[0].language, Language::Rust));
        assert_eq!(result[0].node_type, "function");
        assert_eq!(result[0].name, "main");
        assert_eq!(result[0].content, "fn main() {}");
        assert_eq!(result[0].score, 0.95);
    }

    #[test]
    fn parse_search_results_parses_codebase_id() {
        let value = Value::Array(vec![
            Value::Int(1),
            Value::BulkString(b"doc1".to_vec()),
            Value::Array(vec![
                Value::BulkString(b"chunk_id".to_vec()),
                Value::BulkString(b"chunk1".to_vec()),
                Value::BulkString(b"codebase_id".to_vec()),
                Value::BulkString(b"codebase-a".to_vec()),
                Value::BulkString(b"filepath".to_vec()),
                Value::BulkString(b"src/main.rs".to_vec()),
            ]),
        ]);

        let result = parse_search_results(value).unwrap();

        assert_eq!(result[0].codebase_id, "codebase-a");
    }

    #[test]
    fn build_hybrid_query_omits_codebase_filter_when_unscoped() {
        let query = build_hybrid_query("function", None, None, 3);

        assert!(!query.contains("@codebase_id"));
        assert_eq!(
            query,
            "(function)=>[KNN 3 @embedding $query_vec AS vector_score]"
        );
    }

    #[test]
    fn build_hybrid_query_includes_codebase_filter_when_scoped() {
        let query = build_hybrid_query("function", Some(&Language::Rust), Some("abc123"), 3);

        assert!(query.contains("@codebase_id:{abc123}"));
        assert!(query.contains("@language:{rust}"));
        assert!(query.contains("(function)"));
    }

    #[test]
    fn build_file_query_scopes_filepath_when_codebase_id_is_supplied() {
        let query = build_file_query("src/lib.rs", Some("abc123"));

        assert_eq!(query, "(@codebase_id:{abc123} @filepath:\"src/lib.rs\")");
    }

    #[test]
    fn parse_search_results_parses_line_ranges_from_bulk_strings() {
        let value = Value::Array(vec![
            Value::Int(1),
            Value::BulkString(b"doc1".to_vec()),
            Value::Array(vec![
                Value::BulkString(b"chunk_id".to_vec()),
                Value::BulkString(b"chunk1".to_vec()),
                Value::BulkString(b"filepath".to_vec()),
                Value::BulkString(b"src/main.rs".to_vec()),
                Value::BulkString(b"start_line".to_vec()),
                Value::BulkString(b"12".to_vec()),
                Value::BulkString(b"end_line".to_vec()),
                Value::BulkString(b"34".to_vec()),
            ]),
        ]);

        let result = parse_search_results(value).unwrap();

        assert_eq!(result[0].start_line, Some(12));
        assert_eq!(result[0].end_line, Some(34));
    }

    #[test]
    fn parse_search_results_parses_line_ranges_from_integer_values() {
        let value = Value::Array(vec![
            Value::Int(1),
            Value::BulkString(b"doc1".to_vec()),
            Value::Array(vec![
                Value::BulkString(b"chunk_id".to_vec()),
                Value::BulkString(b"chunk1".to_vec()),
                Value::BulkString(b"filepath".to_vec()),
                Value::BulkString(b"src/main.rs".to_vec()),
                Value::BulkString(b"start_line".to_vec()),
                Value::Int(12),
                Value::BulkString(b"end_line".to_vec()),
                Value::Int(34),
            ]),
        ]);

        let result = parse_search_results(value).unwrap();

        assert_eq!(result[0].start_line, Some(12));
        assert_eq!(result[0].end_line, Some(34));
    }

    #[test]
    fn parse_search_results_returns_error_for_invalid_line_range() {
        let value = Value::Array(vec![
            Value::Int(1),
            Value::BulkString(b"doc1".to_vec()),
            Value::Array(vec![
                Value::BulkString(b"chunk_id".to_vec()),
                Value::BulkString(b"chunk1".to_vec()),
                Value::BulkString(b"filepath".to_vec()),
                Value::BulkString(b"src/main.rs".to_vec()),
                Value::BulkString(b"start_line".to_vec()),
                Value::BulkString(b"not-a-number".to_vec()),
            ]),
        ]);

        let result = parse_search_results(value);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("start_line"));
    }

    #[test]
    fn parse_search_results_returns_empty_when_count_is_zero() {
        let value = Value::Array(vec![Value::Int(0)]);
        let result = parse_search_results(value).unwrap();
        assert!(result.is_empty());
    }
}
