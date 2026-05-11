mod commands;
mod index;
mod search;

use crate::{Chunk, Config, Language, SearchResult};
use redis::cmd;
use tracing::info;

pub use search::parse_search_results;

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
        top_k: usize,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let mut conn = self.connection().await?;
        search::hybrid_search_impl(
            &mut conn,
            index::INDEX_NAME,
            query_embedding,
            query_text,
            language,
            top_k,
        )
        .await
    }

    pub async fn read_file_chunks(&self, filepath: &str) -> anyhow::Result<Vec<SearchResult>> {
        let mut conn = self.connection().await?;
        search::read_file_chunks_impl(&mut conn, index::INDEX_NAME, filepath).await
    }

    pub async fn lookup_chunks_by_name(
        &self,
        names: &[String],
    ) -> anyhow::Result<Vec<SearchResult>> {
        let mut conn = self.connection().await?;
        search::lookup_chunks_by_name_impl(&mut conn, index::INDEX_NAME, names).await
    }

    pub async fn remove_file_chunks(&self, filepath: &str) -> anyhow::Result<usize> {
        let mut conn = self.connection().await?;
        commands::remove_file_chunks_impl(&mut conn, filepath).await
    }

    pub async fn search_shadow(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        language: Option<&Language>,
        top_k: usize,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let mut conn = self.connection().await?;
        search::hybrid_search_impl(
            &mut conn,
            index::SHADOW_INDEX_NAME,
            query_embedding,
            query_text,
            language,
            top_k,
        )
        .await
    }

    pub async fn read_shadow_file_chunks(
        &self,
        filepath: &str,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let mut conn = self.connection().await?;
        search::read_file_chunks_impl(&mut conn, index::SHADOW_INDEX_NAME, filepath).await
    }

    pub async fn store_shadow_chunks_batch(
        &self,
        chunks: &[Chunk],
        embeddings: &[Vec<f32>],
        ttl_seconds: u64,
    ) -> anyhow::Result<()> {
        let mut conn = self.connection().await?;
        commands::store_shadow_chunks_batch_impl(&mut conn, chunks, embeddings, ttl_seconds).await
    }

    pub async fn get_file_mtimes(
        &self,
    ) -> anyhow::Result<std::collections::HashMap<String, String>> {
        let mut conn = self.connection().await?;
        match Self::get_file_mtimes_aggregate(&mut conn).await {
            Ok(mtimes) => Ok(mtimes),
            Err(e) => {
                tracing::debug!("FT.AGGREGATE for mtimes failed, falling back to pipeline: {e}");
                Self::get_file_mtimes_pipeline(&mut conn).await
            }
        }
    }

    async fn get_file_mtimes_aggregate(
        conn: &mut redis::aio::MultiplexedConnection,
    ) -> anyhow::Result<std::collections::HashMap<String, String>> {
        let result: redis::Value = cmd("FT.AGGREGATE")
            .arg(index::INDEX_NAME)
            .arg("*")
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
    ) -> anyhow::Result<std::collections::HashMap<String, String>> {
        let mut mtimes = std::collections::HashMap::new();
        let pattern = format!("{}*", index::KEY_PREFIX);
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
                    pipe.cmd("HMGET").arg(key).arg("filepath").arg("mtime");
                }
                let pipe_results: Vec<redis::Value> = pipe.query_async(conn).await?;
                for value in pipe_results {
                    if let redis::Value::Array(parts) = value {
                        if parts.len() >= 2 {
                            let filepath = match &parts[0] {
                                redis::Value::BulkString(bs) => {
                                    Some(String::from_utf8_lossy(bs).into_owned())
                                }
                                redis::Value::Nil => None,
                                _ => None,
                            };
                            let mtime = match &parts[1] {
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

    pub async fn get_last_synced_commit(&self, directory: &str) -> anyhow::Result<Option<String>> {
        let mut conn = self.connection().await?;
        let key = format!("{}{}", index::SYNC_STATE_PREFIX, directory);

        let result: Option<String> = redis::cmd("HGET")
            .arg(&key)
            .arg("last_commit")
            .query_async(&mut conn)
            .await?;

        Ok(result)
    }

    pub async fn set_last_synced_commit(
        &self,
        directory: &str,
        commit_sha: &str,
    ) -> anyhow::Result<()> {
        let mut conn = self.connection().await?;
        let key = format!("{}{}", index::SYNC_STATE_PREFIX, directory);

        redis::cmd("HSET")
            .arg(&key)
            .arg("last_commit")
            .arg(commit_sha)
            .query_async::<redis::Value>(&mut conn)
            .await?;

        Ok(())
    }

    pub async fn clear_sync_state(&self, directory: &str) -> anyhow::Result<()> {
        let mut conn = self.connection().await?;
        let key = format!("{}{}", index::SYNC_STATE_PREFIX, directory);

        redis::cmd("DEL")
            .arg(&key)
            .query_async::<redis::Value>(&mut conn)
            .await?;

        Ok(())
    }
}
