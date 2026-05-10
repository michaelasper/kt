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
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(KtError::Tokenizer)?;

        let input_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        let attention_mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|&m| m as i64)
            .collect();
        let token_type_ids: Vec<i64> = encoding.get_type_ids().iter().map(|&t| t as i64).collect();

        let seq_len = input_ids.len();
        debug!("Embedding text ({} tokens): {:.50}...", seq_len, text);

        let mut session = self
            .session
            .lock()
            .map_err(|_| anyhow::anyhow!("session lock poisoned"))?;
        let outputs = session.run(ort::inputs! {
            "input_ids" => Tensor::from_array((vec![1usize, seq_len], input_ids.clone()))?,
            "attention_mask" => Tensor::from_array((vec![1usize, seq_len], attention_mask.clone()))?,
            "token_type_ids" => Tensor::from_array((vec![1usize, seq_len], token_type_ids))?,
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

        let pooled = mean_pool(data, &attention_mask, seq_len, EMBEDDING_DIM);
        let normalized = normalize(pooled);

        Ok(normalized)
    }

    pub fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        texts.iter().map(|t| self.embed(t)).collect()
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
}
