use crate::{
    embedding::EmbeddingEngine,
    storage::Storage,
    Language, QueryCitation, QueryRequest, QueryResponse, QueryStatus, QueryTraceStep, SearchResult,
};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub steps: Vec<PlanStep>,
    pub max_steps: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlanStep {
    Search {
        query: String,
        language: Option<Language>,
        limit: usize,
    },
    Read {
        filepath: String,
    },
}

pub struct Planner;

impl Planner {
    pub fn plan(&self, request: &QueryRequest) -> Plan {
        let max_steps = request
            .budgets
            .as_ref()
            .and_then(|b| b.max_steps)
            .unwrap_or(5);

        let mut steps = Vec::new();

        steps.push(PlanStep::Search {
            query: request.query.clone(),
            language: request.language,
            limit: 5,
        });

        let query_lower = request.query.to_lowercase();

        // Heuristic: if "auth" is mentioned, expand with general identity terms
        if query_lower.contains("auth") {
            steps.push(PlanStep::Search {
                query: format!(
                    "{} authentication authorization login identity",
                    request.query
                ),
                language: request.language,
                limit: 5,
            });
        }

        // Heuristic: if "storage" or "data" is mentioned
        if query_lower.contains("storage")
            || query_lower.contains("data")
            || query_lower.contains("db")
        {
            steps.push(PlanStep::Search {
                query: format!("{} database persistence cache store", request.query),
                language: request.language,
                limit: 5,
            });
        }

        // Heuristic: if "sync" or "update" is mentioned
        if query_lower.contains("sync") || query_lower.contains("update") {
            steps.push(PlanStep::Search {
                query: format!(
                    "{} synchronization incremental full reconcile",
                    request.query
                ),
                language: request.language,
                limit: 5,
            });
        }

        Plan { steps, max_steps }
    }
}

pub struct AgentExecutor {
    storage: Arc<Storage>,
    engine: Arc<EmbeddingEngine>,
}

impl AgentExecutor {
    pub fn new(storage: Arc<Storage>, engine: Arc<EmbeddingEngine>) -> Self {
        Self { storage, engine }
    }

    pub async fn execute(&self, request: &QueryRequest, plan: Plan) -> QueryResponse {
        let mut trace = Vec::new();
        let mut evidence: Vec<SearchResult> = Vec::new();
        let mut seen_chunks = std::collections::HashSet::new();
        let mut total_tokens = 0usize;
        let mut steps_taken = 0usize;

        let max_tokens = request
            .budgets
            .as_ref()
            .and_then(|b| b.max_tokens)
            .unwrap_or(4000);
        let max_steps = plan.max_steps;

        let codebase_id = match self.resolve_codebase(request).await {
            Ok(id) => id,
            Err(e) => {
                return QueryResponse {
                    status: QueryStatus::Failure,
                    answer: format!("Error resolving codebase: {e}"),
                    evidence: Vec::new(),
                    trace: Vec::new(),
                    warning: None,
                };
            }
        };

        for step in plan.steps {
            if steps_taken >= max_steps {
                debug!("Max steps reached ({})", max_steps);
                break;
            }

            if total_tokens >= max_tokens {
                debug!("Max tokens reached ({})", max_tokens);
                break;
            }

            match step {
                PlanStep::Search {
                    query,
                    language,
                    limit,
                } => {
                    debug!("Executing step: Search for '{}'", query);
                    let query_embedding = match self.engine.embed(&query).await {
                        Ok(emb) => emb,
                        Err(e) => {
                            warn!("Failed to embed query '{}': {}", query, e);
                            continue;
                        }
                    };

                    let results = match self
                        .storage
                        .hybrid_search_scoped(
                            &query_embedding,
                            &query,
                            language.as_ref(),
                            codebase_id.as_deref(),
                            limit,
                        )
                        .await
                    {
                        Ok(r) => r,
                        Err(e) => {
                            warn!("Search failed for '{}': {}", query, e);
                            continue;
                        }
                    };

                    trace.push(QueryTraceStep {
                        name: "search".to_string(),
                        query: Some(query),
                        filepath: None,
                        results: Some(results.len()),
                    });

                    for result in results {
                        if total_tokens >= max_tokens {
                            break;
                        }

                        if !seen_chunks.insert(result.chunk_id.clone()) {
                            continue;
                        }

                        let tokens = self.count_tokens(&result.content);
                        if total_tokens + tokens > max_tokens {
                            continue;
                        }

                        total_tokens += tokens;
                        evidence.push(result);
                    }
                }
                PlanStep::Read { filepath } => {
                    debug!("Executing step: Read '{}'", filepath);
                    let results = match self
                        .storage
                        .read_file_chunks_scoped(&filepath, codebase_id.as_deref())
                        .await
                    {
                        Ok(r) => r,
                        Err(e) => {
                            warn!("Read failed for '{}': {}", filepath, e);
                            continue;
                        }
                    };

                    trace.push(QueryTraceStep {
                        name: "read".to_string(),
                        query: None,
                        filepath: Some(filepath),
                        results: Some(results.len()),
                    });

                    for result in results {
                        if total_tokens >= max_tokens {
                            break;
                        }

                        if !seen_chunks.insert(result.chunk_id.clone()) {
                            continue;
                        }

                        let tokens = self.count_tokens(&result.content);
                        if total_tokens + tokens > max_tokens {
                            continue;
                        }

                        total_tokens += tokens;
                        evidence.push(result);
                    }
                }
            }
            steps_taken += 1;
        }

        let citations: Vec<QueryCitation> = evidence
            .iter()
            .map(|r| QueryCitation {
                filepath: r.filepath.clone(),
                start_line: r.start_line,
                end_line: r.end_line,
                symbol: Some(r.name.clone()),
            })
            .collect();

        let answer = self.synthesize_answer(&request.query, &evidence);

        QueryResponse {
            status: if evidence.is_empty() {
                QueryStatus::Failure
            } else {
                QueryStatus::Success
            },
            answer,
            evidence: citations,
            trace,
            warning: if total_tokens >= max_tokens {
                Some(format!(
                    "Budget exceeded: stopped after {} tokens",
                    total_tokens
                ))
            } else if steps_taken >= max_steps {
                Some(format!(
                    "Budget exceeded: stopped after {} steps",
                    steps_taken
                ))
            } else {
                None
            },
        }
    }

    async fn resolve_codebase(&self, request: &QueryRequest) -> anyhow::Result<Option<String>> {
        if let Some(ref path) = request.directory_path {
            let codebase = self.storage.register_codebase(Path::new(path), None).await?;
            return Ok(Some(codebase.codebase_id));
        }

        if let Some(ref alias) = request.codebase_alias {
            let codebases = self.storage.list_codebases().await?;
            if let Some(codebase) = codebases.iter().find(|c| c.alias.as_deref() == Some(alias)) {
                return Ok(Some(codebase.codebase_id.clone()));
            }
            return Err(anyhow::anyhow!("Codebase alias '{}' not found", alias));
        }

        Ok(None)
    }

    fn count_tokens(&self, text: &str) -> usize {
        self.engine.count_tokens(text)
    }

    fn synthesize_answer(&self, query: &str, evidence: &[SearchResult]) -> String {
        // TODO: Integrate with a generative LLM (local via ort or remote via reqwest) 
        // to provide a natural language answer grounded in the evidence.
        if evidence.is_empty() {
            return format!("I could not find any information relevant to '{}'.", query);
        }

        let mut answer = format!("Based on my research into '{}', I found several relevant components:\n\n", query);

        for (i, result) in evidence.iter().take(5).enumerate() {
            answer.push_str(&format!("{}. **{}** ({}) in `{}`\n", 
                i + 1, 
                result.name, 
                result.node_type, 
                result.filepath
            ));
            if !result.signature.is_empty() {
                answer.push_str(&format!("   `{}`\n", result.signature));
            }
        }

        if evidence.len() > 5 {
            answer.push_str(&format!("\nAnd {} other relevant fragments.", evidence.len() - 5));
        }

        answer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Language;

    #[test]
    fn test_plan_auth_query() {
        let request = QueryRequest {
            query: "how does auth work".to_string(),
            codebase_alias: None,
            directory_path: None,
            language: Some(Language::Rust),
            budgets: None,
            stream: None,
        };
        let planner = Planner;
        let plan = planner.plan(&request);

        assert!(plan.steps.len() >= 2);
        match &plan.steps[1] {
            PlanStep::Search { query, .. } => {
                assert!(query.contains("authentication"));
                assert!(query.contains("how does auth work"));
            }
            _ => panic!("Expected search step"),
        }
    }

    #[test]
    fn test_plan_sync_query() {
        let request = QueryRequest {
            query: "summarize sync".to_string(),
            codebase_alias: None,
            directory_path: None,
            language: None,
            budgets: None,
            stream: None,
        };
        let planner = Planner;
        let plan = planner.plan(&request);

        assert!(plan.steps.len() >= 2);
        match &plan.steps[1] {
            PlanStep::Search { query, .. } => {
                assert!(query.contains("synchronization"));
                assert!(query.contains("summarize sync"));
            }
            _ => panic!("Expected search step"),
        }
    }
}
