use thiserror::Error;

#[derive(Debug, Error)]
pub enum KtError {
    #[error("Redis error: {0}")]
    Redis(#[from] redis::RedisError),

    #[error("ONNX runtime error: {0}")]
    Ort(#[from] ort::Error),

    #[error("Tokenizer error: {0}")]
    Tokenizer(#[from] tokenizers::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("HTTP request error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Index not found: {0}")]
    IndexNotFound(String),

    #[error("No chunks found for file: {0}")]
    FileNotFound(String),

    #[error("Embedding model not available: {0}")]
    ModelUnavailable(String),

    #[error("Parse error for {path}: {reason}")]
    ParseFailed { path: String, reason: String },

    #[error("Background task panicked or was cancelled: {0}")]
    Join(#[from] tokio::task::JoinError),
}

pub type Result<T> = std::result::Result<T, KtError>;
