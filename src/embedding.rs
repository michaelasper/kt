use crate::{Chunk, Config, KtError};
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;
use std::path::Path;
use tokenizers::Tokenizer;
use tracing::{debug, info};

const MODEL_URL: &str =
    "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/onnx/model.onnx";
const TOKENIZER_URL: &str =
    "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json";
const EXPECTED_MODEL_SHA256: &str =
    "6fd5d72fe4589f189f8ebc006442dbb529bb7ce38f8082112682524616046452";
const EXPECTED_TOKENIZER_SHA256: &str =
    "be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037";
const EMBEDDING_DIM: usize = 384;
const BATCH_SIZE: usize = 32;

pub(crate) fn chunk_embedding_text(chunk: &Chunk) -> String {
    let mut text = format!(
        "filepath: {}\nlanguage: {}\nnode_type: {}\nname: {}\nsignature: {}\n",
        chunk.filepath,
        chunk.language.as_str(),
        chunk.node_type,
        chunk.name,
        chunk.signature
    );

    if let Some(parent_context) = chunk
        .parent_context
        .as_deref()
        .map(str::trim)
        .filter(|ctx| !ctx.is_empty())
    {
        text.push_str("parent_context:\n");
        text.push_str(parent_context);
        text.push('\n');
    }

    text.push_str("content:\n");
    text.push_str(&chunk.content);
    text
}

struct BatchInputs {
    input_ids: Vec<i64>,
    attention_mask: Vec<i64>,
    token_type_ids: Vec<i64>,
    seq_lens: Vec<usize>,
    max_seq_len: usize,
    batch_size: usize,
}

fn tokenize_and_pad(tokenizer: &Tokenizer, texts: &[&str]) -> anyhow::Result<BatchInputs> {
    let mut all_input_ids = Vec::new();
    let mut all_attention_mask = Vec::new();
    let mut all_token_type_ids = Vec::new();
    let mut seq_lens = Vec::new();

    let mut max_seq_len = 0usize;
    for text in texts {
        let encoding = tokenizer.encode(*text, true).map_err(KtError::Tokenizer)?;

        let ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        let mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|&m| m as i64)
            .collect();
        let type_ids: Vec<i64> = encoding.get_type_ids().iter().map(|&t| t as i64).collect();

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

struct SessionPool {
    sessions: tokio::sync::Mutex<Vec<ort::session::Session>>,
    semaphore: std::sync::Arc<tokio::sync::Semaphore>,
}

impl SessionPool {
    fn new(sessions: Vec<ort::session::Session>) -> Self {
        let count = sessions.len();
        Self {
            sessions: tokio::sync::Mutex::new(sessions),
            semaphore: std::sync::Arc::new(tokio::sync::Semaphore::new(count)),
        }
    }

    async fn acquire(&self) -> anyhow::Result<(ort::session::Session, tokio::sync::OwnedSemaphorePermit)> {
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| anyhow::anyhow!("Semaphore error: {e}"))?;
        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .pop()
            .ok_or_else(|| anyhow::anyhow!("Session pool exhausted"))?;
        Ok((session, permit))
    }

    async fn release(&self, session: ort::session::Session) {
        let mut sessions = self.sessions.lock().await;
        sessions.push(session);
    }
}

#[derive(Debug, Clone)]
pub struct EmbeddingEngine {
    pool: std::sync::Arc<SessionPool>,
    tokenizer: std::sync::Arc<Tokenizer>,
}

impl std::fmt::Debug for SessionPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionPool")
            .field("available", &self.semaphore.available_permits())
            .finish()
    }
}

impl EmbeddingEngine {
    pub async fn new(config: &Config) -> anyhow::Result<Self> {
        let cache_dir = &config.model_cache_dir;
        std::fs::create_dir_all(cache_dir)?;

        let model_path = config.model_path();
        let tokenizer_path = config.tokenizer_path();

        if !model_path.exists() || !tokenizer_path.exists() {
            info!("Downloading embedding model files...");
            download_file(MODEL_URL, &model_path, EXPECTED_MODEL_SHA256).await?;
            download_file(TOKENIZER_URL, &tokenizer_path, EXPECTED_TOKENIZER_SHA256).await?;
            info!("Model download complete");
        }

        let concurrency = std::thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(4);

        let mut sessions = Vec::with_capacity(concurrency);
        for _ in 0..concurrency {
            let session = ort::session::Session::builder()
                .map_err(|e| anyhow::anyhow!("Session builder error: {e}"))?
                .with_optimization_level(GraphOptimizationLevel::Level1)
                .map_err(|e| anyhow::anyhow!("Optimization level error: {e}"))?
                .with_intra_threads(1)
                .map_err(|e| anyhow::anyhow!("Thread config error: {e}"))?
                .commit_from_file(&model_path)
                .map_err(|e| anyhow::anyhow!("Model load error: {e}"))?;
            sessions.push(session);
        }

        let tokenizer = Tokenizer::from_file(&tokenizer_path).map_err(KtError::Tokenizer)?;

        info!("Embedding engine initialized (dim={EMBEDDING_DIM}, pool_size={concurrency})");
        Ok(Self {
            pool: std::sync::Arc::new(SessionPool::new(sessions)),
            tokenizer: std::sync::Arc::new(tokenizer),
        })
    }

    pub async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let mut result = self.embed_batch(&[text]).await?;
        result
            .pop()
            .ok_or_else(|| anyhow::anyhow!("No embedding produced"))
    }

    pub async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let mut all_embeddings = Vec::with_capacity(texts.len());

        for sub_batch in texts.chunks(BATCH_SIZE) {
            let pool = self.pool.clone();
            let (mut session, permit) = pool.acquire().await?;
            let tokenizer_clone = self.tokenizer.clone();
            let owned_sub_batch: Vec<String> = sub_batch.iter().map(|s| s.to_string()).collect();

            let (batch_embeddings, returned_session) = tokio::task::spawn_blocking(
                move || -> anyhow::Result<(Vec<Vec<f32>>, ort::session::Session)> {
                    let text_refs: Vec<&str> = owned_sub_batch.iter().map(|s| s.as_str()).collect();
                    let inputs = tokenize_and_pad(&tokenizer_clone, &text_refs)?;

                    debug!(
                        "Batched embedding: {} texts, max_seq_len={}",
                        inputs.batch_size, inputs.max_seq_len
                    );

                    let shape = [inputs.batch_size, inputs.max_seq_len];
                    let attention_mask = inputs.attention_mask;
                    let input_ids = inputs.input_ids;
                    let token_type_ids = inputs.token_type_ids;
                    let batch_size = inputs.batch_size;
                    let max_seq_len = inputs.max_seq_len;
                    let seq_lens = inputs.seq_lens;

                    let outputs = session.run(ort::inputs! {
                        "input_ids" => Tensor::from_array((shape, input_ids))?,
                        "attention_mask" => Tensor::from_array((shape, attention_mask.clone()))?,
                        "token_type_ids" => Tensor::from_array((shape, token_type_ids))?,
                    })?;
                    let first_output = outputs
                        .into_iter()
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("No output from model"))?;
                    let output_tensor: Tensor<f32> = first_output
                        .1
                        .downcast()
                        .map_err(|e| anyhow::anyhow!("Output downcast error: {e}"))?;
                    let (_shape, data_view) = output_tensor.extract_tensor();

                    let mut embeddings = Vec::with_capacity(batch_size);
                    for i in 0..batch_size {
                        let seq_len = seq_lens[i];
                        let offset = i * max_seq_len * EMBEDDING_DIM;
                        let sample_data = &data_view[offset..offset + seq_len * EMBEDDING_DIM];
                        let sample_mask =
                            &attention_mask[i * max_seq_len..i * max_seq_len + seq_len];

                        let pooled = mean_pool(sample_data, sample_mask, seq_len, EMBEDDING_DIM);
                        let normalized = normalize(pooled);
                        embeddings.push(normalized);
                    }

                    Ok((embeddings, session))
                },
            )
            .await??;

            pool.release(returned_session).await;
            drop(permit);
            all_embeddings.extend(batch_embeddings);
        }

        Ok(all_embeddings)
    }

    pub fn dim(&self) -> usize {
        EMBEDDING_DIM
    }
}

fn mean_pool(data: &[f32], attention_mask: &[i64], seq_len: usize, hidden_size: usize) -> Vec<f32> {
    let mut pooled = vec![0.0f32; hidden_size];

    for (token_idx, &mask_val) in attention_mask.iter().take(seq_len).enumerate() {
        let mask = mask_val as f32;
        let offset = token_idx * hidden_size;
        for dim in 0..hidden_size {
            pooled[dim] += data[offset + dim] * mask;
        }
    }

    let mask_sum: f32 = attention_mask.iter().map(|&m| m as f32).sum();
    if mask_sum > 0.0 {
        for val in pooled.iter_mut() {
            *val /= mask_sum;
        }
    }

    pooled
}

fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for val in v.iter_mut() {
            *val /= norm;
        }
    }
    v
}

async fn download_file(url: &str, path: &Path, expected_sha256: &str) -> anyhow::Result<()> {
    info!("Downloading {url}");
    let response = reqwest::get(url).await?;

    if !response.status().is_success() {
        return Err(KtError::ModelUnavailable(format!(
            "Failed to download {url}: status {}",
            response.status()
        ))
        .into());
    }

    let bytes = response.bytes().await?;
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, &bytes)?;

    verify_sha256(&tmp_path, expected_sha256)?;

    std::fs::rename(&tmp_path, path)?;

    info!("Downloaded {} ({} bytes)", path.display(), bytes.len());
    Ok(())
}

fn verify_sha256(path: &Path, expected: &str) -> anyhow::Result<()> {
    let contents = std::fs::read(path)?;
    let hex = crate::util::sha256_digest(&contents);

    if hex != expected {
        return Err(anyhow::anyhow!(
            "SHA256 checksum mismatch for {}: expected {}, got {}",
            path.display(),
            expected,
            hex
        ));
    }

    debug!("SHA256 verified for {}: {}", path.display(), hex);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Chunk, Language};

    fn sample_chunk(parent_context: Option<String>) -> Chunk {
        Chunk {
            chunk_id: "chunk-a".to_string(),
            codebase_id: "codebase-a".to_string(),
            filepath: "src/auth.rs".to_string(),
            language: Language::Rust,
            node_type: "function".to_string(),
            name: "verify_token".to_string(),
            signature: "fn verify_token(token: &str) -> bool".to_string(),
            content: "fn verify_token(token: &str) -> bool {\n    !token.is_empty()\n}".to_string(),
            parent_context,
            start_line: 10,
            end_line: 12,
        }
    }

    #[test]
    fn chunk_embedding_text_includes_metadata_parent_context_and_content() {
        let chunk = sample_chunk(Some("impl AuthService {".to_string()));

        let text = chunk_embedding_text(&chunk);

        assert!(text.contains("filepath: src/auth.rs"));
        assert!(text.contains("language: rust"));
        assert!(text.contains("node_type: function"));
        assert!(text.contains("name: verify_token"));
        assert!(text.contains("signature: fn verify_token(token: &str) -> bool"));
        assert!(text.contains("parent_context:\nimpl AuthService {"));
        assert!(text.contains("content:\nfn verify_token(token: &str) -> bool"));

        let metadata_pos = text.find("filepath:").unwrap();
        let content_pos = text.find("content:").unwrap();
        assert!(metadata_pos < content_pos);
    }

    #[test]
    fn chunk_embedding_text_omits_empty_parent_context() {
        let chunk = sample_chunk(Some("   ".to_string()));

        let text = chunk_embedding_text(&chunk);

        assert!(!text.contains("parent_context:"));
        assert!(text.contains("content:\nfn verify_token"));
    }

    #[test]
    fn test_mean_pool() {
        let data = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let mask = vec![1, 1];
        let result = mean_pool(&data, &mask, 2, 3);
        assert_eq!(result.len(), 3);
        assert!((result[0] - 2.5).abs() < 0.001);
        assert!((result[1] - 3.5).abs() < 0.001);
        assert!((result[2] - 4.5).abs() < 0.001);
    }

    #[test]
    fn test_normalize() {
        let v = vec![3.0, 4.0];
        let result = normalize(v);
        let norm: f32 = result.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.001);
    }

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
        assert_eq!(
            no_pad_count, 0,
            "longer sequence should have no extra padding"
        );
    }

    #[test]
    fn test_tokenize_and_pad_handles_multiple_lengths() {
        let tokenizer_path = dirs::home_dir()
            .map(|d| d.join(".cache/kt/models/tokenizer.json"))
            .unwrap();
        if !tokenizer_path.exists() {
            return;
        }
        let tokenizer = Tokenizer::from_file(&tokenizer_path).unwrap();

        let texts: Vec<&str> = (0..40)
            .map(|i| {
                if i < 32 {
                    "short"
                } else {
                    "a slightly longer text for padding"
                }
            })
            .collect();

        let inputs = tokenize_and_pad(&tokenizer, &texts).unwrap();

        assert_eq!(inputs.batch_size, 40);
        assert_eq!(inputs.seq_lens.len(), 40);
        assert_eq!(inputs.input_ids.len(), 40 * inputs.max_seq_len);

        let short_len = inputs.seq_lens[0];
        let long_len = inputs.seq_lens[32];
        assert!(short_len < long_len);

        for i in 0..32 {
            assert_eq!(inputs.seq_lens[i], short_len);
        }
        for i in 32..40 {
            assert_eq!(inputs.seq_lens[i], long_len);
        }
    }
}
