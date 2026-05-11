use crate::Chunk;
use redis::cmd;
use tracing::{debug, info};

use super::index::{INDEX_NAME, KEY_PREFIX, SHADOW_KEY_PREFIX};
use super::search::{escape_exact_match, extract_doc_keys};

macro_rules! append_hset_fields {
    ($cmd:expr, $chunk:expr, $embedding_bytes:expr, $mtime:expr) => {{
        $cmd.arg("chunk_id")
            .arg(&$chunk.chunk_id)
            .arg("filepath")
            .arg(&$chunk.filepath)
            .arg("language")
            .arg($chunk.language.as_str())
            .arg("node_type")
            .arg(&$chunk.node_type)
            .arg("name")
            .arg(&$chunk.name)
            .arg("signature")
            .arg(&$chunk.signature)
            .arg("content")
            .arg(&$chunk.content)
            .arg("start_line")
            .arg($chunk.start_line as i64)
            .arg("end_line")
            .arg($chunk.end_line as i64)
            .arg("embedding")
            .arg($embedding_bytes);
        if let Some(mt) = $mtime {
            $cmd.arg("mtime").arg(mt);
        }
        if let Some(ref parent_ctx) = $chunk.parent_context {
            $cmd.arg("parent_context").arg(parent_ctx);
        }
    }};
}

pub(super) async fn store_chunk_impl(
    conn: &mut redis::aio::MultiplexedConnection,
    chunk: &Chunk,
    embedding: &[f32],
    mtime: Option<&str>,
) -> anyhow::Result<()> {
    let key = format!("{KEY_PREFIX}{}", chunk.chunk_id);
    let embedding_bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();

    debug!("Storing chunk {} at key {}", chunk.name, key);
    let mut cmd = cmd("HSET");
    cmd.arg(&key);
    append_hset_fields!(cmd, chunk, &embedding_bytes, mtime);

    cmd.query_async::<redis::Value>(conn).await?;
    Ok(())
}

pub(super) async fn store_chunks_batch_impl(
    conn: &mut redis::aio::MultiplexedConnection,
    chunks: &[Chunk],
    embeddings: &[Vec<f32>],
    mtimes: Option<&[String]>,
) -> anyhow::Result<()> {
    let mut pipe = redis::pipe();

    for (i, (chunk, embedding)) in chunks.iter().zip(embeddings.iter()).enumerate() {
        let key = format!("{KEY_PREFIX}{}", chunk.chunk_id);
        let embedding_bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();

        let pipe_cmd = pipe.cmd("HSET");
        pipe_cmd.arg(&key);
        append_hset_fields!(
            pipe_cmd,
            chunk,
            &embedding_bytes,
            mtimes.and_then(|m| m.get(i))
        );

        pipe_cmd.ignore();
    }

    pipe.query_async::<redis::Value>(conn).await?;
    if mtimes.is_some() {
        info!("Stored {} chunks with mtimes in batch", chunks.len());
    } else {
        info!("Stored {} chunks in batch", chunks.len());
    }
    Ok(())
}

pub(super) async fn remove_file_chunks_impl(
    conn: &mut redis::aio::MultiplexedConnection,
    filepath: &str,
) -> anyhow::Result<usize> {
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
        .query_async(conn)
        .await?;

    let keys_to_delete = extract_doc_keys(&result);
    let removed = keys_to_delete.len();

    if !keys_to_delete.is_empty() {
        let mut pipe = redis::pipe();
        for key in &keys_to_delete {
            pipe.cmd("DEL").arg(key).ignore();
        }
        pipe.query_async::<redis::Value>(conn).await?;
    }

    debug!("Removed {removed} chunks for file {filepath}");
    Ok(removed)
}

pub(super) async fn store_shadow_chunks_batch_impl(
    conn: &mut redis::aio::MultiplexedConnection,
    chunks: &[Chunk],
    embeddings: &[Vec<f32>],
    ttl_seconds: u64,
) -> anyhow::Result<()> {
    let mut pipe = redis::pipe();

    for (chunk, embedding) in chunks.iter().zip(embeddings.iter()) {
        let key = format!("{SHADOW_KEY_PREFIX}{}", chunk.chunk_id);
        let embedding_bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();

        let pipe_cmd = pipe.cmd("HSET");
        pipe_cmd.arg(&key);
        append_hset_fields!(pipe_cmd, chunk, &embedding_bytes, None::<&str>);

        pipe_cmd.ignore();

        pipe.cmd("EXPIRE").arg(&key).arg(ttl_seconds).ignore();
    }

    pipe.query_async::<redis::Value>(conn).await?;
    info!(
        "Stored {} shadow chunks with TTL {}s",
        chunks.len(),
        ttl_seconds
    );
    Ok(())
}
