mod commands;
mod index;
mod search;

use crate::{Chunk, Codebase, Config, FileRole, Language, SearchResult};
use redis::cmd;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, info};

pub use search::parse_search_results;

#[derive(Debug, Clone)]
pub struct Storage {
    client: redis::Client,
    config: Arc<Config>,
    connection: Arc<tokio::sync::OnceCell<redis::aio::MultiplexedConnection>>,
}

impl Storage {
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        let client = redis::Client::open(config.redis_url.as_str())?;
        Ok(Self {
            client,
            config: Arc::new(config.clone()),
            connection: Arc::new(tokio::sync::OnceCell::new()),
        })
    }

    pub async fn connection(&self) -> anyhow::Result<redis::aio::MultiplexedConnection> {
        let conn = self
            .connection
            .get_or_try_init(|| async {
                tokio::time::timeout(
                    self.config.redis_timeout,
                    self.client.get_multiplexed_async_connection(),
                )
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "Timed out connecting to Redis after {}s",
                        self.config.redis_timeout.as_secs()
                    )
                })?
                .map_err(anyhow::Error::from)
            })
            .await?;
        Ok(conn.clone())
    }

    pub async fn ensure_index(&self) -> anyhow::Result<()> {
        let mut conn = self.connection().await?;
        index::ensure_latest_schema(&mut conn).await?;

        if index::index_exists(&mut conn, index::INDEX_NAME).await? {
            info!(
                "Index {} already exists, skipping creation",
                index::INDEX_NAME
            );
        } else {
            index::create_index(&mut conn, index::INDEX_NAME, index::KEY_PREFIX, true).await?;
        }

        index::alter_add_mtime(&mut conn).await?;
        Ok(())
    }

    pub async fn ensure_shadow_index(&self) -> anyhow::Result<()> {
        let mut conn = self.connection().await?;
        index::ensure_latest_schema(&mut conn).await?;

        if index::index_exists(&mut conn, index::SHADOW_INDEX_NAME).await? {
            info!(
                "Shadow index {} already exists, skipping creation",
                index::SHADOW_INDEX_NAME
            );
            return Ok(());
        }

        index::create_index(
            &mut conn,
            index::SHADOW_INDEX_NAME,
            index::SHADOW_KEY_PREFIX,
            false,
        )
        .await
    }

    pub async fn store_chunk(&self, chunk: &Chunk, embedding: &[f32]) -> anyhow::Result<()> {
        let mut conn = self.connection().await?;
        commands::store_chunk_impl(&mut conn, chunk, embedding, None).await
    }

    pub async fn store_chunks_batch(
        &self,
        chunks: &[Chunk],
        embeddings: &[Vec<f32>],
        mtimes: Option<&[String]>,
    ) -> anyhow::Result<()> {
        let mut conn = self.connection().await?;
        commands::store_chunks_batch_impl(&mut conn, chunks, embeddings, mtimes).await
    }

    pub async fn hybrid_search(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        language: Option<&Language>,
        file_role: Option<&FileRole>,
        top_k: usize,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let mut conn = self.connection().await?;
        let mut results = search::hybrid_search_impl(
            &mut conn,
            index::INDEX_NAME,
            query_embedding,
            query_text,
            language,
            None,
            file_role,
            top_k,
        )
        .await?;
        self.hydrate_codebase_metadata(&mut conn, &mut results)
            .await?;
        Ok(results)
    }

    pub async fn hybrid_search_scoped(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        language: Option<&Language>,
        codebase_id: Option<&str>,
        file_role: Option<&FileRole>,
        top_k: usize,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let mut conn = self.connection().await?;
        let mut results = search::hybrid_search_impl(
            &mut conn,
            index::INDEX_NAME,
            query_embedding,
            query_text,
            language,
            codebase_id,
            file_role,
            top_k,
        )
        .await?;
        self.hydrate_codebase_metadata(&mut conn, &mut results)
            .await?;
        Ok(results)
    }

    pub async fn read_file_chunks(&self, filepath: &str) -> anyhow::Result<Vec<SearchResult>> {
        let mut conn = self.connection().await?;
        let mut results =
            search::read_file_chunks_impl(&mut conn, index::INDEX_NAME, filepath, None).await?;
        self.hydrate_codebase_metadata(&mut conn, &mut results)
            .await?;
        Ok(results)
    }

    pub async fn read_file_chunks_scoped(
        &self,
        filepath: &str,
        codebase_id: Option<&str>,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let mut conn = self.connection().await?;
        let mut results =
            search::read_file_chunks_impl(&mut conn, index::INDEX_NAME, filepath, codebase_id)
                .await?;
        self.hydrate_codebase_metadata(&mut conn, &mut results)
            .await?;
        Ok(results)
    }

    pub async fn lookup_chunks_by_name_scoped(
        &self,
        names: &[String],
        codebase_id: Option<&str>,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let mut conn = self.connection().await?;
        let mut results =
            search::lookup_chunks_by_name_impl(&mut conn, index::INDEX_NAME, names, codebase_id)
                .await?;
        self.hydrate_codebase_metadata(&mut conn, &mut results)
            .await?;
        Ok(results)
    }

    pub async fn remove_file_chunks(&self, filepath: &str) -> anyhow::Result<usize> {
        let mut conn = self.connection().await?;
        commands::remove_file_chunks_impl(&mut conn, None, filepath).await
    }

    pub async fn remove_file_chunks_scoped(
        &self,
        codebase_id: &str,
        filepath: &str,
    ) -> anyhow::Result<usize> {
        let mut conn = self.connection().await?;
        commands::remove_file_chunks_impl(&mut conn, Some(codebase_id), filepath).await
    }

    pub async fn search_shadow(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        language: Option<&Language>,
        file_role: Option<&FileRole>,
        top_k: usize,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let mut conn = self.connection().await?;
        let mut results = shadow_index_or_empty(
            search::hybrid_search_impl(
                &mut conn,
                index::SHADOW_INDEX_NAME,
                query_embedding,
                query_text,
                language,
                None,
                file_role,
                top_k,
            )
            .await,
        )?;
        self.hydrate_codebase_metadata(&mut conn, &mut results)
            .await?;
        Ok(results)
    }

    pub async fn search_shadow_scoped(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        language: Option<&Language>,
        codebase_id: Option<&str>,
        file_role: Option<&FileRole>,
        top_k: usize,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let mut conn = self.connection().await?;
        let mut results = shadow_index_or_empty(
            search::hybrid_search_impl(
                &mut conn,
                index::SHADOW_INDEX_NAME,
                query_embedding,
                query_text,
                language,
                codebase_id,
                file_role,
                top_k,
            )
            .await,
        )?;
        self.hydrate_codebase_metadata(&mut conn, &mut results)
            .await?;
        Ok(results)
    }

    pub async fn read_shadow_file_chunks(
        &self,
        filepath: &str,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let mut conn = self.connection().await?;
        let mut results = shadow_index_or_empty(
            search::read_file_chunks_impl(&mut conn, index::SHADOW_INDEX_NAME, filepath, None)
                .await,
        )?;
        self.hydrate_codebase_metadata(&mut conn, &mut results)
            .await?;
        Ok(results)
    }

    pub async fn read_shadow_file_chunks_scoped(
        &self,
        filepath: &str,
        codebase_id: Option<&str>,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let mut conn = self.connection().await?;
        let mut results = shadow_index_or_empty(
            search::read_file_chunks_impl(
                &mut conn,
                index::SHADOW_INDEX_NAME,
                filepath,
                codebase_id,
            )
            .await,
        )?;
        self.hydrate_codebase_metadata(&mut conn, &mut results)
            .await?;
        Ok(results)
    }

    pub async fn store_shadow_chunks_batch(
        &self,
        chunks: &[Chunk],
        embeddings: &[Vec<f32>],
        ttl_seconds: u64,
    ) -> anyhow::Result<()> {
        self.ensure_shadow_index().await?;
        let mut conn = self.connection().await?;
        commands::store_shadow_chunks_batch_impl(&mut conn, chunks, embeddings, ttl_seconds).await
    }

    pub async fn get_file_mtimes(
        &self,
        codebase_id: Option<&str>,
    ) -> anyhow::Result<std::collections::HashMap<String, String>> {
        let mut conn = self.connection().await?;
        match Self::get_file_mtimes_aggregate(&mut conn, codebase_id).await {
            Ok(mtimes) => Ok(mtimes),
            Err(e) => {
                tracing::debug!("FT.AGGREGATE for mtimes failed, falling back to pipeline: {e}");
                Self::get_file_mtimes_pipeline(&mut conn, codebase_id).await
            }
        }
    }

    async fn get_file_mtimes_aggregate(
        conn: &mut redis::aio::MultiplexedConnection,
        codebase_id: Option<&str>,
    ) -> anyhow::Result<std::collections::HashMap<String, String>> {
        let query = if let Some(id) = codebase_id {
            search::build_codebase_filter(id)
        } else {
            "*".to_string()
        };
        let result: redis::Value = cmd("FT.AGGREGATE")
            .arg(index::INDEX_NAME)
            .arg(query)
            .arg("LOAD")
            .arg(2)
            .arg("@filepath")
            .arg("@mtime")
            .arg("GROUPBY")
            .arg(1)
            .arg("@filepath")
            .arg("REDUCE")
            .arg("FIRST_VALUE")
            .arg(1)
            .arg("@mtime")
            .arg("AS")
            .arg("mtime")
            .arg("DIALECT")
            .arg(2)
            .query_async(conn)
            .await?;

        Self::parse_aggregate_mtimes(result)
    }

    fn parse_aggregate_mtimes(
        value: redis::Value,
    ) -> anyhow::Result<std::collections::HashMap<String, String>> {
        let arr = match value {
            redis::Value::Array(a) => a,
            _ => anyhow::bail!("FT.AGGREGATE expected array response"),
        };

        let mut mtimes = std::collections::HashMap::new();
        if arr.is_empty() {
            return Ok(mtimes);
        }

        for row in arr.iter().skip(1) {
            if let redis::Value::Array(fields) = row {
                let mut filepath = None;
                let mut mtime = None;
                let mut j = 0;
                while j + 1 < fields.len() {
                    if let (redis::Value::BulkString(k), redis::Value::BulkString(v)) =
                        (&fields[j], &fields[j + 1])
                    {
                        match String::from_utf8_lossy(k).as_ref() {
                            "filepath" => filepath = Some(String::from_utf8_lossy(v).into_owned()),
                            "mtime" => mtime = Some(String::from_utf8_lossy(v).into_owned()),
                            _ => {}
                        }
                    }
                    j += 2;
                }
                if let (Some(fp), Some(mt)) = (filepath, mtime) {
                    if !mt.is_empty() {
                        mtimes.entry(fp).or_insert(mt);
                    }
                }
            }
        }

        Ok(mtimes)
    }

    async fn get_file_mtimes_pipeline(
        conn: &mut redis::aio::MultiplexedConnection,
        codebase_id: Option<&str>,
    ) -> anyhow::Result<std::collections::HashMap<String, String>> {
        let mut mtimes = std::collections::HashMap::new();
        let pattern = match codebase_id {
            Some(id) => format!("{}{id}:*", index::KEY_PREFIX),
            None => format!("{}*", index::KEY_PREFIX),
        };
        let mut cursor: u64 = 0;

        loop {
            let (new_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(100)
                .query_async(conn)
                .await?;

            if !keys.is_empty() {
                let mut pipe = redis::pipe();
                for key in &keys {
                    pipe.cmd("HMGET")
                        .arg(key)
                        .arg("codebase_id")
                        .arg("filepath")
                        .arg("mtime");
                }
                let pipe_results: Vec<redis::Value> = pipe.query_async(conn).await?;
                for value in pipe_results {
                    if let redis::Value::Array(parts) = value {
                        if parts.len() >= 3 {
                            let key_codebase_id = match &parts[0] {
                                redis::Value::BulkString(bs) => {
                                    Some(String::from_utf8_lossy(bs).into_owned())
                                }
                                redis::Value::Nil => None,
                                _ => None,
                            };
                            if let Some(expected) = codebase_id {
                                if key_codebase_id.as_deref() != Some(expected) {
                                    continue;
                                }
                            }
                            let filepath = match &parts[1] {
                                redis::Value::BulkString(bs) => {
                                    Some(String::from_utf8_lossy(bs).into_owned())
                                }
                                redis::Value::Nil => None,
                                _ => None,
                            };
                            let mtime = match &parts[2] {
                                redis::Value::BulkString(bs) => {
                                    let s = String::from_utf8_lossy(bs).into_owned();
                                    if s.is_empty() {
                                        None
                                    } else {
                                        Some(s)
                                    }
                                }
                                redis::Value::Nil => None,
                                _ => None,
                            };
                            if let (Some(fp), Some(mt)) = (filepath, mtime) {
                                mtimes.entry(fp).or_insert(mt);
                            }
                        }
                    }
                }
            }

            cursor = new_cursor;
            if cursor == 0 {
                break;
            }
        }

        Ok(mtimes)
    }

    pub async fn register_codebase(
        &self,
        root: &Path,
        alias: Option<&str>,
    ) -> anyhow::Result<Codebase> {
        let mut conn = self.connection().await?;
        let mut codebase = Codebase::from_root(root, alias.map(str::to_string))?;

        if let Some(alias) = alias {
            if alias.trim().is_empty() {
                anyhow::bail!("codebase alias cannot be empty");
            }

            let alias_key = format!("{}{}", index::CODEBASE_ALIAS_PREFIX, alias);
            let reserved: Option<String> = cmd("SET")
                .arg(&alias_key)
                .arg(&codebase.codebase_id)
                .arg("NX")
                .query_async(&mut conn)
                .await?;
            if reserved.is_none() {
                let existing_id: Option<String> =
                    cmd("GET").arg(&alias_key).query_async(&mut conn).await?;
                if existing_id.as_deref() != Some(codebase.codebase_id.as_str()) {
                    anyhow::bail!(
                        "codebase alias '{}' already points to {}, not {}",
                        alias,
                        existing_id.unwrap_or_else(|| "<missing>".to_string()),
                        codebase.codebase_id
                    );
                }
            }
        }

        if let Some(existing) =
            Self::get_codebase_by_id_impl(&mut conn, &codebase.codebase_id).await?
        {
            if codebase.alias.is_none() {
                codebase.alias = existing.alias.clone();
            }
            codebase.last_synced_commit = existing.last_synced_commit;
            codebase.indexed = existing.indexed;

            if existing.alias.as_deref() != codebase.alias.as_deref() {
                if let Some(old_alias) = existing.alias.clone() {
                    cmd("DEL")
                        .arg(format!("{}{}", index::CODEBASE_ALIAS_PREFIX, old_alias))
                        .query_async::<redis::Value>(&mut conn)
                        .await?;
                }
            }
        }

        Self::store_codebase_impl(&mut conn, &codebase).await?;
        if let Some(alias) = &codebase.alias {
            cmd("SET")
                .arg(format!("{}{}", index::CODEBASE_ALIAS_PREFIX, alias))
                .arg(&codebase.codebase_id)
                .query_async::<redis::Value>(&mut conn)
                .await?;
        }

        Ok(codebase)
    }

    pub async fn resolve_codebase(
        &self,
        directory_path: Option<&Path>,
        codebase_alias: Option<&str>,
    ) -> anyhow::Result<Option<Codebase>> {
        let mut conn = self.connection().await?;
        let by_path = match directory_path {
            Some(path) => {
                let codebase = Codebase::from_root(path, None)?;
                Some(
                    Self::get_codebase_by_id_impl(&mut conn, &codebase.codebase_id)
                        .await?
                        .unwrap_or(codebase),
                )
            }
            None => None,
        };

        let by_alias = match codebase_alias {
            Some(alias) => {
                let id: Option<String> = cmd("GET")
                    .arg(format!("{}{}", index::CODEBASE_ALIAS_PREFIX, alias))
                    .query_async(&mut conn)
                    .await?;
                let id = id.ok_or_else(|| anyhow::anyhow!("unknown codebase alias: {alias}"))?;
                Some(
                    Self::get_codebase_by_id_impl(&mut conn, &id)
                        .await?
                        .ok_or_else(|| {
                            anyhow::anyhow!("codebase alias '{alias}' points to missing id {id}")
                        })?,
                )
            }
            None => None,
        };

        match (by_path, by_alias) {
            (Some(path_codebase), Some(alias_codebase)) => {
                if path_codebase.codebase_id != alias_codebase.codebase_id {
                    anyhow::bail!(
                        "directory_path resolves to codebase {}, but alias resolves to {}",
                        path_codebase.codebase_id,
                        alias_codebase.codebase_id
                    );
                }
                Ok(Some(alias_codebase))
            }
            (Some(codebase), None) | (None, Some(codebase)) => Ok(Some(codebase)),
            (None, None) => Ok(None),
        }
    }

    pub async fn list_codebases(&self) -> anyhow::Result<Vec<Codebase>> {
        let mut conn = self.connection().await?;
        let mut cursor = 0u64;
        let mut ids = Vec::new();
        let pattern = format!("{}*", index::CODEBASE_PREFIX);

        loop {
            let (next_cursor, keys): (u64, Vec<String>) = cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(100)
                .query_async(&mut conn)
                .await?;
            ids.extend(
                keys.into_iter()
                    .filter_map(|key| key.strip_prefix(index::CODEBASE_PREFIX).map(str::to_string)),
            );
            cursor = next_cursor;
            if cursor == 0 {
                break;
            }
        }

        let mut codebases = Vec::new();
        for id in ids {
            if let Some(codebase) = Self::get_codebase_by_id_impl(&mut conn, &id).await? {
                codebases.push(codebase);
            }
        }
        codebases.sort_by(|a, b| a.root_path.cmp(&b.root_path));
        Ok(codebases)
    }

    pub async fn get_last_synced_commit(
        &self,
        codebase_id: &str,
    ) -> anyhow::Result<Option<String>> {
        let mut conn = self.connection().await?;
        let result: Option<String> = cmd("HGET")
            .arg(format!("{}{}", index::CODEBASE_PREFIX, codebase_id))
            .arg("last_commit")
            .query_async(&mut conn)
            .await?;

        Ok(result.filter(|value| !value.is_empty()))
    }

    pub async fn mark_codebase_indexed(
        &self,
        codebase_id: &str,
        commit_sha: Option<&str>,
    ) -> anyhow::Result<()> {
        let mut conn = self.connection().await?;
        let key = format!("{}{}", index::CODEBASE_PREFIX, codebase_id);

        let mut command = cmd("HSET");
        command
            .arg(&key)
            .arg("indexed")
            .arg("true")
            .arg("last_commit")
            .arg(commit_sha.unwrap_or(""));
        command.query_async::<redis::Value>(&mut conn).await?;

        Ok(())
    }

    async fn store_codebase_impl(
        conn: &mut redis::aio::MultiplexedConnection,
        codebase: &Codebase,
    ) -> anyhow::Result<()> {
        cmd("HSET")
            .arg(format!(
                "{}{}",
                index::CODEBASE_PREFIX,
                codebase.codebase_id
            ))
            .arg("codebase_id")
            .arg(&codebase.codebase_id)
            .arg("alias")
            .arg(codebase.alias.as_deref().unwrap_or(""))
            .arg("root_path")
            .arg(&codebase.root_path)
            .arg("last_commit")
            .arg(codebase.last_synced_commit.as_deref().unwrap_or(""))
            .arg("indexed")
            .arg(if codebase.indexed { "true" } else { "false" })
            .query_async::<redis::Value>(conn)
            .await?;
        Ok(())
    }

    async fn get_codebase_by_id_impl(
        conn: &mut redis::aio::MultiplexedConnection,
        codebase_id: &str,
    ) -> anyhow::Result<Option<Codebase>> {
        let values: HashMap<String, String> = cmd("HGETALL")
            .arg(format!("{}{}", index::CODEBASE_PREFIX, codebase_id))
            .query_async(conn)
            .await?;

        if values.is_empty() {
            return Ok(None);
        }

        let alias = values
            .get("alias")
            .filter(|alias| !alias.is_empty())
            .cloned();
        let last_synced_commit = values
            .get("last_commit")
            .filter(|commit| !commit.is_empty())
            .cloned();
        let indexed = values.get("indexed").is_some_and(|value| value == "true");

        Ok(Some(Codebase {
            codebase_id: values
                .get("codebase_id")
                .cloned()
                .unwrap_or_else(|| codebase_id.to_string()),
            alias,
            root_path: values.get("root_path").cloned().unwrap_or_default(),
            last_synced_commit,
            indexed,
        }))
    }

    async fn hydrate_codebase_metadata(
        &self,
        conn: &mut redis::aio::MultiplexedConnection,
        results: &mut [SearchResult],
    ) -> anyhow::Result<()> {
        let mut codebases = HashMap::new();
        for result in results.iter() {
            if !result.codebase_id.is_empty() && !codebases.contains_key(&result.codebase_id) {
                let codebase = Self::get_codebase_by_id_impl(conn, &result.codebase_id).await?;
                codebases.insert(result.codebase_id.clone(), codebase);
            }
        }

        for result in results {
            if let Some(Some(codebase)) = codebases.get(&result.codebase_id) {
                result.codebase_alias = codebase.alias.clone();
                result.root_path = codebase.root_path.clone();
            }
        }

        Ok(())
    }
}

fn shadow_index_or_empty(
    result: anyhow::Result<Vec<SearchResult>>,
) -> anyhow::Result<Vec<SearchResult>> {
    match result {
        Ok(results) => Ok(results),
        Err(error) if is_missing_shadow_index(&error) => {
            debug!(
                index = index::SHADOW_INDEX_NAME,
                "Shadow index is not present; treating shadow overlay as empty"
            );
            Ok(Vec::new())
        }
        Err(error) => Err(error),
    }
}

fn is_missing_shadow_index(error: &anyhow::Error) -> bool {
    match error.downcast_ref::<redis::RedisError>() {
        Some(redis_error) => index::is_index_not_found_error(redis_error),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use redis::{ErrorKind, RedisError};

    #[test]
    fn shadow_index_or_empty_returns_empty_when_shadow_index_is_missing() {
        let error: anyhow::Error =
            RedisError::from((ErrorKind::ResponseError, "Unknown Index name")).into();

        let results = shadow_index_or_empty(Err(error)).unwrap();

        assert!(results.is_empty());
    }

    #[test]
    fn shadow_index_or_empty_preserves_non_missing_index_errors() {
        let error: anyhow::Error =
            RedisError::from((ErrorKind::ResponseError, "syntax error")).into();

        let result = shadow_index_or_empty(Err(error));

        assert!(result.is_err());
    }
}
