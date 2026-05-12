pub mod codebase;
pub mod config;
pub mod discovery;
pub mod embedding;
pub mod error;
pub mod git;
pub mod global_config;
pub mod indexing;
pub mod mcp;
pub mod mcp_setup;
pub mod storage;
pub mod sync;
pub mod sync_ui;
pub mod upgrade;

pub use codebase::Codebase;
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
    pub codebase_id: String,
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
    pub fn generate_id(codebase_id: &str, filepath: &str, name: &str, start_line: usize) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(codebase_id.as_bytes());
        hasher.update(b"\x00");
        hasher.update(filepath.as_bytes());
        hasher.update(b"\x00");
        hasher.update(name.as_bytes());
        hasher.update(b"\x00");
        hasher.update(start_line.to_be_bytes());
        let result = hasher.finalize();
        hex::encode(result)
    }
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub chunk_id: String,
    pub codebase_id: String,
    pub codebase_alias: Option<String>,
    pub root_path: String,
    pub filepath: String,
    pub language: Language,
    pub node_type: String,
    pub name: String,
    pub signature: String,
    pub content: String,
    pub parent_context: Option<String>,
    pub score: f64,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::Chunk;

    #[test]
    fn generate_id_uniqueness() {
        let id_a = Chunk::generate_id("codebase-a", "src/lib.rs", "new", 10);
        let id_b = Chunk::generate_id("codebase-a", "src/lib.rs", "new", 20);
        assert_ne!(
            id_a, id_b,
            "same filepath+name at different lines must produce different IDs"
        );
    }

    #[test]
    fn generate_id_stability() {
        let id_a = Chunk::generate_id("codebase-a", "src/lib.rs", "new", 10);
        let id_b = Chunk::generate_id("codebase-a", "src/lib.rs", "new", 10);
        assert_eq!(id_a, id_b, "same inputs must produce the same ID");
    }

    #[test]
    fn generate_id_separator_safety() {
        let id_a = Chunk::generate_id("codebase-a", "fo", "obar", 1);
        let id_b = Chunk::generate_id("codebase-a", "foob", "ar", 1);
        assert_ne!(
            id_a, id_b,
            "boundary-crossing field values must produce different IDs"
        );
    }

    #[test]
    fn generate_id_name_line_boundary_safety() {
        let id_a = Chunk::generate_id("codebase-a", "a", "b1", 0);
        let id_b = Chunk::generate_id("codebase-a", "a", "b", 10);
        assert_ne!(
            id_a, id_b,
            "name/start_line boundary must be separator-safe"
        );
    }

    #[test]
    fn generate_id_includes_codebase_id() {
        let id_a = Chunk::generate_id("codebase-a", "src/lib.rs", "new", 10);
        let id_b = Chunk::generate_id("codebase-b", "src/lib.rs", "new", 10);
        assert_ne!(
            id_a, id_b,
            "same filepath+name+line in different codebases must not collide"
        );
    }
}
