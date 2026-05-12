use crate::KtError;
use redis::cmd;
use tracing::{debug, info};

pub(crate) const INDEX_NAME: &str = "idx:kt_codebase";
pub(crate) const KEY_PREFIX: &str = "kt:doc:";
pub(crate) const SHADOW_INDEX_NAME: &str = "idx:kt_shadow";
pub(crate) const SHADOW_KEY_PREFIX: &str = "kt:shadow:";
pub(crate) const SYNC_STATE_PREFIX: &str = "kt:sync_state:";

pub(crate) fn is_index_not_found_error(err: &redis::RedisError) -> bool {
    let msg = err.to_string().to_lowercase();
    msg.contains("unknown index")
        || msg.contains("no such index")
        || (msg.contains("index name") && (msg.contains("unknown") || msg.contains("not found")))
}

pub(super) async fn index_exists(
    conn: &mut redis::aio::MultiplexedConnection,
    index_name: &str,
) -> anyhow::Result<bool> {
    let result: redis::RedisResult<redis::Value> =
        cmd("FT.INFO").arg(index_name).query_async(conn).await;
    match result {
        Ok(_) => Ok(true),
        Err(e) => {
            if is_index_not_found_error(&e) || e.to_string().to_lowercase().contains("not found") {
                Ok(false)
            } else {
                Err(KtError::Redis(e).into())
            }
        }
    }
}

fn build_schema_args(include_mtime: bool) -> Vec<&'static str> {
    let mut args = vec![
        "chunk_id",
        "TAG",
        "filepath",
        "TEXT",
        "language",
        "TAG",
        "node_type",
        "TAG",
        "name",
        "TEXT",
        "signature",
        "TEXT",
        "content",
        "TEXT",
        "start_line",
        "NUMERIC",
        "SORTABLE",
        "end_line",
        "NUMERIC",
        "parent_context",
        "TEXT",
        "embedding",
        "VECTOR",
        "FLAT",
        "6",
        "TYPE",
        "FLOAT32",
        "DIM",
        "384",
        "DISTANCE_METRIC",
        "COSINE",
    ];
    if include_mtime {
        args.extend_from_slice(&["mtime", "TEXT"]);
    }
    args
}

pub(super) async fn create_index(
    conn: &mut redis::aio::MultiplexedConnection,
    index_name: &str,
    key_prefix: &str,
    include_mtime: bool,
) -> anyhow::Result<()> {
    info!("Creating Redis index {index_name}");

    let mut cmd = cmd("FT.CREATE");
    cmd.arg(index_name)
        .arg("ON")
        .arg("HASH")
        .arg("PREFIX")
        .arg(1)
        .arg(key_prefix)
        .arg("SCHEMA");

    for arg in build_schema_args(include_mtime) {
        cmd.arg(arg);
    }

    let result: redis::RedisResult<redis::Value> = cmd.query_async(conn).await;

    match result {
        Ok(_) => {
            info!("Index {index_name} created successfully");
            Ok(())
        }
        Err(e) if e.to_string().to_lowercase().contains("already exists") => {
            debug!("Index {index_name} already exists (race condition)");
            Ok(())
        }
        Err(e) => Err(KtError::Redis(e).into()),
    }
}

pub(super) async fn alter_add_mtime(
    conn: &mut redis::aio::MultiplexedConnection,
) -> anyhow::Result<()> {
    let result: redis::RedisResult<redis::Value> = cmd("FT.ALTER")
        .arg(INDEX_NAME)
        .arg("SCHEMA")
        .arg("ADD")
        .arg("mtime")
        .arg("TEXT")
        .query_async(conn)
        .await;

    match result {
        Ok(_) => {
            debug!("Added mtime field to index {INDEX_NAME}");
            Ok(())
        }
        Err(e)
            if e.to_string().to_lowercase().contains("already exists")
                || e.to_string().to_lowercase().contains("duplicate") =>
        {
            debug!("mtime field already exists in index {INDEX_NAME}");
            Ok(())
        }
        Err(e) => Err(KtError::Redis(e).into()),
    }
}
