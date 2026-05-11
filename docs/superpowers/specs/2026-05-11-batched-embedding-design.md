# Batched Embedding Inference

**Date:** 2026-05-11
**Issue:** https://github.com/michaelasper/kt/issues/5
**Status:** Approved

## Problem

`EmbeddingEngine::embed_batch()` processes texts sequentially through the ONNX model — one inference pass per text. For a file with 20 chunks, this means 20 separate ONNX runs, 20 Mutex lock acquisitions, and 20 tokenization-inference-pool-normalize cycles. The function name implies batching but no actual batching occurs.

## Design

### Approach: True ONNX Batched Inference with Sub-batches

Pad-to-max-in-batch strategy with a sub-batch cap of 32. Split input texts into groups of 32, pad each group to its own max sequence length, run a single ONNX inference per group, then mean-pool and normalize each result.

### Pipeline

```
embed_batch(texts: &[&str])
  ├── Split into sub-batches of size 32
  │   ├── tokenize_and_pad(sub_batch) → (input_ids, attention_mask, token_type_ids, seq_lens)
  │   │     • Tokenize each text via self.tokenizer.encode()
  │   │     • Find max_seq_len in sub-batch
  │   │     • Pad shorter sequences with 0s, extend attention_mask accordingly
  │   │     • Stack into batch tensors [B, max_seq_len]
  │   ├── session.run(inputs) → output tensor [B, max_seq_len, 384]
  │   └── For each sample: mean_pool(seq_len, attention_mask, output) → normalize
  └── Concatenate all sub-batch results
```

Single-item `embed(text)` delegates to `embed_batch(&[text])`.

### Key Constants

- `BATCH_SIZE: usize = 32` — balances memory (~12.5 MB per sub-batch) with throughput
- `EMBEDDING_DIM: usize = 384` — unchanged

### Tokenization + Padding

New private function `tokenize_and_pad` handles the batch preparation:

1. Tokenize each text in the sub-batch using `self.tokenizer.encode(text, true)`
2. Collect per-sample `(input_ids, attention_mask, token_type_ids)` and record `seq_len`
3. Find `max_seq_len = max(all seq_lens)`
4. Pad each sample's arrays to `max_seq_len` using `Vec::resize(max_seq_len, 0)`
5. Flatten into batch tensors with shape `[sub_batch_size, max_seq_len]`

No external padding library — just `Vec::resize`.

### Mutex & Session

- Keep `Mutex<Session>` for thread safety
- Lock acquired once per sub-batch instead of once per text
- 20 chunks: 20 lock acquisitions → 1 (or ceil(20/32) = 1)

### Mean Pooling

The existing `mean_pool` function signature changes to accept pre-sliced data per sample from the batch output tensor. The batch output is `[B, max_seq_len, 384]` — for each sample, slice `[seq_len, 384]` and mean-pool using the original (unpadded) attention mask.

### No Changes to `sync.rs`

The `execute()` function already calls `engine.embed_batch(&texts)` per file. Interface unchanged.

### Error Handling

- Tokenization failure on any text in a sub-batch fails the whole sub-batch
- Matches current behavior where one bad text fails the embed call
- The sync loop handles per-file errors gracefully

## Tests

- **Batched vs single parity:** embed same texts individually and batched, verify cosine similarity > 0.99
- **Sub-batch splitting:** 40 texts with BATCH_SIZE=32 produces 40 results
- **Padding correctness:** verify padded attention mask has 0s in padding positions
- **Existing tests:** `test_normalize` and `test_mean_pool` continue to pass

## Performance Impact

For a file with N chunks:
- Before: N ONNX inference passes, N Mutex acquisitions
- After: ceil(N/32) ONNX inference passes, ceil(N/32) Mutex acquisitions
- Expected speedup: 5-15x for typical files (10-50 chunks)
