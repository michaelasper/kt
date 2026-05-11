use crate::{Config, KtError};
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;
use std::path::Path;
use tokenizers::Tokenizer;
use tracing::{debug, info};

const MODEL_URL: &str =
    "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/onnx/model.onnx";
const TOKENIZER_URL: &str =
    "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json";
const EMBEDDING_DIM: usize = 384;
const BATCH_SIZE: usize = 32;

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

#[derive(Debug)]
pub struct EmbeddingEngine {
    session: std::sync::Mutex<ort::session::Session>,
    tokenizer: Tokenizer,
}

impl EmbeddingEngine {
    pub async fn new(config: &Config) -> anyhow::Result<Self> {
        let cache_dir = &config.model_cache_dir;
        std::fs::create_dir_all(cache_dir)?;

        let model_path = config.model_path();
        let tokenizer_path = config.tokenizer_path();

        if !model_path.exists() || !tokenizer_path.exists() {
            info!("Downloading embedding model files...");
            download_file(MODEL_URL, &model_path).await?;
            download_file(TOKENIZER_URL, &tokenizer_path).await?;
            info!("Model download complete");
        }

        let session = ort::session::Session::builder()
            .map_err(|e| anyhow::anyhow!("Session builder error: {e}"))?
            .with_optimization_level(GraphOptimizationLevel::Level1)
            .map_err(|e| anyhow::anyhow!("Optimization level error: {e}"))?
            .with_intra_threads(1)
            .map_err(|e| anyhow::anyhow!("Thread config error: {e}"))?
            .commit_from_file(&model_path)
            .map_err(|e| anyhow::anyhow!("Model load error: {e}"))?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path).map_err(KtError::Tokenizer)?;

        info!("Embedding engine initialized (dim={EMBEDDING_DIM})");
        Ok(Self {
            session: std::sync::Mutex::new(session),
            tokenizer,
        })
    }

    pub fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let mut result = self.embed_batch(&[text])?;
        result
            .pop()
            .ok_or_else(|| anyhow::anyhow!("No embedding produced"))
    }

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

            let shape = [inputs.batch_size, inputs.max_seq_len];
            let attention_mask = inputs.attention_mask.clone();
            let mut session = self
                .session
                .lock()
                .map_err(|_| anyhow::anyhow!("session lock poisoned"))?;

            let outputs = session.run(ort::inputs! {
                "input_ids" => Tensor::from_array((shape, inputs.input_ids))?,
                "attention_mask" => Tensor::from_array((shape, inputs.attention_mask))?,
                "token_type_ids" => Tensor::from_array((shape, inputs.token_type_ids))?,
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
                let sample_mask =
                    &attention_mask[i * inputs.max_seq_len..i * inputs.max_seq_len + seq_len];

                let pooled = mean_pool(sample_data, sample_mask, seq_len, EMBEDDING_DIM);
                let normalized = normalize(pooled);
                all_embeddings.push(normalized);
            }
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

async fn download_file(url: &str, path: &Path) -> anyhow::Result<()> {
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
    std::fs::rename(&tmp_path, path)?;

    info!("Downloaded {} ({} bytes)", path.display(), bytes.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
