//! The model files Mostalgia needs, and downloading the missing ones into
//! the data directory's `models/` (what `scripts/download-model.sh` does,
//! for installs without a source checkout: the app offers it on first
//! launch, `mostalgia models download` does it from the terminal).

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

use crate::paths;

/// What a model is for (shown when asking, and so a later version can let
/// people pick).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// Whisper and the voice activity detector: local transcription.
    Transcription,
    /// pyannote + WeSpeaker: telling speakers apart.
    Speakers,
    /// multilingual-e5-small: the search behind *Ask*.
    Search,
}

/// One file to download.
#[derive(Clone, Copy, Debug)]
pub struct Model {
    /// Path below `models/`.
    pub file: &'static str,
    pub url: &'static str,
    /// Download size in bytes (for the progress bar before the server
    /// says; close is enough).
    pub size: u64,
    pub purpose: Purpose,
    /// The download is a `.tar.bz2`; this member of it is the model.
    pub archive_member: Option<&'static str>,
}

pub const MODELS: &[Model] = &[
    Model {
        file: paths::WHISPER_MODEL,
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin",
        size: 1_624_555_275,
        purpose: Purpose::Transcription,
        archive_member: None,
    },
    Model {
        file: paths::VAD_MODEL,
        url: "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin",
        size: 885_098,
        purpose: Purpose::Transcription,
        archive_member: None,
    },
    Model {
        file: crate::diarize::SEGMENTATION_MODEL,
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2",
        size: 6_958_444,
        purpose: Purpose::Speakers,
        archive_member: Some("sherpa-onnx-pyannote-segmentation-3-0/model.onnx"),
    },
    Model {
        file: crate::diarize::EMBEDDING_MODEL,
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_resnet34_LM.onnx",
        size: 26_530_550,
        purpose: Purpose::Speakers,
        archive_member: None,
    },
    Model {
        file: "e5-small/model.safetensors",
        url: "https://huggingface.co/intfloat/multilingual-e5-small/resolve/main/model.safetensors",
        size: 470_637_416,
        purpose: Purpose::Search,
        archive_member: None,
    },
    Model {
        file: "e5-small/tokenizer.json",
        url: "https://huggingface.co/intfloat/multilingual-e5-small/resolve/main/tokenizer.json",
        size: 17_082_730,
        purpose: Purpose::Search,
        archive_member: None,
    },
    Model {
        file: "e5-small/config.json",
        url: "https://huggingface.co/intfloat/multilingual-e5-small/resolve/main/config.json",
        size: 655,
        purpose: Purpose::Search,
        archive_member: None,
    },
];

/// Where downloads go: the data directory's `models/`.
pub fn dir() -> PathBuf {
    paths::data_dir().join("models")
}

impl Model {
    pub fn path(&self) -> PathBuf {
        dir().join(self.file)
    }

    /// Present in the data directory, or (Whisper, VAD and speaker models)
    /// in a source checkout's `./models/`.
    pub fn present(&self) -> bool {
        let found = |p: &Path| p.metadata().is_ok_and(|m| m.len() > 0);
        found(&self.path()) || (!self.file.contains('/') && found(&paths::model(self.file)))
    }
}

/// The models that still need downloading.
pub fn missing() -> Vec<Model> {
    MODELS.iter().filter(|m| !m.present()).copied().collect()
}

/// Bytes a download of `models` takes, roughly.
pub fn total_size(models: &[Model]) -> u64 {
    models.iter().map(|m| m.size).sum()
}

/// How far a download has got.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    /// The file being downloaded.
    pub file: String,
    /// Bytes so far and in all, over every file of this download.
    pub done: u64,
    pub total: u64,
}

impl Progress {
    pub fn fraction(&self) -> f32 {
        if self.total == 0 { 0.0 } else { (self.done as f64 / self.total as f64).min(1.0) as f32 }
    }
}

/// Downloads `models` one after the other, calling `progress` as bytes
/// arrive. A cancelled or broken download keeps its `.part` file and
/// carries on from there next time. Returns an error naming the file that
/// failed, or "cancelled" when `cancel` was set.
pub fn download(models: &[Model], cancel: &AtomicBool, mut progress: impl FnMut(&Progress)) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        // No overall timeout: Whisper alone is 1.6 GB. Stalls end the read.
        .timeout(None)
        .user_agent(concat!("Mostalgia/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let mut state = Progress { total: total_size(models), ..Default::default() };
    for model in models {
        state.file = model.file.to_string();
        progress(&state);
        let before = state.done;
        fetch(&client, model, cancel, &mut state, &mut progress)
            .with_context(|| format!("{}: {}", crate::i18n::tr("Download fehlgeschlagen"), model.file))?;
        // The real size may differ from the estimate; count the estimate.
        state.done = before + model.size;
        progress(&state);
    }
    Ok(())
}

fn fetch(
    client: &reqwest::blocking::Client,
    model: &Model,
    cancel: &AtomicBool,
    state: &mut Progress,
    progress: &mut impl FnMut(&Progress),
) -> Result<()> {
    let dest = model.path();
    fs::create_dir_all(dest.parent().unwrap())?;
    let part = PathBuf::from(format!("{}.part", dest.display()));
    let have = part.metadata().map(|m| m.len()).unwrap_or(0);
    let mut request = client.get(model.url);
    if have > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let mut response = request.send()?;
    let status = response.status();
    if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE && have > 0 {
        // The .part is already complete.
    } else if !status.is_success() {
        bail!("HTTP {status}");
    } else {
        let resumed = status == reqwest::StatusCode::PARTIAL_CONTENT;
        let mut file = if resumed {
            OpenOptions::new().append(true).open(&part)?
        } else {
            File::create(&part)?
        };
        let start = state.done;
        let mut got = if resumed { have } else { 0 };
        // What's left to come, when the server says (0 can mean unknown).
        let remaining = response.content_length().filter(|&l| l > 0);
        // Scale this file's bytes to its share of the estimated total.
        let length = remaining.map(|l| l + got).unwrap_or(model.size);
        let mut buf = vec![0u8; 256 * 1024];
        let mut last = std::time::Instant::now();
        loop {
            if cancel.load(Ordering::Relaxed) {
                bail!("{}", crate::i18n::tr("abgebrochen"));
            }
            let n = response.read(&mut buf)?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n])?;
            got += n as u64;
            if last.elapsed() >= Duration::from_millis(100) {
                last = std::time::Instant::now();
                state.done = start + (model.size as f64 * (got as f64 / length as f64).min(1.0)) as u64;
                progress(state);
            }
        }
        file.sync_all()?;
        if let Some(expected) = remaining {
            let expected = expected + if resumed { have } else { 0 };
            if got != expected {
                bail!("{got} of {expected} bytes");
            }
        }
    }
    match model.archive_member {
        None => fs::rename(&part, &dest)?,
        Some(member) => {
            unpack(&part, member, &dest)?;
            fs::remove_file(&part)?;
        }
    }
    Ok(())
}

/// Takes `member` out of the `.tar.bz2` at `archive` and writes it to `dest`.
fn unpack(archive: &Path, member: &str, dest: &Path) -> Result<()> {
    let mut tar = tar::Archive::new(bzip2::read::BzDecoder::new(File::open(archive)?));
    for entry in tar.entries()? {
        let mut entry = entry?;
        if entry.path()?.to_str() == Some(member) {
            let tmp = PathBuf::from(format!("{}.unpack", dest.display()));
            std::io::copy(&mut entry, &mut File::create(&tmp)?)?;
            fs::rename(&tmp, dest)?;
            return Ok(());
        }
    }
    Err(anyhow!("{member} not found in the archive"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_model_has_a_unique_https_source() {
        let mut files: Vec<_> = MODELS.iter().map(|m| m.file).collect();
        files.sort();
        files.dedup();
        assert_eq!(files.len(), MODELS.len());
        assert!(MODELS.iter().all(|m| m.url.starts_with("https://") && m.size > 0));
    }

    #[test]
    fn unpacks_one_member_of_a_tar_bz2() {
        let dir = std::env::temp_dir().join(format!("mostalgia-models-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("a.tar.bz2");
        {
            let bz = bzip2::write::BzEncoder::new(File::create(&archive).unwrap(), bzip2::Compression::fast());
            let mut tar = tar::Builder::new(bz);
            for (name, body) in [("x/readme.txt", &b"hi"[..]), ("x/model.onnx", &b"weights"[..])] {
                let mut header = tar::Header::new_gnu();
                header.set_size(body.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                tar.append_data(&mut header, name, body).unwrap();
            }
            tar.into_inner().unwrap().finish().unwrap();
        }
        let dest = dir.join("model.onnx");
        unpack(&archive, "x/model.onnx", &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"weights");
        assert!(unpack(&archive, "x/missing", &dir.join("m")).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn progress_fraction_stays_in_range() {
        assert_eq!(Progress::default().fraction(), 0.0);
        assert_eq!(Progress { done: 5, total: 4, ..Default::default() }.fraction(), 1.0);
    }
}
