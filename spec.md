---

# 📝 Project Specification: kt (Knowledge Transfer)

### **1. Meta Information**

* **Project Name:** `kt`
* **Description:** A local, privacy-first polyglot codebase RAG via MCP.
* **Target Interface:** Oh My Pi (via standard MCP JSON-RPC protocol).
* **Languages Supported (V1):** Rust, Go, Java.

---

### **2. Executive Summary**

**kt** is a local Retrieval-Augmented Generation (RAG) system designed to act as the ultimate knowledge transfer bridge between your local codebase and the Oh My Pi inference engine. By utilizing **Tree-sitter** for AST-based logical code chunking and **Redis Stack** for high-speed hybrid vector search, `kt` allows your local LLM to instantly query, understand, and reason about Rust, Go, and Java projects. The entire pipeline is exposed as a **Model Context Protocol (MCP) Server**, granting Oh My Pi autonomous tool-calling access to the local codebase.

---

### **3. Architecture & Data Flow**

The system operates entirely on the local machine and is divided into three core layers:

#### **A. The Indexing Engine (Write Path)**

* **File Discovery:** Scans the target workspace for `.rs`, `.go`, and `.java` files.
* **AST Parser (Tree-sitter):** Slices files into semantic boundaries (e.g., functions, structs, impl blocks, classes) and injects parent metadata (like Java class annotations or Rust struct definitions) into the chunk context.
* **Local Embeddings:** Passes the semantic chunks through `all-MiniLM-L6-v2` (via `sentence-transformers`) running on the local CPU to generate dense vectors.
* **Redis Ingestion:** Stores the chunk text, metadata, and vector byte array in Redis Hashes.

#### **B. The Storage Engine (Redis Stack)**

* A single, unified Redis Index (`idx:kt_codebase`) handles all languages.
* **Hybrid Search:** Combines Dense Vector Search (semantic intent, e.g., "how do we hash passwords") with BM25 Keyword Search (exact syntax, e.g., `BcryptHasher`).

#### **C. The Interface Layer (MCP Server)**

* A Python-based server implementing the `@modelcontextprotocol/sdk`.
* Communicates with Oh My Pi via standard input/output (`stdio`).

---

### **4. Redis Schema Design**

All data is stored in Redis Hashes with the prefix `kt:doc:`. The schema for the RediSearch index is as follows:

| Field Name | Type | Description |
| --- | --- | --- |
| `chunk_id` | `TAG` | Unique hash of `filepath + node_name`. |
| `filepath` | `TEXT` | Repository-relative path (e.g., `src/main.rs`). |
| `language` | `TAG` | Language filter (`rust`, `go`, `java`). |
| `node_type` | `TAG` | AST node type (`function`, `class`, `struct`, `impl`). |
| `content` | `TEXT` | The raw source code of the chunk + injected parent context. |
| `embedding` | `VECTOR` | 384-dimensional `FLOAT32` vector (`FLAT` index) using `COSINE` distance. |

---

### **5. Exposed MCP Tools**

To make Oh My Pi autonomous, the `kt` MCP server exposes the following tools:

1. **`kt_search` (Hybrid Knowledge Search)**
* **Inputs:** `query` (string), `language` (optional string), `top_k` (optional int, default 3).
* **Behavior:** Embeds the user query, executes a Redis Hybrid Search, and returns the top matching AST chunks wrapped in XML tags.
* **Agent Prompt Trigger:** *"How does the Go service authenticate with the Java backend?"*


2. **`kt_read_file` (Exact File Lookup)**
* **Inputs:** `filepath` (string).
* **Behavior:** Bypasses vector search. Queries Redis for all chunks matching the exact `filepath` and reconstructs the file.
* **Agent Prompt Trigger:** *"Read the contents of `backend/src/main/java/com/kt/Auth.java`."*


3. **`kt_sync` (Action Tool)**
* **Inputs:** `directory_path` (string).
* **Behavior:** Triggers the Tree-sitter pipeline to parse and embed any modified files in the given directory, updating the Redis index.
* **Agent Prompt Trigger:** *"I just rewrote the parser module, update your index."*



---

### **6. Implementation Milestones**

**Phase 1: Storage & Connectivity**

* Deploy Redis Stack locally via Docker.
* Define the `idx:kt_codebase` schema using the `redis-py` library.
* Verify basic CRUD operations and vector search functionality with dummy data.

**Phase 2: The `kt` Parsing Engine**

* Integrate Python bindings for Tree-sitter (`tree-sitter-rust`, `tree-sitter-go`, `tree-sitter-java`).
* Write extraction logic to pull semantic nodes and format them with their necessary parent context.
* Integrate `sentence-transformers` and pipe the resulting vectors into Redis.

**Phase 3: The `kt` MCP Server**

* Implement the Python MCP SDK.
* Wrap the Redis query and indexing logic into the `kt_search`, `kt_read_file`, and `kt_sync` tools.
* Configure Oh My Pi to launch `kt` as an MCP subprocess.

---

### **7. Known Risks & Mitigations**

* **Risk:** *AST Parsing Failures on Incomplete Code.* If you are mid-edit and syntax is broken, Tree-sitter might fail to generate an AST.
* **Mitigation:** The ingestion script will catch Tree-sitter exceptions and gracefully fall back to a basic text-splitter (chunking by line breaks) just to ensure the code remains searchable.


* **Risk:** *Context Window Saturation.* Over-fetching code chunks will blow out the local LLM's context window.
* **Mitigation:** The `kt_search` tool will enforce a strict `top_k` limit and implement a hard token-truncation check before sending the payload back over the MCP protocol.
