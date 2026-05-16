use crate::{CallRef, FileRole, Language, SearchResult};
use redis::{cmd, Cmd, Pipeline};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use tracing::{debug, warn};

const SEARCH_RETURN_FIELDS: [&str; 13] = [
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
    "file_role",
    "calls",
];
const READ_FILE_PAGE_SIZE: usize = 1000;
const RRF_CONSTANT: f64 = 60.0;

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
        let doc_key =
            parse_string_value(&arr[i]).unwrap_or_else(|| format!("<result {}>", (i - 1) / 2));
        let fields = &arr[i + 1];

        let mut chunk_id = String::new();
        let mut codebase_id = String::new();
        let mut filepath = String::new();
        let mut language: Option<Language> = None;
        let mut node_type = String::new();
        let mut name = String::new();
        let mut signature = String::new();
        let mut content = String::new();
        let mut parent_context: Option<String> = None;
        let mut file_role = crate::FileRole::Implementation;
        let mut score = 0.0f64;
        let mut start_line: Option<usize> = None;
        let mut end_line: Option<usize> = None;
        let mut calls = String::new();

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
                                language = Some(val.parse::<Language>().map_err(|err| {
                                    anyhow::anyhow!(
                                        "{err} in FT.SEARCH result for document {doc_key}"
                                    )
                                })?);
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
                        "file_role" => {
                            if let Some(val) = parse_string_value(&field_pairs[j + 1]) {
                                file_role = crate::FileRole::parse(&val)
                                    .unwrap_or(crate::FileRole::Implementation);
                            }
                        }
                        "calls" => {
                            if let Some(val) = parse_string_value(&field_pairs[j + 1]) {
                                calls = val;
                            }
                        }
                        _ => {}
                    }
                }
                j += 2;
            }
        }

        if !filepath.is_empty() {
            let language = language.ok_or_else(|| {
                anyhow::anyhow!(
                    "FT.SEARCH result for document {doc_key} ({filepath}) is missing required language field"
                )
            })?;
            let parsed_calls = if calls.is_empty() {
                Vec::new()
            } else {
                calls
                    .split_whitespace()
                    .filter_map(|s| {
                        if let Some(pos) = s.find("::") {
                            let receiver = s[..pos].to_string();
                            let name = s[pos + 2..].to_string();
                            if !name.is_empty() {
                                Some(CallRef {
                                    name,
                                    receiver: Some(receiver),
                                })
                            } else {
                                None
                            }
                        } else {
                            Some(CallRef {
                                name: s.to_string(),
                                receiver: None,
                            })
                        }
                    })
                    .collect()
            };
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
                file_role,
                calls: parsed_calls,
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
    let mut result = String::with_capacity(path.len());
    for ch in path.chars() {
        match ch {
            '\\' | '"' | '\'' | '(' | ')' | '-' | '!' | '@' | '$' | ':' | '{' | '}' | '[' | ']'
            | '~' | '%' | '^' | '&' | '#' | '<' | '>' | '|' | ';' | ',' => {
                result.push('\\');
                result.push(ch);
            }
            _ => result.push(ch),
        }
    }
    result
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
    let mut result = String::with_capacity(query.len() * 2);
    for ch in query.chars() {
        if ch.is_control() {
            continue;
        }
        match ch {
            '\\' | '"' | '\'' | '(' | ')' | ':' | '{' | '}' | '|' | '@' | '!' | '-' | '*' | '['
            | ']' | ';' | ',' | '.' | '~' | '%' | '^' | '&' | '#' | '<' | '>' | '/' | '$' | '?'
            | '=' | '+' => {
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

fn build_scope_filters(
    language: Option<&Language>,
    codebase_id: Option<&str>,
    file_role: Option<&FileRole>,
) -> Vec<String> {
    let mut filters = Vec::new();
    if let Some(id) = codebase_id {
        filters.push(build_codebase_filter(id));
    }
    if let Some(lang) = language {
        filters.push(tag_filter("language", lang.as_str()));
    }
    if let Some(role) = file_role {
        filters.push(tag_filter("file_role", role.as_str()));
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

pub(crate) fn candidate_limit(top_k: usize) -> usize {
    if top_k == 0 {
        0
    } else {
        top_k.saturating_mul(20).clamp(100, 1000)
    }
}

pub(crate) fn build_semantic_query(
    query_text: &str,
    language: Option<&Language>,
    codebase_id: Option<&str>,
    file_role: Option<&FileRole>,
    candidate_limit: usize,
) -> String {
    let _ = query_text;
    let filters = build_scope_filters(language, codebase_id, file_role);
    let base = combine_filters_and_text(&filters, None);
    format!("{base}=>[KNN {candidate_limit} @embedding $query_vec AS vector_score]")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LexicalMode {
    And,
    Or,
}

pub(crate) fn build_lexical_query(
    query_text: &str,
    language: Option<&Language>,
    codebase_id: Option<&str>,
    file_role: Option<&FileRole>,
    mode: LexicalMode,
) -> Option<String> {
    let effective_query = query_text.trim();
    if effective_query.is_empty() {
        return None;
    }

    let filters = build_scope_filters(language, codebase_id, file_role);
    let escaped = escape_fts_query(effective_query);

    let processed_query = if mode == LexicalMode::Or {
        escaped.split_whitespace().collect::<Vec<_>>().join(" | ")
    } else {
        escaped
    };

    Some(combine_filters_and_text(&filters, Some(processed_query)))
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

#[allow(clippy::too_many_arguments)]
pub(super) async fn hybrid_search_impl(
    conn: &mut redis::aio::MultiplexedConnection,
    index_name: &str,
    query_embedding: &[f32],
    query_text: &str,
    language: Option<&Language>,
    codebase_id: Option<&str>,
    file_role: Option<&FileRole>,
    top_k: usize,
) -> anyhow::Result<Vec<SearchResult>> {
    if top_k == 0 {
        return Ok(Vec::new());
    }

    let limit = candidate_limit(top_k);
    let embedding_bytes: Vec<u8> = query_embedding
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();

    // Stage 1: Strict Hybrid (AND)
    let results = execute_hybrid_lane(
        conn,
        HybridLaneParams {
            index_name,
            embedding_bytes: &embedding_bytes,
            query_text,
            language,
            codebase_id,
            file_role,
            limit,
            top_k,
            mode: LexicalMode::And,
        },
    )
    .await?;

    if !results.is_empty() {
        return Ok(results);
    }

    // Stage 2: Relaxed Hybrid (OR)
    debug!("Stage 1 (AND) returned 0 results, falling back to Stage 2 (OR)");
    let results = execute_hybrid_lane(
        conn,
        HybridLaneParams {
            index_name,
            embedding_bytes: &embedding_bytes,
            query_text,
            language,
            codebase_id,
            file_role,
            limit,
            top_k,
            mode: LexicalMode::Or,
        },
    )
    .await?;

    let thresholded: Vec<_> = results.into_iter().filter(|r| r.score < 0.6).collect();

    if !thresholded.is_empty() {
        return Ok(thresholded);
    }

    // Stage 3: Pure Semantic
    debug!("Stage 2 (OR) returned 0 results, falling back to Stage 3 (Pure Semantic)");
    let semantic_query = build_semantic_query(query_text, language, codebase_id, file_role, limit);
    let semantic_result: redis::Value = cmd("FT.SEARCH")
        .arg(index_name)
        .arg(&semantic_query)
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
        .arg(limit)
        .query_async(conn)
        .await?;

    let results = parse_search_results(semantic_result)?;
    Ok(results
        .into_iter()
        .filter(|r| r.score < 0.6)
        .take(top_k)
        .collect())
}

struct HybridLaneParams<'a> {
    index_name: &'a str,
    embedding_bytes: &'a [u8],
    query_text: &'a str,
    language: Option<&'a Language>,
    codebase_id: Option<&'a str>,
    file_role: Option<&'a FileRole>,
    limit: usize,
    top_k: usize,
    mode: LexicalMode,
}

async fn execute_hybrid_lane(
    conn: &mut redis::aio::MultiplexedConnection,
    params: HybridLaneParams<'_>,
) -> anyhow::Result<Vec<SearchResult>> {
    let semantic_query = build_semantic_query(
        params.query_text,
        params.language,
        params.codebase_id,
        params.file_role,
        params.limit,
    );

    let mut pipe = redis::pipe();
    pipe.cmd("FT.SEARCH")
        .arg(params.index_name)
        .arg(&semantic_query)
        .arg("PARAMS")
        .arg(2)
        .arg("query_vec")
        .arg(params.embedding_bytes)
        .arg("DIALECT")
        .arg(2)
        .arg("SORTBY")
        .arg("vector_score")
        .arg("ASC")
        .arg("LIMIT")
        .arg(0)
        .arg(params.limit);
    append_search_return_fields_to_pipeline(&mut pipe);

    let lexical_query = build_lexical_query(
        params.query_text,
        params.language,
        params.codebase_id,
        params.file_role,
        params.mode,
    );
    if let Some(lq) = &lexical_query {
        pipe.cmd("FT.SEARCH")
            .arg(params.index_name)
            .arg(lq)
            .arg("DIALECT")
            .arg(2)
            .arg("LIMIT")
            .arg(0)
            .arg(params.limit);
        append_search_return_fields_to_pipeline(&mut pipe);
    }

    let pipe_results: Vec<redis::Value> = pipe.query_async(conn).await?;
    let semantic_results = parse_search_results(pipe_results[0].clone())?;
    let lexical_results = if lexical_query.is_some() {
        parse_search_results(pipe_results[1].clone())?
    } else {
        Vec::new()
    };

    Ok(fuse_search_lanes(
        semantic_results,
        lexical_results,
        params.top_k,
    ))
}

struct FusedSearchResult {
    result: SearchResult,
    rrf_score: f64,
    first_seen: usize,
}

fn file_role_boost(role: &FileRole) -> f64 {
    match role {
        FileRole::Implementation => 2.0,
        FileRole::Config => 2.0,
        FileRole::Fixture => 1.4,
        FileRole::Generated => 1.2,
        FileRole::Test => 1.0,
    }
}

pub(crate) fn fuse_search_lanes(
    semantic_results: Vec<SearchResult>,
    lexical_results: Vec<SearchResult>,
    top_k: usize,
) -> Vec<SearchResult> {
    if top_k == 0 {
        return Vec::new();
    }

    let mut fused: HashMap<String, FusedSearchResult> = HashMap::new();
    let mut first_seen_counter = 0usize;

    add_rrf_lane(&mut fused, &mut first_seen_counter, semantic_results);
    add_rrf_lane(&mut fused, &mut first_seen_counter, lexical_results);

    let mut results: Vec<FusedSearchResult> = fused.into_values().collect();

    for result in &mut results {
        let boost = file_role_boost(&result.result.file_role);
        result.rrf_score *= boost;
    }

    results.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.first_seen.cmp(&b.first_seen))
            .then_with(|| a.result.chunk_id.cmp(&b.result.chunk_id))
    });
    results.truncate(top_k);

    results
        .into_iter()
        .map(|mut fused| {
            fused.result.score = if fused.rrf_score > 0.0 {
                1.0 / fused.rrf_score
            } else {
                f64::INFINITY
            };
            fused.result
        })
        .collect()
}

fn add_rrf_lane(
    fused: &mut HashMap<String, FusedSearchResult>,
    first_seen_counter: &mut usize,
    lane_results: Vec<SearchResult>,
) {
    let mut seen_in_lane = HashSet::new();

    for (rank, result) in lane_results.into_iter().enumerate() {
        if !seen_in_lane.insert(result.chunk_id.clone()) {
            continue;
        }

        let contribution = 1.0 / (RRF_CONSTANT + rank as f64 + 1.0);
        match fused.get_mut(&result.chunk_id) {
            Some(existing) => existing.rrf_score += contribution,
            None => {
                let first_seen = *first_seen_counter;
                *first_seen_counter += 1;
                fused.insert(
                    result.chunk_id.clone(),
                    FusedSearchResult {
                        result,
                        rrf_score: contribution,
                        first_seen,
                    },
                );
            }
        }
    }
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
    use super::{
        build_file_query, build_lexical_query, build_semantic_query, candidate_limit,
        fuse_search_lanes, parse_search_results, LexicalMode,
    };
    use crate::storage::index::is_index_not_found_error;
    use crate::{FileRole, Language, SearchResult};
    use redis::RedisError;
    use redis::Value;

    fn sample_result(chunk_id: &str) -> SearchResult {
        SearchResult {
            chunk_id: chunk_id.to_string(),
            codebase_id: "codebase-a".to_string(),
            codebase_alias: None,
            root_path: String::new(),
            filepath: format!("src/{chunk_id}.rs"),
            language: Language::Rust,
            node_type: "function".to_string(),
            name: chunk_id.to_string(),
            signature: format!("fn {chunk_id}()"),
            content: String::new(),
            parent_context: None,
            score: 0.0,
            start_line: None,
            end_line: None,
            file_role: crate::FileRole::Implementation,
            calls: Vec::new(),
        }
    }

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
    fn parse_search_results_returns_error_for_unknown_language() {
        let value = Value::Array(vec![
            Value::Int(1),
            Value::BulkString(b"doc1".to_vec()),
            Value::Array(vec![
                Value::BulkString(b"chunk_id".to_vec()),
                Value::BulkString(b"c1".to_vec()),
                Value::BulkString(b"language".to_vec()),
                Value::BulkString(b"cobol".to_vec()),
            ]),
        ]);
        let result = parse_search_results(value);
        assert!(result.is_err());
    }

    #[test]
    fn parse_search_results_returns_error_for_missing_language() {
        let value = Value::Array(vec![
            Value::Int(1),
            Value::BulkString(b"doc1".to_vec()),
            Value::Array(vec![
                Value::BulkString(b"chunk_id".to_vec()),
                Value::BulkString(b"c1".to_vec()),
                Value::BulkString(b"filepath".to_vec()),
                Value::BulkString(b"file1.rs".to_vec()),
            ]),
        ]);
        let result = parse_search_results(value);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("missing required language field"));
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
                Value::BulkString(b"language".to_vec()),
                Value::BulkString(b"rust".to_vec()),
            ]),
        ]);

        let result = parse_search_results(value).unwrap();

        assert_eq!(result[0].codebase_id, "codebase-a");
    }

    #[test]
    fn build_semantic_query_uses_wildcard_when_unscoped() {
        let query = build_semantic_query("how does auth work", None, None, None, 25);

        assert!(!query.contains("@codebase_id"));
        assert!(!query.contains("auth"));
        assert_eq!(query, "*=>[KNN 25 @embedding $query_vec AS vector_score]");
    }

    #[test]
    fn build_semantic_query_uses_only_hard_filters_when_scoped() {
        let query = build_semantic_query(
            "how does auth work",
            Some(&Language::Rust),
            Some("repo:one"),
            None,
            25,
        );

        assert!(query.contains("@codebase_id:{repo\\:one}"));
        assert!(query.contains("@language:{rust}"));
        assert!(!query.contains("auth"));
        assert_eq!(
            query,
            "(@codebase_id:{repo\\:one} @language:{rust})=>[KNN 25 @embedding $query_vec AS vector_score]"
        );
    }

    #[test]
    fn build_lexical_query_escapes_text_and_hard_filters() {
        let query = build_lexical_query(
            "auth: user-role? (admin)",
            Some(&Language::Rust),
            Some("repo:one"),
            None,
            LexicalMode::And,
        )
        .unwrap();

        assert_eq!(
            query,
            "(@codebase_id:{repo\\:one} @language:{rust} (auth\\: user\\-role\\? \\(admin\\)))"
        );
    }

    #[test]
    fn build_lexical_query_skips_empty_text() {
        assert!(build_lexical_query(
            " \t\n",
            Some(&Language::Rust),
            Some("repo"),
            None,
            LexicalMode::And
        )
        .is_none());
    }
    #[test]
    fn build_lexical_query_supports_or_mode() {
        let query = build_lexical_query(
            "auth user",
            Some(&Language::Rust),
            None,
            None,
            LexicalMode::Or,
        )
        .unwrap();

        assert_eq!(query, "(@language:{rust} (auth | user))");
    }

    #[test]
    fn candidate_limit_scales_top_k_and_caps_work() {
        assert_eq!(candidate_limit(0), 0);
        assert_eq!(candidate_limit(1), 100);
        assert_eq!(candidate_limit(5), 100);
        assert_eq!(candidate_limit(10), 200);
        assert_eq!(candidate_limit(50), 1000);
        assert_eq!(candidate_limit(usize::MAX), 1000);
    }

    #[test]
    fn fuse_search_lanes_rrf_deduplicates_sorts_and_truncates() {
        let semantic = vec![sample_result("a"), sample_result("b"), sample_result("c")];
        let lexical = vec![sample_result("c"), sample_result("a"), sample_result("d")];

        let merged = fuse_search_lanes(semantic, lexical, 3);

        assert_eq!(
            merged
                .iter()
                .map(|r| r.chunk_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "c", "b"]
        );
        assert!(merged[0].score < merged[1].score);
        assert!(merged[1].score < merged[2].score);
    }

    #[test]
    fn fuse_search_lanes_returns_empty_for_zero_top_k() {
        let merged = fuse_search_lanes(vec![sample_result("a")], vec![sample_result("b")], 0);

        assert!(merged.is_empty());
    }

    #[test]
    fn fuse_search_lanes_boosts_implementation_over_test() {
        let impl_result = sample_result("impl_fn");
        let test_result = SearchResult {
            file_role: FileRole::Test,
            ..sample_result("test_fn")
        };

        // test is in semantic lane (first_seen=0), impl is in lexical lane (first_seen=1)
        // Without boost, test wins on first_seen tiebreaker.
        // With boost, impl wins (rrf: impl = 1/61*2.0 vs test = 1/61*1.0)
        let merged = fuse_search_lanes(vec![test_result], vec![impl_result], 2);

        assert_eq!(merged[0].chunk_id, "impl_fn");
        assert_eq!(merged[1].chunk_id, "test_fn");
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
                Value::BulkString(b"language".to_vec()),
                Value::BulkString(b"rust".to_vec()),
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
                Value::BulkString(b"language".to_vec()),
                Value::BulkString(b"rust".to_vec()),
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
    fn test_escape_exact_match_escapes_special_characters() {
        let path = "src/auth(main)-v1.rs";
        let escaped = super::escape_exact_match(path);
        assert_eq!(escaped, "src/auth\\(main\\)\\-v1.rs");
    }

    #[test]
    fn parse_search_results_returns_empty_when_count_is_zero() {
        let value = Value::Array(vec![Value::Int(0)]);
        let result = parse_search_results(value).unwrap();
        assert!(result.is_empty());
    }
}
