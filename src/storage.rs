use crate::{Chunk, Config, KtError, Language, SearchResult};
use redis::cmd;
use tracing::{debug, info, warn};

const INDEX_NAME: &str = "idx:kt_codebase";
const KEY_PREFIX: &str = "kt:doc:";
const SHADOW_INDEX_NAME: &str = "idx:kt_shadow";
const SHADOW_KEY_PREFIX: &str = "kt:shadow:";

#[derive(Debug, Clone)]
pub struct Storage {
    client: redis::Client,
    _config: Config,
}

impl Storage {
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        let client = redis::Client::open(config.redis_url.as_str())?;
        Ok(Self {
            client,
            _config: config.clone(),
        })
    }

    pub async fn connection(&self) -> anyhow::Result<redis::aio::MultiplexedConnection> {
        let conn = self.client.get_multiplexed_async_connection().await?;
        Ok(conn)
    }

    pub async fn ensure_index(&self) -> anyhow::Result<()> {
        let mut conn = self.connection().await?;

        if self.index_exists(&mut conn).await? {
            info!("Index {INDEX_NAME} already exists, skipping creation");
            return Ok(());
        }

        info!("Creating Redis index {INDEX_NAME}");
        let result: redis::RedisResult<redis::Value> = cmd("FT.CREATE")
            .arg(INDEX_NAME)
            .arg("ON")
            .arg("HASH")
            .arg("PREFIX")
            .arg(1)
            .arg(KEY_PREFIX)
            .arg("SCHEMA")
            .arg("chunk_id")
            .arg("TAG")
            .arg("filepath")
            .arg("TEXT")
            .arg("language")
            .arg("TAG")
            .arg("node_type")
            .arg("TAG")
            .arg("name")
            .arg("TEXT")
            .arg("signature")
            .arg("TEXT")
            .arg("content")
            .arg("TEXT")
            .arg("start_line")
            .arg("NUMERIC")
            .arg("end_line")
            .arg("NUMERIC")
            .arg("parent_context")
            .arg("TEXT")
            .arg("embedding")
            .arg("VECTOR")
            .arg("FLAT")
            .arg(6)
            .arg("TYPE")
            .arg("FLOAT32")
            .arg("DIM")
            .arg(384)
            .arg("DISTANCE_METRIC")
            .arg("COSINE")
            .query_async(&mut conn)
            .await;

        match result {
            Ok(_) => {
                info!("Index {INDEX_NAME} created successfully");
                Ok(())
            }
            Err(e) if e.to_string().contains("Index already exists") => {
                debug!("Index {INDEX_NAME} already exists (race condition)");
                Ok(())
            }
            Err(e) => Err(KtError::Redis(e).into()),
        }
    }

    async fn index_exists(
        &self,
        conn: &mut redis::aio::MultiplexedConnection,
    ) -> anyhow::Result<bool> {
        let result: redis::RedisResult<redis::Value> =
            cmd("FT.INFO").arg(INDEX_NAME).query_async(conn).await;
        match result {
            Ok(_) => Ok(true),
            Err(e) => {
                if is_index_not_found_error(&e)
                    || e.to_string().to_lowercase().contains("not found")
                {
                    Ok(false)
                } else {
                    Err(KtError::Redis(e).into())
                }
            }
        }
    }

    pub async fn store_chunk(&self, chunk: &Chunk, embedding: &[f32]) -> anyhow::Result<()> {
        let mut conn = self.connection().await?;
        let key = format!("{KEY_PREFIX}{}", chunk.chunk_id);
        let embedding_bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();

        debug!("Storing chunk {} at key {}", chunk.name, key);
        let mut cmd = cmd("HSET");
        cmd.arg(&key)
            .arg("chunk_id")
            .arg(&chunk.chunk_id)
            .arg("filepath")
            .arg(&chunk.filepath)
            .arg("language")
            .arg(chunk.language.as_str())
            .arg("node_type")
            .arg(&chunk.node_type)
            .arg("name")
            .arg(&chunk.name)
            .arg("signature")
            .arg(&chunk.signature)
            .arg("content")
            .arg(&chunk.content)
            .arg("start_line")
            .arg(chunk.start_line as i64)
            .arg("end_line")
            .arg(chunk.end_line as i64)
            .arg("embedding")
            .arg(&embedding_bytes);

        if let Some(ref parent_ctx) = chunk.parent_context {
            cmd.arg("parent_context").arg(parent_ctx);
        }

        cmd.query_async::<redis::Value>(&mut conn).await?;
        Ok(())
    }

    pub async fn store_chunks_batch(
        &self,
        chunks: &[Chunk],
        embeddings: &[Vec<f32>],
    ) -> anyhow::Result<()> {
        let mut conn = self.connection().await?;
        let mut pipe = redis::pipe();

        for (chunk, embedding) in chunks.iter().zip(embeddings.iter()) {
            let key = format!("{KEY_PREFIX}{}", chunk.chunk_id);
            let embedding_bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();

            let pipe_cmd = pipe.cmd("HSET");
            pipe_cmd
                .arg(&key)
                .arg("chunk_id")
                .arg(&chunk.chunk_id)
                .arg("filepath")
                .arg(&chunk.filepath)
                .arg("language")
                .arg(chunk.language.as_str())
                .arg("node_type")
                .arg(&chunk.node_type)
                .arg("name")
                .arg(&chunk.name)
                .arg("signature")
                .arg(&chunk.signature)
                .arg("content")
                .arg(&chunk.content)
                .arg("start_line")
                .arg(chunk.start_line as i64)
                .arg("end_line")
                .arg(chunk.end_line as i64)
                .arg("embedding")
                .arg(&embedding_bytes);

            if let Some(ref parent_ctx) = chunk.parent_context {
                pipe_cmd.arg("parent_context").arg(parent_ctx);
            }

            pipe_cmd.ignore();
        }

        pipe.query_async::<redis::Value>(&mut conn).await?;
        info!("Stored {} chunks in batch", chunks.len());
        Ok(())
    }

    pub async fn hybrid_search(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        language: Option<&Language>,
        top_k: usize,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let mut conn = self.connection().await?;
        let embedding_bytes: Vec<u8> = query_embedding
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();

        let effective_query = query_text.trim();
        let query_str = if effective_query.is_empty() {
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
        };

        debug!("Hybrid search query: {}", query_str);

        let result: redis::Value = cmd("FT.SEARCH")
            .arg(INDEX_NAME)
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
            .query_async(&mut conn)
            .await?;

        parse_search_results(result)
    }

    pub async fn read_file_chunks(&self, filepath: &str) -> anyhow::Result<Vec<SearchResult>> {
        if filepath.trim().is_empty() {
            return Ok(Vec::new());
        }

        let mut conn = self.connection().await?;

        let query_str = format!("@filepath:\"{}\"", escape_exact_match(filepath));
        debug!("Reading file chunks: {}", query_str);

        let result: redis::Value = cmd("FT.SEARCH")
            .arg(INDEX_NAME)
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
            .query_async(&mut conn)
            .await?;

        parse_search_results(result)
    }

    pub async fn lookup_chunks_by_name(
        &self,
        names: &[String],
    ) -> anyhow::Result<Vec<SearchResult>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let mut conn = self.connection().await?;
        let mut results = Vec::new();

        for name in names {
            let query_str = format!("@name:\"{}\"", escape_exact_match(name));
            let result: redis::Value = cmd("FT.SEARCH")
                .arg(INDEX_NAME)
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
                .arg("parent_context")
                .query_async(&mut conn)
                .await?;

            results.extend(parse_search_results(result)?);
        }

        Ok(results)
    }

    pub async fn remove_file_chunks(&self, filepath: &str) -> anyhow::Result<usize> {
        let mut conn = self.connection().await?;

        let query_str = format!("@filepath:\"{}\"", escape_exact_match(filepath));
        debug!("Removing chunks for file: {}", query_str);

        let result: redis::Value = cmd("FT.SEARCH")
            .arg(INDEX_NAME)
            .arg(&query_str)
            .arg("DIALECT")
            .arg(2)
            .arg("LIMIT")
            .arg(0)
            .arg(1000)
            .arg("RETURN")
            .arg(1)
            .arg("chunk_id")
            .query_async(&mut conn)
            .await?;

        let keys_to_delete = extract_doc_keys(&result);
        let removed = keys_to_delete.len();

        if !keys_to_delete.is_empty() {
            let mut pipe = redis::pipe();
            for key in &keys_to_delete {
                pipe.cmd("DEL").arg(key).ignore();
            }
            pipe.query_async::<redis::Value>(&mut conn).await?;
        }

        debug!("Removed {removed} chunks for file {filepath}");
        Ok(removed)
    }

    pub async fn get_indexed_files(&self) -> anyhow::Result<Vec<String>> {
        let mut conn = self.connection().await?;
        let mut files = std::collections::HashSet::new();
        let pattern = format!("{KEY_PREFIX}*");
        let mut cursor: u64 = 0;

        loop {
            let (new_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(100)
                .query_async(&mut conn)
                .await?;

            for key in &keys {
                let filepath: Option<String> = redis::cmd("HGET")
                    .arg(key)
                    .arg("filepath")
                    .query_async(&mut conn)
                    .await?;
                if let Some(fp) = filepath {
                    files.insert(fp);
                }
            }

            cursor = new_cursor;
            if cursor == 0 {
                break;
            }
        }

        Ok(files.into_iter().collect())
    }

    pub async fn get_file_mtimes(
        &self,
    ) -> anyhow::Result<std::collections::HashMap<String, String>> {
        let mut conn = self.connection().await?;
        let mut mtimes = std::collections::HashMap::new();
        let pattern = format!("{KEY_PREFIX}*");
        let mut cursor: u64 = 0;

        loop {
            let (new_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(100)
                .query_async(&mut conn)
                .await?;

            for key in &keys {
                let filepath: Option<String> = redis::cmd("HGET")
                    .arg(key)
                    .arg("filepath")
                    .query_async(&mut conn)
                    .await?;
                let mtime: Option<String> = redis::cmd("HGET")
                    .arg(key)
                    .arg("mtime")
                    .query_async(&mut conn)
                    .await?;
                if let (Some(fp), Some(mt)) = (filepath, mtime) {
                    mtimes.entry(fp).or_insert(mt);
                }
            }

            cursor = new_cursor;
            if cursor == 0 {
                break;
            }
        }

        Ok(mtimes)
    }

    pub async fn store_chunk_with_mtime(
        &self,
        chunk: &Chunk,
        embedding: &[f32],
        mtime: &str,
    ) -> anyhow::Result<()> {
        let mut conn = self.connection().await?;
        let key = format!("{KEY_PREFIX}{}", chunk.chunk_id);
        let embedding_bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();

        let mut cmd = cmd("HSET");
        cmd.arg(&key)
            .arg("chunk_id")
            .arg(&chunk.chunk_id)
            .arg("filepath")
            .arg(&chunk.filepath)
            .arg("language")
            .arg(chunk.language.as_str())
            .arg("node_type")
            .arg(&chunk.node_type)
            .arg("name")
            .arg(&chunk.name)
            .arg("signature")
            .arg(&chunk.signature)
            .arg("content")
            .arg(&chunk.content)
            .arg("start_line")
            .arg(chunk.start_line as i64)
            .arg("end_line")
            .arg(chunk.end_line as i64)
            .arg("embedding")
            .arg(&embedding_bytes)
            .arg("mtime")
            .arg(mtime);

        if let Some(ref parent_ctx) = chunk.parent_context {
            cmd.arg("parent_context").arg(parent_ctx);
        }

        cmd.query_async::<redis::Value>(&mut conn).await?;
        Ok(())
    }

    pub async fn ensure_shadow_index(&self) -> anyhow::Result<()> {
        let mut conn = self.connection().await?;

        if self.shadow_index_exists(&mut conn).await? {
            info!("Shadow index {SHADOW_INDEX_NAME} already exists, skipping creation");
            return Ok(());
        }

        info!("Creating shadow index {SHADOW_INDEX_NAME}");
        let result: redis::RedisResult<redis::Value> = cmd("FT.CREATE")
            .arg(SHADOW_INDEX_NAME)
            .arg("ON")
            .arg("HASH")
            .arg("PREFIX")
            .arg(1)
            .arg(SHADOW_KEY_PREFIX)
            .arg("SCHEMA")
            .arg("chunk_id")
            .arg("TAG")
            .arg("filepath")
            .arg("TEXT")
            .arg("language")
            .arg("TAG")
            .arg("node_type")
            .arg("TAG")
            .arg("name")
            .arg("TEXT")
            .arg("signature")
            .arg("TEXT")
            .arg("content")
            .arg("TEXT")
            .arg("start_line")
            .arg("NUMERIC")
            .arg("end_line")
            .arg("NUMERIC")
            .arg("parent_context")
            .arg("TEXT")
            .arg("embedding")
            .arg("VECTOR")
            .arg("FLAT")
            .arg(6)
            .arg("TYPE")
            .arg("FLOAT32")
            .arg("DIM")
            .arg(384)
            .arg("DISTANCE_METRIC")
            .arg("COSINE")
            .query_async(&mut conn)
            .await;

        match result {
            Ok(_) => {
                info!("Shadow index {SHADOW_INDEX_NAME} created successfully");
                Ok(())
            }
            Err(e) if e.to_string().contains("Index already exists") => {
                debug!("Shadow index {SHADOW_INDEX_NAME} already exists (race condition)");
                Ok(())
            }
            Err(e) => Err(KtError::Redis(e).into()),
        }
    }

    async fn shadow_index_exists(
        &self,
        conn: &mut redis::aio::MultiplexedConnection,
    ) -> anyhow::Result<bool> {
        let result: redis::RedisResult<redis::Value> = cmd("FT.INFO")
            .arg(SHADOW_INDEX_NAME)
            .query_async(conn)
            .await;
        match result {
            Ok(_) => Ok(true),
            Err(e) => {
                if is_index_not_found_error(&e)
                    || e.to_string().to_lowercase().contains("not found")
                {
                    Ok(false)
                } else {
                    Err(KtError::Redis(e).into())
                }
            }
        }
    }

    pub async fn store_shadow_chunks_batch(
        &self,
        chunks: &[Chunk],
        embeddings: &[Vec<f32>],
        ttl_seconds: u64,
    ) -> anyhow::Result<()> {
        let mut conn = self.connection().await?;
        let mut pipe = redis::pipe();

        for (chunk, embedding) in chunks.iter().zip(embeddings.iter()) {
            let key = format!("{SHADOW_KEY_PREFIX}{}", chunk.chunk_id);
            let embedding_bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();

            let pipe_cmd = pipe.cmd("HSET");
            pipe_cmd
                .arg(&key)
                .arg("chunk_id")
                .arg(&chunk.chunk_id)
                .arg("filepath")
                .arg(&chunk.filepath)
                .arg("language")
                .arg(chunk.language.as_str())
                .arg("node_type")
                .arg(&chunk.node_type)
                .arg("name")
                .arg(&chunk.name)
                .arg("signature")
                .arg(&chunk.signature)
                .arg("content")
                .arg(&chunk.content)
                .arg("start_line")
                .arg(chunk.start_line as i64)
                .arg("end_line")
                .arg(chunk.end_line as i64)
                .arg("embedding")
                .arg(&embedding_bytes);

            if let Some(ref parent_ctx) = chunk.parent_context {
                pipe_cmd.arg("parent_context").arg(parent_ctx);
            }

            pipe_cmd.ignore();

            pipe.cmd("EXPIRE").arg(&key).arg(ttl_seconds).ignore();
        }

        pipe.query_async::<redis::Value>(&mut conn).await?;
        info!(
            "Stored {} shadow chunks with TTL {}s",
            chunks.len(),
            ttl_seconds
        );
        Ok(())
    }

    pub async fn search_shadow(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        language: Option<&Language>,
        top_k: usize,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let mut conn = self.connection().await?;
        let embedding_bytes: Vec<u8> = query_embedding
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();

        let effective_query = query_text.trim();
        let query_str = if effective_query.is_empty() {
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
        };

        debug!("Shadow search query: {}", query_str);

        let result: redis::Value = cmd("FT.SEARCH")
            .arg(SHADOW_INDEX_NAME)
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
            .query_async(&mut conn)
            .await?;

        parse_search_results(result)
    }

    pub async fn read_shadow_file_chunks(
        &self,
        filepath: &str,
    ) -> anyhow::Result<Vec<SearchResult>> {
        if filepath.trim().is_empty() {
            return Ok(Vec::new());
        }

        let mut conn = self.connection().await?;

        let query_str = format!("@filepath:\"{}\"", escape_exact_match(filepath));
        debug!("Reading shadow file chunks: {}", query_str);

        let result: redis::Value = cmd("FT.SEARCH")
            .arg(SHADOW_INDEX_NAME)
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
            .query_async(&mut conn)
            .await?;

        parse_search_results(result)
    }
}

fn parse_search_results(value: redis::Value) -> anyhow::Result<Vec<SearchResult>> {
    let mut results = Vec::new();

    let arr = match value {
        redis::Value::Array(a) => a,
        _ => return Ok(results),
    };

    if arr.is_empty() {
        return Ok(results);
    }

    let total_count = match &arr[0] {
        redis::Value::Int(n) => *n as usize,
        redis::Value::BulkString(bs) => String::from_utf8_lossy(bs).parse().unwrap_or(0),
        _ => return Ok(results),
    };

    if total_count == 0 {
        return Ok(results);
    }

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

fn is_index_not_found_error(err: &redis::RedisError) -> bool {
    let msg = err.to_string().to_lowercase();
    msg.contains("unknown index") || msg.contains("index name") && msg.contains("unknown")
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

fn escape_exact_match(path: &str) -> String {
    path.replace('\\', "\\\\").replace('"', "\\\"")
}

fn extract_doc_keys(value: &redis::Value) -> Vec<String> {
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
            if key.starts_with(KEY_PREFIX) {
                keys.push(key.into_owned());
            }
        }
        i += 2;
    }

    keys
}
