# Agentic RAG Query Contract

**Date:** 2026-05-12
**Issue:** https://github.com/michaelasper/kt/issues/40
**Status:** Draft

## Problem

Standard RAG retrieval (`kt_search`) relies on single-hop similarity. While effective for finding specific symbols, it struggles with abstract codebase questions ("how does auth work?", "summarize the sync pipeline") that require multiple steps of searching, reading, and reasoning.

We need a stable public contract for an **Agentic RAG layer** that external agents (Claude, GPT-4, etc.) can use to ask high-level questions and receive citation-backed answers.

## Goals

- Support abstract questions through a high-level query interface.
- Provide explicit control over resource usage via budgets (`token_count`, `max_steps`).
- Return grounded answers citing exact files and symbols.
- Maintain consistency with existing codebase scoping (`codebase_alias`, `directory_path`).
- Enable future streaming support for real-time synthesis.

## Public Contract (MCP Tool: `kt_query`)

### Request Parameters

```typescript
{
  /** The natural language codebase question */
  query: string;

  /** Optional codebase scoping */
  codebase_alias?: string;
  directory_path?: string;

  /** Optional language filter */
  language?: "rust" | "go" | "java";

  /** Resource budgets */
  budgets?: {
    /** Maximum total tokens allowed in the response */
    max_tokens?: number;
    /** Maximum number of retrieval/read iterations */
    max_steps?: number;
  };

  /** If true, the server should attempt to stream the answer (if supported) */
  stream?: boolean;
}
```

### Response Shape (XML-wrapped for MCP)

The response will be wrapped in a `<query_result>` tag containing the synthesized answer and metadata.

```xml
<query_result status="success | partial | failure">
  <answer>
    The synthesized natural language answer goes here.
  </answer>

  <evidence>
    <cite filepath="src/auth.rs" lines="10-45" symbol="authenticate_request" />
    <cite filepath="src/storage/mod.rs" lines="100-120" />
  </evidence>

  <trace>
    <step name="initial_search" query="authentication mechanism" results="12" />
    <step name="file_read" filepath="src/auth.rs" />
    <step name="followup_search" query="UserSession storage" results="3" />
  </trace>
</query_result>
```

## Examples

### 1. Successful Answer

**Query:** "How are file modification times handled during sync?"

```xml
<query_result status="success">
  <answer>
    File modification times (mtimes) are used for partial sync when a git repository is not detected.
    The system reads known mtimes from Redis via `storage.get_file_mtimes`, compares them against
    the local filesystem using `discovery::get_file_mtime`, and only indexes files where the
    mtime has changed.
  </answer>
  <evidence>
    <cite filepath="src/sync.rs" lines="150-165" symbol="plan_with_options" />
    <cite filepath="src/discovery.rs" lines="40-55" symbol="get_file_mtime" />
  </evidence>
  <trace>
    <step name="initial_search" query="file modification times sync" results="5" />
    <step name="file_read" filepath="src/sync.rs" />
  </trace>
</query_result>
```

### 2. Partial Answer (Budget Exceeded)

**Query:** "Explain the entire storage layer architecture."
**Budget:** `max_steps: 2`

```xml
<query_result status="partial" warning="max_steps exceeded (2/2)">
  <answer>
    The storage layer is built around a Redis backend using RediSearch (FT.CREATE).
    It manages two primary indices: the main index (`idx:kt`) and a shadow index
    for ephemeral PR changes. I've identified the core Storage struct, but was
    unable to fully map the command implementations due to step limits.
  </answer>
  <evidence>
    <cite filepath="src/storage/mod.rs" symbol="Storage" />
    <cite filepath="src/storage/index.rs" />
  </evidence>
  <trace>
    <step name="initial_search" query="storage layer architecture" results="10" />
    <step name="file_read" filepath="src/storage/mod.rs" />
  </trace>
</query_result>
```

### 3. No Answer (Failure)

**Query:** "How do I deploy this to Kubernetes?"

```xml
<query_result status="failure">
  <answer>
    I was unable to find any information regarding Kubernetes deployment in the indexed codebase.
  </answer>
  <trace>
    <step name="initial_search" query="kubernetes deploy k8s" results="0" />
  </trace>
</query_result>
```

## Non-Goals for MVP

- **Local LLM Orchestration**: The first version of this contract may be fulfilled by the calling agent (acting as the orchestrator) using multiple `kt_search` calls, or a simple hardcoded "agent" within `kt`.
- **Session State**: This is a stateless query interface. Context from previous questions is not preserved automatically.

## Implementation Plan

1.  **Issue #40**: Land this design doc and define the `QueryRequest` and `QueryResponse` structs in `src/lib.rs`. (Current)
2.  **Issue #44**: Create an evaluation suite with "Golden Answers" for specific abstract questions.
3.  **Issue #41**: Implement the planning logic (splitting query into search terms).
