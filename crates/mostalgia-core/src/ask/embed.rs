//! Sentence embeddings: by default multilingual-e5-small (BERT, 384-dim),
//! run on the CPU with candle (the ONNX runtime that ships with sherpa-onnx
//! is too old for the `ort` crate, and candle needs no native library at
//! all); or a cloud model over an OpenAI-compatible `/embeddings` endpoint
//! on one of the configured connections ([`Model`]).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, anyhow};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config as BertConfig, DTYPE};
use tokenizers::{PaddingParams, Tokenizer, TruncationParams};

use crate::config::{Config, SttChoice};
use crate::connections::{Connection, api_error, http, unreachable};
use crate::i18n::{tr, trf};
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
        let config: BertConfig = serde_json::from_str(
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

/// Embeds with the shared local model; `None` when it isn't downloaded.
/// e5 texts carry their "query: " / "passage: " prefix.
fn embed_local(texts: &[String]) -> Result<Option<Vec<Vec<f32>>>> {
    if !available() {
        return Ok(None);
    }
    let mut slot = shared().lock().unwrap();
    if slot.is_none() {
        *slot = Some(Embedder::load(&model_dir())?);
    }
    slot.as_ref().unwrap().embed(texts).map(Some)
}

/// Key of the local model in the index state.
pub const LOCAL_KEY: &str = "local:e5-small";
/// Texts per cloud request.
const CLOUD_BATCH: usize = 64;
/// Longer passages are cut to this many characters before going to a
/// cloud model (well inside the usual 2–8k token input limits).
const CLOUD_MAX_CHARS: usize = 6000;

/// An embedding model: the local e5 model or a cloud one. Vectors of two
/// models can't be compared, so the index records which one made them.
#[derive(Clone, Debug, PartialEq)]
pub enum Model {
    Local,
    Cloud { conn: Connection, model: String },
    /// A cloud model whose connection is gone from the config.
    Missing { key: String },
}

impl Model {
    /// The model chosen in the settings.
    pub fn configured() -> Self {
        Self::from_choice(&Config::load())
    }

    pub fn from_choice(config: &Config) -> Self {
        let SttChoice { connection, model } = &config.embedding;
        if connection.is_empty() {
            return Model::Local;
        }
        match config.connection(connection) {
            Some(conn) => Model::Cloud { conn: conn.clone(), model: model.clone() },
            None => Model::Missing { key: format!("{connection}:{model}") },
        }
    }

    /// The model behind a key stored in the index state.
    pub fn from_key(key: &str, config: &Config) -> Self {
        if key == LOCAL_KEY {
            return Model::Local;
        }
        let (connection, model) = key.split_once(':').unwrap_or((key, ""));
        match config.connection(connection) {
            Some(conn) => Model::Cloud { conn: conn.clone(), model: model.to_string() },
            None => Model::Missing { key: key.to_string() },
        }
    }

    /// "local:e5-small" or "<connection id>:<model id>".
    pub fn key(&self) -> String {
        match self {
            Model::Local => LOCAL_KEY.into(),
            Model::Cloud { conn, model } => format!("{}:{model}", conn.id),
            Model::Missing { key } => key.clone(),
        }
    }

    /// Whether it can embed right now: downloaded, or set up with a key.
    pub fn ready(&self) -> bool {
        match self {
            Model::Local => available(),
            Model::Cloud { conn, model } => !model.is_empty() && conn.is_configured(),
            Model::Missing { .. } => false,
        }
    }

    /// Why it can't embed (for the settings and the CLI).
    pub fn not_ready_reason(&self) -> String {
        match self {
            Model::Local => tr("Bedeutungsmodell fehlt (scripts/download-model.sh)").into(),
            Model::Cloud { conn, model } if model.is_empty() => trf("{}: kein Embedding-Modell gewählt", &[&conn.name]),
            Model::Cloud { conn, .. } => trf("{}: kein API-Schlüssel hinterlegt (Einstellungen → KI)", &[&conn.name]),
            Model::Missing { key } => trf("Verbindung \"{}\" gibt es nicht mehr", &[&key.split(':').next().unwrap_or_default()]),
        }
    }

    /// Embeddings for passages to search in; `None` when the local model
    /// isn't downloaded.
    pub fn passages(&self, texts: &[String]) -> Result<Option<Vec<Vec<f32>>>> {
        self.embed(texts, "passage: ")
    }

    /// The embedding of a search query. A cloud model's answers are kept
    /// for a while, since live answers and retries search the same words.
    pub fn query(&self, text: &str) -> Result<Option<Vec<f32>>> {
        if !matches!(self, Model::Cloud { .. }) {
            return Ok(self.embed(&[text.to_string()], "query: ")?.and_then(|mut v| v.pop()));
        }
        let cache_key = (self.key(), text.to_string());
        if let Some(v) = query_cache().lock().unwrap_or_else(|e| e.into_inner()).get(&cache_key) {
            return Ok(Some(v.clone()));
        }
        let v = self.embed(&[text.to_string()], "query: ")?.and_then(|mut v| v.pop());
        if let Some(v) = &v {
            let mut cache = query_cache().lock().unwrap_or_else(|e| e.into_inner());
            if cache.len() >= QUERY_CACHE {
                cache.clear();
            }
            cache.insert(cache_key, v.clone());
        }
        Ok(v)
    }

    fn embed(&self, texts: &[String], e5_prefix: &str) -> Result<Option<Vec<Vec<f32>>>> {
        match self {
            Model::Local => embed_local(&texts.iter().map(|t| format!("{e5_prefix}{t}")).collect::<Vec<_>>()),
            Model::Cloud { conn, model } => embed_cloud(conn, model, texts).map(Some),
            Model::Missing { .. } => anyhow::bail!("{}", self.not_ready_reason()),
        }
    }
}

/// Cloud query embeddings kept (cleared when full).
const QUERY_CACHE: usize = 128;

fn query_cache() -> &'static Mutex<std::collections::HashMap<(String, String), Vec<f32>>> {
    static CELL: OnceLock<Mutex<std::collections::HashMap<(String, String), Vec<f32>>>> = OnceLock::new();
    CELL.get_or_init(Default::default)
}

/// Embeds over an OpenAI-compatible `/embeddings` endpoint, in batches;
/// vectors come back L2-normalized like the local ones.
fn embed_cloud(conn: &Connection, model: &str, texts: &[String]) -> Result<Vec<Vec<f32>>> {
    anyhow::ensure!(!model.is_empty(), "{}", trf("{}: kein Embedding-Modell gewählt", &[&conn.name]));
    // A keyless local server works without a reachable keyring too.
    let key = if conn.needs_key() { conn.require_key()? } else { conn.api_key().ok().flatten() };
    let client = http(120)?;
    let mut out = Vec::with_capacity(texts.len());
    for batch in texts.chunks(CLOUD_BATCH) {
        let input: Vec<String> = batch
            .iter()
            .map(|t| if t.chars().count() > CLOUD_MAX_CHARS { t.chars().take(CLOUD_MAX_CHARS).collect() } else { t.clone() })
            .collect();
        let mut req = client
            .post(format!("{}/embeddings", conn.base()))
            .json(&serde_json::json!({ "model": model, "input": input, "encoding_format": "float" }));
        if let Some(key) = &key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().map_err(|e| unreachable(conn, e))?;
        let status = resp.status();
        let body = resp.text().map_err(|e| unreachable(conn, e))?;
        if !status.is_success() {
            return Err(api_error(conn, status, &body));
        }
        out.extend(parse_embeddings(&body, batch.len()).with_context(|| trf("{}: unerwartete Antwort auf /embeddings", &[&conn.name]))?);
    }
    Ok(out)
}

/// The vectors of an `/embeddings` response, in input order, normalized.
fn parse_embeddings(body: &str, n: usize) -> Result<Vec<Vec<f32>>> {
    let value: serde_json::Value = serde_json::from_str(body)?;
    let data = value["data"].as_array().context("no data")?;
    let mut out: Vec<Option<Vec<f32>>> = vec![None; n];
    for (k, item) in data.iter().enumerate() {
        let index = item["index"].as_u64().map_or(k, |i| i as usize);
        let v: Vec<f32> = item["embedding"]
            .as_array()
            .context("no embedding")?
            .iter()
            .map(|x| x.as_f64().unwrap_or_default() as f32)
            .collect();
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        anyhow::ensure!(index < n && norm > 0.0, "bad embedding");
        out[index] = Some(v.into_iter().map(|x| x / norm).collect());
    }
    out.into_iter().map(|v| v.context("missing embedding")).collect()
}

/// Cosine similarity of two normalized vectors (0 when the sizes differ).
pub fn similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

pub fn to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn from_bytes(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_normalizes_out_of_order() {
        let body = r#"{"data":[{"index":1,"embedding":[0,2]},{"index":0,"embedding":[3,4]}]}"#;
        let v = parse_embeddings(body, 2).unwrap();
        assert_eq!(v, vec![vec![0.6, 0.8], vec![0.0, 1.0]]);
        assert!(parse_embeddings(r#"{"data":[{"index":0,"embedding":[1]}]}"#, 2).is_err());
    }

    #[test]
    fn keys_round_trip() {
        let config = Config::default();
        assert_eq!(Model::from_key(LOCAL_KEY, &config), Model::Local);
        let m = Model::from_key("openai:text-embedding-3-small", &config);
        assert!(matches!(&m, Model::Cloud { conn, model } if conn.id == "openai" && model == "text-embedding-3-small"));
        assert_eq!(m.key(), "openai:text-embedding-3-small");
        assert!(matches!(Model::from_key("gone:x", &config), Model::Missing { .. }));
        assert_eq!(similarity(&[1.0, 0.0], &[1.0]), 0.0);
    }
}
