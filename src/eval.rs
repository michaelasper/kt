use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalFixture {
    pub name: String,
    pub files: HashMap<String, String>,
    pub queries: Vec<EvalQuery>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalQuery {
    pub query: String,
    pub expected_evidence: Vec<ExpectedEvidence>,
    pub negative_evidence: Vec<ExpectedEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExpectedEvidence {
    pub filepath: String,
    pub symbol: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvalResult {
    pub fixture_name: String,
    pub query: String,
    pub status: EvalStatus,
    pub recall_at_10: f64,
    pub found_evidence: Vec<String>,
    pub missing_evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EvalStatus {
    Pass,
    Fail,
}

pub fn create_auth_fixture() -> EvalFixture {
    let mut files = HashMap::new();
    files.insert(
        "src/auth.rs".to_string(),
        r#"
pub struct AuthService {
    token_secret: String,
}

impl AuthService {
    pub fn authenticate_request(&self, authorization_header: &str) -> bool {
        let token = authorization_header.trim_start_matches("Bearer ");
        self.verify_session_token(token)
    }

    fn verify_session_token(&self, token: &str) -> bool {
        !token.is_empty() && token.contains(&self.token_secret)
    }
}
"#
        .to_string(),
    );
    files.insert(
        "src/session.rs".to_string(),
        r#"
pub struct SessionManager;
impl SessionManager {
    pub fn create_session(&self, user_id: &str) -> String {
        format!("session_{}", user_id)
    }
}
"#
        .to_string(),
    );
    files.insert(
        "src/billing.rs".to_string(),
        r#"
pub fn calculate_invoice_total(line_items: &[u64]) -> u64 {
    line_items.iter().sum()
}
"#
        .to_string(),
    );

    EvalFixture {
        name: "auth_flow".to_string(),
        files,
        queries: vec![EvalQuery {
            query: "how does request authentication work?".to_string(),
            expected_evidence: vec![
                ExpectedEvidence {
                    filepath: "src/auth.rs".to_string(),
                    symbol: Some("authenticate_request".to_string()),
                },
                ExpectedEvidence {
                    filepath: "src/auth.rs".to_string(),
                    symbol: Some("verify_session_token".to_string()),
                },
            ],
            negative_evidence: vec![ExpectedEvidence {
                filepath: "src/billing.rs".to_string(),
                symbol: None,
            }],
        }],
    }
}

pub fn create_storage_fixture() -> EvalFixture {
    let mut files = HashMap::new();
    files.insert(
        "src/storage/mod.rs".to_string(),
        r#"
pub struct Storage {
    client: redis::Client,
}

impl Storage {
    pub async fn get_chunk(&self, chunk_id: &str) -> anyhow::Result<String> {
        self.client.get_connection()?.get(chunk_id).map_err(|e| e.into())
    }
}
"#
        .to_string(),
    );
    files.insert(
        "src/storage/search.rs".to_string(),
        r#"
pub async fn search(query: &str) -> Vec<String> {
    vec!["result".to_string()]
}
"#
        .to_string(),
    );

    EvalFixture {
        name: "storage_layer".to_string(),
        files,
        queries: vec![EvalQuery {
            query: "how are chunks retrieved from storage?".to_string(),
            expected_evidence: vec![ExpectedEvidence {
                filepath: "src/storage/mod.rs".to_string(),
                symbol: Some("get_chunk".to_string()),
            }],
            negative_evidence: vec![],
        }],
    }
}

pub fn create_sync_fixture() -> EvalFixture {
    let mut files = HashMap::new();
    files.insert(
        "src/sync.rs".to_string(),
        r#"
pub async fn execute_sync() {
    let files = discover_files();
    for file in files {
        index_file(file);
    }
}

fn discover_files() -> Vec<String> { vec![] }
fn index_file(_f: String) {}
"#
        .to_string(),
    );

    EvalFixture {
        name: "sync_pipeline".to_string(),
        files,
        queries: vec![EvalQuery {
            query: "summarize the sync process".to_string(),
            expected_evidence: vec![
                ExpectedEvidence {
                    filepath: "src/sync.rs".to_string(),
                    symbol: Some("execute_sync".to_string()),
                },
                ExpectedEvidence {
                    filepath: "src/sync.rs".to_string(),
                    symbol: Some("discover_files".to_string()),
                },
            ],
            negative_evidence: vec![],
        }],
    }
}
