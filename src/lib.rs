#[cfg(feature = "agentic-rag")]
pub mod agent;
pub mod codebase;
pub mod config;
pub mod diagnostics;
pub mod discovery;
pub mod embedding;
pub mod error;
pub mod eval;
pub mod git;
pub mod global_config;
pub mod indexing;
pub mod mcp;
pub mod mcp_setup;
pub mod storage;
pub mod sync;
pub mod sync_ui;
pub mod upgrade;
pub mod util;

pub use codebase::Codebase;
pub use config::Config;
pub use error::KtError;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Rust,
    Go,
    Java,
    Python,
    Swift,
    #[serde(rename = "objective-c")]
    ObjectiveC,
    Markdown,
    Html,
}

impl Language {
    pub const ALL: &'static [Self] = &[
        Self::Rust,
        Self::Go,
        Self::Java,
        Self::Python,
        Self::Swift,
        Self::ObjectiveC,
        Self::Markdown,
        Self::Html,
    ];

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        Self::ALL
            .iter()
            .copied()
            .find(|language| language.matches_name(s))
    }

    pub fn from_extension(ext: &str) -> Option<Self> {
        let ext = ext.trim_start_matches('.').to_ascii_lowercase();
        Self::ALL
            .iter()
            .copied()
            .find(|language| language.extensions().contains(&ext.as_str()))
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Go => "go",
            Self::Java => "java",
            Self::Python => "python",
            Self::Swift => "swift",
            Self::ObjectiveC => "objective-c",
            Self::Markdown => "markdown",
            Self::Html => "html",
        }
    }

    pub fn aliases(&self) -> &'static [&'static str] {
        match self {
            Self::Rust => &["rust", "rs"],
            Self::Go => &["go", "golang"],
            Self::Java => &["java"],
            Self::Python => &["python", "py", "py3"],
            Self::Swift => &["swift"],
            Self::ObjectiveC => &[
                "objective-c",
                "objectivec",
                "objective c",
                "objc",
                "obj-c",
                "obj c",
            ],
            Self::Markdown => &["markdown", "md", "mdx"],
            Self::Html => &["html", "htm", "xhtml"],
        }
    }

    pub fn extensions(&self) -> &'static [&'static str] {
        match self {
            Self::Rust => &["rs"],
            Self::Go => &["go"],
            Self::Java => &["java"],
            Self::Python => &["py", "pyw"],
            Self::Swift => &["swift"],
            Self::ObjectiveC => &["m", "mm", "h"],
            Self::Markdown => &["md", "markdown", "mdx"],
            Self::Html => &["html", "htm", "xhtml"],
        }
    }

    fn matches_name(&self, name: &str) -> bool {
        self.aliases()
            .iter()
            .any(|alias| alias.eq_ignore_ascii_case(name))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseLanguageError {
    input: String,
}

impl ParseLanguageError {
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl std::fmt::Display for ParseLanguageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Unknown language '{}' (expected one of: {})",
            self.input,
            Language::ALL
                .iter()
                .map(Language::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl std::error::Error for ParseLanguageError {}

impl std::str::FromStr for Language {
    type Err = ParseLanguageError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or_else(|| ParseLanguageError {
            input: s.to_string(),
        })
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QueryBudgets {
    pub max_tokens: Option<usize>,
    pub max_steps: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QueryRequest {
    pub query: String,
    pub codebase_alias: Option<String>,
    pub directory_path: Option<String>,
    pub language: Option<Language>,
    pub budgets: Option<QueryBudgets>,
    pub stream: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum QueryStatus {
    Success,
    Partial,
    Failure,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QueryCitation {
    pub filepath: String,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub symbol: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QueryTraceStep {
    pub name: String,
    pub query: Option<String>,
    pub filepath: Option<String>,
    pub results: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QueryResponse {
    pub status: QueryStatus,
    pub answer: String,
    pub evidence: Vec<QueryCitation>,
    pub trace: Vec<QueryTraceStep>,
    pub warning: Option<String>,
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
    use super::{Chunk, Language};

    #[test]
    fn language_parse_accepts_canonical_names_and_aliases() {
        assert_eq!(Language::parse("rust"), Some(Language::Rust));
        assert_eq!(Language::parse("rs"), Some(Language::Rust));
        assert_eq!(Language::parse("GO"), Some(Language::Go));
        assert_eq!(Language::parse("golang"), Some(Language::Go));
        assert_eq!(Language::parse(" java "), Some(Language::Java));
        assert_eq!(Language::parse("python"), Some(Language::Python));
        assert_eq!(Language::parse("py"), Some(Language::Python));
        assert_eq!(Language::parse("swift"), Some(Language::Swift));
        assert_eq!(Language::parse("objective-c"), Some(Language::ObjectiveC));
        assert_eq!(Language::parse("objc"), Some(Language::ObjectiveC));
        assert_eq!(Language::parse("markdown"), Some(Language::Markdown));
        assert_eq!(Language::parse("md"), Some(Language::Markdown));
        assert_eq!(Language::parse("html"), Some(Language::Html));
        assert_eq!(Language::parse("htm"), Some(Language::Html));
    }

    #[test]
    fn language_from_extension_accepts_supported_source_and_document_files() {
        assert_eq!(Language::from_extension(".rs"), Some(Language::Rust));
        assert_eq!(Language::from_extension("MD"), Some(Language::Markdown));
        assert_eq!(Language::from_extension("py"), Some(Language::Python));
        assert_eq!(Language::from_extension("pyw"), Some(Language::Python));
        assert_eq!(Language::from_extension("swift"), Some(Language::Swift));
        assert_eq!(Language::from_extension("m"), Some(Language::ObjectiveC));
        assert_eq!(Language::from_extension("mm"), Some(Language::ObjectiveC));
        assert_eq!(Language::from_extension("h"), Some(Language::ObjectiveC));
        assert_eq!(Language::from_extension("md"), Some(Language::Markdown));
        assert_eq!(
            Language::from_extension("markdown"),
            Some(Language::Markdown)
        );
        assert_eq!(Language::from_extension("mdx"), Some(Language::Markdown));
        assert_eq!(Language::from_extension("html"), Some(Language::Html));
        assert_eq!(Language::from_extension("htm"), Some(Language::Html));
    }

    #[test]
    fn language_parse_rejects_unknown_names() {
        let error = "typescript".parse::<Language>().unwrap_err();

        assert_eq!(error.input(), "typescript");
        assert!(error.to_string().contains("Unknown language"));
    }

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
