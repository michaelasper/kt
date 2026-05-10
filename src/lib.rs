pub mod config;
pub mod discovery;
pub mod embedding;
pub mod error;
pub mod indexing;
pub mod mcp;
pub mod storage;

pub use config::Config;
pub use error::KtError;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Rust,
    Go,
    Java,
}

impl Language {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "rs" => Some(Self::Rust),
            "go" => Some(Self::Go),
            "java" => Some(Self::Java),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Go => "go",
            Self::Java => "java",
        }
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub chunk_id: String,
    pub filepath: String,
    pub language: Language,
    pub node_type: String,
    pub name: String,
    pub signature: String,
    pub content: String,
    pub parent_context: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
}

impl Chunk {
    pub fn generate_id(filepath: &str, name: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(filepath.as_bytes());
        hasher.update(name.as_bytes());
        let result = hasher.finalize();
        hex::encode(result)
    }
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub chunk_id: String,
    pub filepath: String,
    pub language: Language,
    pub node_type: String,
    pub name: String,
    pub signature: String,
    pub content: String,
    pub parent_context: Option<String>,
    pub score: f64,
}
