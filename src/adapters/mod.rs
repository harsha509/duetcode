pub mod claude;
pub mod gemini;

use anyhow::Result;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct ImageInput {
    #[allow(dead_code)]
    pub path: PathBuf,
    pub media_type: String,
    pub data: Vec<u8>,
}

impl ImageInput {
    pub fn load(path: PathBuf) -> Result<Self> {
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let media_type = match extension.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            other => {
                anyhow::bail!(
                    "unsupported image format '.{}' — use png, jpg, gif, or webp",
                    other
                );
            }
        }
        .to_string();

        let data =
            std::fs::read(&path).map_err(|e| anyhow::anyhow!("cannot read {}: {}", path.display(), e))?;

        Ok(Self {
            path,
            media_type,
            data,
        })
    }

    pub fn base64_data(&self) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(&self.data)
    }
}

pub trait ModelAdapter {
    fn generate(
        &self,
        prompt: &str,
        context: &str,
        images: &[ImageInput],
    ) -> Result<String>;

    fn name(&self) -> &str;

    /// Whether the adapter already streams output to the terminal during generate().
    fn streams_output(&self) -> bool {
        false
    }
}
