//! Sentence embeddings with multilingual-e5-small (BERT, 384-dim), run on
//! the CPU with candle. The ONNX runtime that ships with sherpa-onnx is too
//! old for the `ort` crate, and candle needs no native library at all.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, anyhow};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, DTYPE};
use tokenizers::{PaddingParams, Tokenizer, TruncationParams};

use crate::paths;

pub const DIM: usize = 384;
/// Passages are truncated to this many tokens (the model allows 512).
const MAX_TOKENS: usize = 512;
/// Passages embedded per forward pass. One at a time is fastest with
/// candle on the CPU (no padding; batches of 8 were ~2.3x slower).
const BATCH: usize = 1;

pub fn model_dir() -> PathBuf {
    paths::data_dir().join("models").join("e5-small")
}

/// Whether the embedding model is downloaded.
pub fn available() -> bool {
    let dir = model_dir();
    ["model.safetensors", "tokenizer.json", "config.json"]
        .iter()
        .all(|f| dir.join(f).exists())
}

pub struct Embedder {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
}

impl Embedder {
    pub fn load(dir: &Path) -> Result<Self> {
        let device = Device::Cpu;
        let config: Config = serde_json::from_str(
            &std::fs::read_to_string(dir.join("config.json")).context("reading e5 config.json")?,
        )?;
        let mut tokenizer = Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|e| anyhow!("loading tokenizer: {e}"))?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: MAX_TOKENS,
                ..Default::default()
            }))
            .map_err(|e| anyhow!("{e}"))?;
        tokenizer.with_padding(Some(PaddingParams::default()));
        // SAFETY: the model file is only read, and not modified while mapped.
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[dir.join("model.safetensors")], DTYPE, &device)? };
        let model = BertModel::load(vb, &config).context("loading e5 model")?;
        Ok(Self { model, tokenizer, device })
    }

    /// L2-normalized embeddings. e5 expects "query: " / "passage: " prefixes;
    /// callers pass the full text including the prefix.
    pub fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(BATCH) {
            let enc = self
                .tokenizer
                .encode_batch(batch.to_vec(), true)
                .map_err(|e| anyhow!("tokenizing: {e}"))?;
            let ids: Vec<Tensor> = enc
                .iter()
                .map(|e| Tensor::new(e.get_ids(), &self.device))
                .collect::<candle_core::Result<_>>()?;
            let masks: Vec<Tensor> = enc
                .iter()
                .map(|e| Tensor::new(e.get_attention_mask(), &self.device))
                .collect::<candle_core::Result<_>>()?;
            let ids = Tensor::stack(&ids, 0)?;
            let mask = Tensor::stack(&masks, 0)?;
            let types = ids.zeros_like()?;
            let hidden = self.model.forward(&ids, &types, Some(&mask))?; // (b, t, h)
            // Mean pooling over real tokens.
            let m = mask.to_dtype(DType::F32)?.unsqueeze(2)?; // (b, t, 1)
            let summed = hidden.broadcast_mul(&m)?.sum(1)?; // (b, h)
            let counts = m.sum(1)?; // (b, 1)
            let mean = summed.broadcast_div(&counts)?;
            let norm = mean.sqr()?.sum_keepdim(1)?.sqrt()?;
            let normed = mean.broadcast_div(&norm)?;
            out.extend(normed.to_vec2::<f32>()?);
        }
        Ok(out)
    }
}

/// The process-wide embedder, loaded on first use (≈0.5 s) and kept.
fn shared() -> &'static Mutex<Option<Embedder>> {
    static CELL: OnceLock<Mutex<Option<Embedder>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

/// Embeds with the shared model; `None` when the model isn't downloaded.
pub fn embed(texts: &[String]) -> Result<Option<Vec<Vec<f32>>>> {
    if !available() {
        return Ok(None);
    }
    let mut slot = shared().lock().unwrap();
    if slot.is_none() {
        *slot = Some(Embedder::load(&model_dir())?);
    }
    slot.as_ref().unwrap().embed(texts).map(Some)
}

pub fn to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn from_bytes(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}
