use crate::{Language, QueryRequest};
use serde::{Deserialize, Serialize};

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

        // Initial strategy:
        // 1. Semantic search for the exact query
        // 2. Extract keywords for a secondary lexical-heavy search (simulated)
        // 3. For the MVP, we'll keep it simple and deterministic.

        steps.push(PlanStep::Search {
            query: request.query.clone(),
            language: request.language,
            limit: 5,
        });

        // Heuristic: if "auth" is mentioned, search for auth symbols
        if request.query.to_lowercase().contains("auth") {
            steps.push(PlanStep::Search {
                query: "authenticate authorize login session token".to_string(),
                language: request.language,
                limit: 5,
            });
        }

        // Heuristic: if "storage" or "redis" is mentioned
        if request.query.to_lowercase().contains("storage")
            || request.query.to_lowercase().contains("redis")
        {
            steps.push(PlanStep::Search {
                query: "Redis Storage FT.CREATE index".to_string(),
                language: request.language,
                limit: 5,
            });
        }

        // Heuristic: if "sync" is mentioned
        if request.query.to_lowercase().contains("sync") {
            steps.push(PlanStep::Search {
                query: "SyncStrategy SyncPlan execute discover_files".to_string(),
                language: request.language,
                limit: 5,
            });
        }

        Plan { steps, max_steps }
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
            PlanStep::Search { query, .. } => assert!(query.contains("authenticate")),
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
            PlanStep::Search { query, .. } => assert!(query.contains("SyncPlan")),
            _ => panic!("Expected search step"),
        }
    }
}
