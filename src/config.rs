use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub redis_url: String,
    pub model_cache_dir: PathBuf,
}

impl Config {
    pub fn from_env() -> Self {
        let redis_url =
            std::env::var("KT_REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());
        let model_cache_dir = std::env::var("KT_MODEL_CACHE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::cache_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join("kt")
            });
        Self {
            redis_url,
            model_cache_dir,
        }
    }

    pub fn model_path(&self) -> PathBuf {
        self.model_cache_dir.join("model.onnx")
    }

    pub fn tokenizer_path(&self) -> PathBuf {
        self.model_cache_dir.join("tokenizer.json")
    }
}
