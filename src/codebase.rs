use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Codebase {
    pub codebase_id: String,
    pub alias: Option<String>,
    pub root_path: String,
    pub last_synced_commit: Option<String>,
    pub indexed: bool,
}

impl Codebase {
    pub fn from_root(root: &Path, alias: Option<String>) -> anyhow::Result<Self> {
        let canonical = root.canonicalize()?;
        let root_path = canonical
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid UTF-8 in directory path"))?
            .to_string();

        Ok(Self {
            codebase_id: Self::generate_id(&root_path),
            alias,
            root_path,
            last_synced_commit: None,
            indexed: false,
        })
    }

    pub fn generate_id(canonical_root_path: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(canonical_root_path.as_bytes());
        hex::encode(hasher.finalize())
    }
}
