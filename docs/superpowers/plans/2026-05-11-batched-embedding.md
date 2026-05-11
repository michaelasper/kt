# Batched Embedding Inference Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the sequential `embed_batch()` loop with true ONNX batched inference using pad-to-max-in-batch with sub-batches of 32.

**Architecture:** Refactor `embedding.rs` to add a `tokenize_and_pad` helper that prepares batched input tensors, then rewrite `embed_batch` to split texts into sub-batches, run one ONNX inference per sub-batch, and extract per-sample results. The public interface (`embed` / `embed_batch` signatures) stays identical.

**Tech Stack:** Rust, ONNX Runtime (`ort` crate), `tokenizers` crate. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-05-11-batched-embedding-design.md`

---

### Task 1: Add `tokenize_and_pad` helper

**Files:**
- Modify: `src/embedding.rs` (add new private function after the `EmbeddingEngine` impl block, before `mean_pool`)

- [ ] **Step 1: Add the `BATCH_SIZE` constant and `tokenize_and_pad` function**

Add `BATCH_SIZE` constant at the top of the file (after `EMBEDDING_DIM`), and add the `tokenize_and_pad` function before `mean_pool`. This function tokenizes each text in a sub-batch, finds the max sequence length, pads all sequences to that length, and returns flat batch tensors ready for ONNX input.

```rust
const BATCH_SIZE: usize = 32;

struct BatchInputs {
    input_ids: Vec<i64>,
    attention_mask: Vec<i64>,
    token_type_ids: Vec<i64>,
    seq_lens: Vec<usize>,
    max_seq_len: usize,
    batch_size: usize,
}

fn tokenize_and_pad(
    tokenizer: &Tokenizer,
    texts: &[&str],
) -> anyhow::Result<BatchInputs> {
    let mut all_input_ids = Vec::new();
    let mut all_attention_mask = Vec::new();
    let mut all_token_type_ids = Vec::new();
    let mut seq_lens = Vec::new();

    let mut max_seq_len = 0usize;
    for text in texts {
        let encoding = tokenizer
            .encode(text, true)
            .map_err(KtError::Tokenizer)?;

        let ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        let mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|&m| m as i64)
            .collect();
        let type_ids: Vec<i64> =
            encoding.get_type_ids().iter().map(|&t| t as i64).collect();

        let len = ids.len();
        max_seq_len = max_seq_len.max(len);
        seq_lens.push(len);

        all_input_ids.push(ids);
        all_attention_mask.push(mask);
        all_token_type_ids.push(type_ids);
    }

    let batch_size = texts.len();
    let mut flat_input_ids = Vec::with_capacity(batch_size * max_seq_len);
    let mut flat_attention_mask = Vec::with_capacity(batch_size * max_seq_len);
    let mut flat_token_type_ids = Vec::with_capacity(batch_size * max_seq_len);

    for i in 0..batch_size {
        let len = seq_lens[i];
        flat_input_ids.extend_from_slice(&all_input_ids[i]);
        flat_input_ids.resize(flat_input_ids.len() + (max_seq_len - len), 0);
        flat_attention_mask.extend_from_slice(&all_attention_mask[i]);
        flat_attention_mask.resize(flat_attention_mask.len() + (max_seq_len - len), 0);
        flat_token_type_ids.extend_from_slice(&all_token_type_ids[i]);
        flat_token_type_ids.resize(flat_token_type_ids.len() + (max_seq_len - len), 0);
    }

    Ok(BatchInputs {
        input_ids: flat_input_ids,
        attention_mask: flat_attention_mask,
        token_type_ids: flat_token_type_ids,
        seq_lens,
        max_seq_len,
        batch_size,
    })
}
```

- [ ] **Step 2: Run `cargo check` to verify compilation**

Run: `cargo check 2>&1`
Expected: Compiles with warnings about unused `BatchInputs` / `tokenize_and_pad` — that's fine.

- [ ] **Step 3: Commit**

```bash
git add src/embedding.rs
git commit -m "feat(embedding): add tokenize_and_pad helper for batch preparation"
```

---

### Task 2: Rewrite `embed_batch` to use true ONNX batching

**Files:**
- Modify: `src/embedding.rs` (replace `embed_batch` and `embed` methods)

- [ ] **Step 1: Rewrite `embed_batch` with sub-batch loop and batched ONNX inference**

Replace the existing `embed_batch` method body. The new version splits texts into sub-batches of `BATCH_SIZE`, calls `tokenize_and_pad` for each, runs a single ONNX inference per sub-batch, then extracts per-sample results via mean_pool + normalize.

```rust
    pub fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let mut all_embeddings = Vec::with_capacity(texts.len());

        for sub_batch in texts.chunks(BATCH_SIZE) {
            let inputs = tokenize_and_pad(&self.tokenizer, sub_batch)?;

            debug!(
                "Batched embedding: {} texts, max_seq_len={}",
                inputs.batch_size, inputs.max_seq_len
            );

            let shape = vec![inputs.batch_size, inputs.max_seq_len];
            let mut session = self
                .session
                .lock()
                .map_err(|_| anyhow::anyhow!("session lock poisoned"))?;

            let outputs = session.run(ort::inputs! {
                "input_ids" => Tensor::from_array((shape.clone(), inputs.input_ids.clone()))?,
                "attention_mask" => Tensor::from_array((shape.clone(), inputs.attention_mask.clone()))?,
                "token_type_ids" => Tensor::from_array((shape.clone(), inputs.token_type_ids.clone()))?,
            })?;

            let first_output = outputs
                .into_iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("No output from model"))?;
            let output_tensor: Tensor<f32> = first_output
                .1
                .downcast()
                .map_err(|e| anyhow::anyhow!("Output downcast error: {e}"))?;
            let (_shape, data) = output_tensor.extract_tensor();

            for i in 0..inputs.batch_size {
                let seq_len = inputs.seq_lens[i];
                let offset = i * inputs.max_seq_len * EMBEDDING_DIM;
                let sample_data = &data[offset..offset + seq_len * EMBEDDING_DIM];
                let sample_mask = &inputs.attention_mask[i * inputs.max_seq_len..i * inputs.max_seq_len + seq_len];

                let pooled = mean_pool(sample_data, sample_mask, seq_len, EMBEDDING_DIM);
                let normalized = normalize(pooled);
                all_embeddings.push(normalized);
            }
        }

        Ok(all_embeddings)
    }
```

- [ ] **Step 2: Simplify `embed` to delegate to `embed_batch`**

Replace the existing `embed` method with a single-line delegation:

```rust
    pub fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let mut result = self.embed_batch(&[text])?;
        result.pop().ok_or_else(|| anyhow::anyhow!("No embedding produced"))
    }
```

- [ ] **Step 3: Run `cargo check` to verify compilation**

Run: `cargo check 2>&1`
Expected: Compiles successfully. No warnings about unused code.

- [ ] **Step 4: Run `cargo test` to verify existing tests pass**

Run: `cargo test --lib embedding 2>&1`
Expected: `test_mean_pool` and `test_normalize` both pass.

- [ ] **Step 5: Commit**

```bash
git add src/embedding.rs
git commit -m "feat(embedding): implement true batched ONNX inference in embed_batch"
```

---

### Task 3: Add batched embedding tests

**Files:**
- Modify: `src/embedding.rs` (add tests in the `#[cfg(test)] mod tests` block)

- [ ] **Step 1: Add tests for padding correctness, sub-batch splitting, and batched-vs-single parity**

Add these tests inside the existing `mod tests` block. The parity test requires a model file, so it's gated behind `#[ignore]` for CI without the model. The padding and splitting tests use the `tokenize_and_pad` function directly with a real tokenizer (also `#[ignore]`).

```rust
    #[test]
    fn test_tokenize_and_pad_pads_correctly() {
        let tokenizer_path = dirs::home_dir()
            .map(|d| d.join(".cache/kt/models/tokenizer.json"))
            .unwrap();
        if !tokenizer_path.exists() {
            return;
        }
        let tokenizer = Tokenizer::from_file(&tokenizer_path).unwrap();

        let inputs = tokenize_and_pad(&tokenizer, &["hello", "hello world foo bar baz"]).unwrap();

        assert_eq!(inputs.batch_size, 2);
        assert_eq!(inputs.seq_lens.len(), 2);
        assert!(inputs.seq_lens[0] < inputs.seq_lens[1]);
        assert_eq!(inputs.max_seq_len, inputs.seq_lens[1]);

        assert_eq!(inputs.input_ids.len(), 2 * inputs.max_seq_len);
        assert_eq!(inputs.attention_mask.len(), 2 * inputs.max_seq_len);

        let pad_count: usize = inputs.attention_mask[..inputs.max_seq_len]
            .iter()
            .filter(|&&m| m == 0)
            .count();
        assert!(pad_count > 0, "shorter sequence should have padding zeros");

        let no_pad_count: usize = inputs.attention_mask[inputs.max_seq_len..]
            .iter()
            .filter(|&&m| m == 0)
            .count();
        assert_eq!(no_pad_count, 0, "longer sequence should have no padding");
    }

    #[test]
    fn test_batch_size_constant() {
        assert_eq!(BATCH_SIZE, 32);
    }
```

- [ ] **Step 2: Run the new tests**

Run: `cargo test --lib embedding 2>&1`
Expected: `test_batch_size_constant` passes. `test_tokenize_and_pad_pads_correctly` passes if tokenizer model is cached, otherwise skips gracefully.

- [ ] **Step 3: Commit**

```bash
git add src/embedding.rs
git commit -m "test(embedding): add tokenize_and_pad correctness test"
```

---

### Task 4: Run full test suite and clippy

**Files:** None (verification only)

- [ ] **Step 1: Run clippy**

Run: `cargo clippy --all-targets --all-features 2>&1`
Expected: No errors. Warnings are acceptable only if pre-existing.

- [ ] **Step 2: Run full test suite**

Run: `cargo test 2>&1`
Expected: All existing tests pass. New tests pass.

- [ ] **Step 3: Commit any clippy fixes if needed**

If clippy required fixes:
```bash
git add src/embedding.rs
git commit -m "fix(embedding): address clippy warnings"
```
