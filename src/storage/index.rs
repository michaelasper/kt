use crate::KtError;
use redis::cmd;
use tracing::{debug, info};

pub(crate) const INDEX_NAME: &str = "idx:kt_codebase";
pub(crate) const KEY_PREFIX: &str = "kt:doc:";
pub(crate) const SHADOW_INDEX_NAME: &str = "idx:kt_shadow";
pub(crate) const SHADOW_KEY_PREFIX: &str = "kt:shadow:";
pub(crate) const SYNC_STATE_PREFIX: &str = "kt:sync_state:";
pub(crate) const CODEBASE_PREFIX: &str = "kt:codebase:";
pub(crate) const CODEBASE_ALIAS_PREFIX: &str = "kt:codebase_alias:";
pub(crate) const SCHEMA_VERSION_KEY: &str = "kt:schema_version";
pub(crate) const SCHEMA_MIGRATION_LOCK_KEY: &str = "kt:schema_migration_lock";
pub(crate) const SCHEMA_VERSION: &str = "4";

pub(crate) fn is_index_not_found_error(err: &redis::RedisError) -> bool {
    let msg = err.to_string().to_lowercase();
    msg.contains("unknown index")
        || msg.contains("no such index")
        || msg.contains("no: such index")
        || msg.contains("index not found")
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
        "codebase_id",
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
        "file_role",
        "TAG",
        "calls",
        "TEXT",
        "parent_context",
        "TEXT",
        "embedding",
        "VECTOR",
        "HNSW",
        "10",
        "TYPE",
        "FLOAT32",
        "DIM",
        "384",
        "DISTANCE_METRIC",
        "COSINE",
        "M",
        "16",
        "EF_CONSTRUCTION",
        "200",
    ];
    if include_mtime {
        args.extend_from_slice(&["mtime", "TEXT"]);
    }
    args
}

pub(super) async fn ensure_latest_schema(
    conn: &mut redis::aio::MultiplexedConnection,
) -> anyhow::Result<()> {
    let version: Option<String> = cmd("GET").arg(SCHEMA_VERSION_KEY).query_async(conn).await?;
    if version.as_deref() == Some(SCHEMA_VERSION) {
        return Ok(());
    }

    let lock: Option<String> = cmd("SET")
        .arg(SCHEMA_MIGRATION_LOCK_KEY)
        .arg("1")
        .arg("NX")
        .arg("EX")
        .arg(60)
        .query_async(conn)
        .await?;

    if lock.is_some() {
        migrate_to_latest_schema(conn).await?;
        cmd("SET")
            .arg(SCHEMA_VERSION_KEY)
            .arg(SCHEMA_VERSION)
            .query_async::<redis::Value>(conn)
            .await?;
        cmd("DEL")
            .arg(SCHEMA_MIGRATION_LOCK_KEY)
            .query_async::<redis::Value>(conn)
            .await?;
        return Ok(());
    }

    for _ in 0..200 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let version: Option<String> = cmd("GET").arg(SCHEMA_VERSION_KEY).query_async(conn).await?;
        if version.as_deref() == Some(SCHEMA_VERSION) {
            return Ok(());
        }
    }

    anyhow::bail!("Timed out waiting for kt Redis schema migration");
}

async fn migrate_to_latest_schema(
    conn: &mut redis::aio::MultiplexedConnection,
) -> anyhow::Result<()> {
    info!("Migrating Redis schema to v{SCHEMA_VERSION}; existing kt index data will be removed");
    drop_index_with_data(conn, INDEX_NAME).await?;
    drop_index_with_data(conn, SHADOW_INDEX_NAME).await?;

    for pattern in [
        format!("{KEY_PREFIX}*"),
        format!("{SHADOW_KEY_PREFIX}*"),
        format!("{SYNC_STATE_PREFIX}*"),
        format!("{CODEBASE_PREFIX}*"),
        format!("{CODEBASE_ALIAS_PREFIX}*"),
    ] {
        delete_keys_by_pattern(conn, &pattern).await?;
    }

    Ok(())
}

async fn drop_index_with_data(
    conn: &mut redis::aio::MultiplexedConnection,
    index_name: &str,
) -> anyhow::Result<()> {
    let result: redis::RedisResult<redis::Value> = cmd("FT.DROPINDEX")
        .arg(index_name)
        .arg("DD")
        .query_async(conn)
        .await;

    match result {
        Ok(_) => Ok(()),
        Err(e)
            if is_index_not_found_error(&e)
                || e.to_string().to_lowercase().contains("not found") =>
        {
            Ok(())
        }
        Err(e) => Err(KtError::Redis(e).into()),
    }
}

async fn delete_keys_by_pattern(
    conn: &mut redis::aio::MultiplexedConnection,
    pattern: &str,
) -> anyhow::Result<()> {
    let mut cursor = 0u64;
    let mut keys = Vec::new();

    loop {
        let (next_cursor, batch): (u64, Vec<String>) = cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg(pattern)
            .arg("COUNT")
            .arg(1000)
            .query_async(conn)
            .await?;
        keys.extend(batch);
        cursor = next_cursor;
        if cursor == 0 {
            break;
        }
    }

    if !keys.is_empty() {
        let mut pipe = redis::pipe();
        for key in keys {
            pipe.cmd("DEL").arg(key).ignore();
        }
        pipe.query_async::<redis::Value>(conn).await?;
    }

    Ok(())
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
