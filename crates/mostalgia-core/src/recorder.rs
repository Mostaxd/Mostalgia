use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use crossbeam_channel::Sender;

use crate::audio::{self, Source};
use crate::chunker::{self, Chunker};
use crate::transcribe::Engine;
use crate::vad::{self, SpeechDetector};
use crate::SAMPLE_RATE;

pub struct RecordConfig {
    /// Silero VAD model; the live path falls back to an energy gate without it.
    pub vad_model: PathBuf,
    /// Silero speech threshold (0–1, lower = more sensitive).
    pub vad_threshold: f32,
    /// Parent directory; each recording gets its own `session-<ts>` folder.
    pub out_dir: PathBuf,
    pub mic: bool,
    pub system: bool,
    /// Pulse source for the mic (`None` = default source).
    pub mic_device: Option<String>,
    /// Pulse source for system audio (`None` = monitor of the default output).
    pub system_device: Option<String>,
    /// Tell remote speakers apart live (needs the diarization models).
    pub diarize: bool,
    /// Noise suppression on the mic before live transcription (the WAV
    /// stays raw).
    pub denoise: bool,
    /// Emit provisional text for the chunk still being spoken.
    pub interim: bool,
}

/// What a running recording reports to its front end.
pub enum Event {
    /// A live transcript line. Times are seconds from the recording start.
    Line {
        source: Source,
        /// "Sprecher N" within the source, when diarization is on and has
        /// heard more than one voice.
        speaker: Option<String>,
        start_secs: f64,
        end_secs: f64,
        text: String,
    },
    /// Provisional text for the utterance still being spoken on `source`;
    /// replaced by `Line`s when it ends. An empty `text` clears it.
    Interim {
        source: Source,
        start_secs: f64,
        text: String,
    },
    /// RMS level of the latest 100 ms block, for level meters.
    Level { source: Source, rms: f32 },
    /// A chunk went through Whisper (for latency display).
    Chunk {
        source: Source,
        audio_secs: f64,
        millis: u128,
    },
    /// Something went wrong but recording continues.
    Warning(String),
}

/// A finished recording on disk.
pub struct Recording {
    pub dir: PathBuf,
    pub started_at: DateTime<Local>,
    pub duration_secs: f64,
    pub tracks: Vec<(Source, PathBuf)>,
}

/// Records the enabled sources to WAV files and transcribes them live on a
/// worker thread. Stop it with [`Recorder::stop`] (or a [`StopHandle`] from
/// a signal handler), then [`Recorder::join`] for the recording and the
/// engine back.
pub struct Recorder {
    stop: StopHandle,
    paused: Arc<AtomicBool>,
    dir: PathBuf,
    worker: JoinHandle<Result<(Recording, Engine)>>,
}

#[derive(Clone)]
pub struct StopHandle(Arc<AtomicBool>);

impl StopHandle {
    pub fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl Recorder {
    pub fn start(cfg: RecordConfig, engine: Engine, events: Sender<Event>) -> Result<Self> {
        anyhow::ensure!(cfg.mic || cfg.system, "nothing to record: enable the mic or system audio");
        let started_at = Local::now();
        let dir = cfg.out_dir.join(format!("session-{}", started_at.timestamp()));
        std::fs::create_dir_all(&dir).context("creating session directory")?;

        // Each source gets its own detector and chunker, so one side's noise
        // never bleeds into the other's transcript.
        let load = |on: bool| -> Result<Option<Chunker>> {
            on.then(|| SpeechDetector::load(&cfg.vad_model, cfg.vad_threshold).map(Chunker::new))
                .transpose()
        };
        let mic_live = load(cfg.mic)?;
        let sys_live = load(cfg.system)?;
        if !cfg.vad_model.exists() {
            let _ = events.send(Event::Warning(format!(
                "VAD model {} not found — using the energy gate",
                cfg.vad_model.display()
            )));
        }

        let stop = StopHandle(Arc::new(AtomicBool::new(false)));
        let paused = Arc::new(AtomicBool::new(false));
        let (block_tx, block_rx) = crossbeam_channel::bounded::<audio::Block>(256);
        let mut captures = Vec::new();
        let mut tracks = Vec::new();
        for (on, source, device) in [
            (cfg.mic, Source::Mic, cfg.mic_device.clone()),
            (
                cfg.system,
                Source::System,
                Some(cfg.system_device.clone().unwrap_or_else(|| "@DEFAULT_MONITOR@".into())),
            ),
        ] {
            if !on {
                continue;
            }
            let path = dir.join(source.file_name());
            captures.push(audio::spawn_capture(
                source,
                device,
                path.clone(),
                block_tx.clone(),
                stop.0.clone(),
                paused.clone(),
            )?);
            tracks.push((source, path));
        }
        drop(block_tx);

        let mut speakers = if cfg.diarize && cfg.system {
            match crate::diarize::LiveSpeakers::new() {
                Ok(s) => Some(s),
                Err(e) => {
                    let _ = events.send(Event::Warning(format!("speaker detection off: {e:#}")));
                    None
                }
            }
        } else {
            None
        };
        let mut denoiser = (cfg.denoise && cfg.mic).then(crate::denoise::Denoiser::new);
        let interim = cfg.interim;
        let worker_dir = dir.clone();
        let worker = std::thread::spawn(move || -> Result<(Recording, Engine)> {
            let mut engine = engine;
            let mut live = [mic_live, sys_live];
            let mut samples_seen = [0u64; 2];
            let idx = |s: Source| match s {
                Source::Mic => 0,
                Source::System => 1,
            };
            let mut run = |engine: &mut Engine, source: Source, utts: Vec<chunker::Utterance>| {
                for utt in utts {
                    let t = std::time::Instant::now();
                    match chunker::transcribe_utterance(engine, &utt) {
                        Ok(segments) => {
                            let speaker = match (source, speakers.as_mut()) {
                                (Source::System, Some(s)) if !segments.is_empty() => s.identify(&utt.samples),
                                _ => None,
                            };
                            let _ = events.send(Event::Chunk {
                                source,
                                audio_secs: utt.samples.len() as f64 / SAMPLE_RATE as f64,
                                millis: t.elapsed().as_millis(),
                            });
                            for seg in segments {
                                let _ = events.send(Event::Line {
                                    source,
                                    speaker: speaker.clone(),
                                    start_secs: seg.start_secs,
                                    end_secs: seg.end_secs,
                                    text: seg.text,
                                });
                            }
                        }
                        Err(e) => {
                            let _ = events.send(Event::Warning(format!("transcription error: {e:#}")));
                        }
                    }
                }
            };

            // Interim passes: at most one per second per source, only when
            // the open chunk grew, and never ahead of finished chunks.
            let mut interim_at = [std::time::Instant::now(); 2];
            let mut interim_len = [0usize; 2];
            let mut interim_text: [String; 2] = Default::default();
            for block in block_rx.iter() {
                let i = idx(block.source);
                samples_seen[i] += block.samples.len() as u64;
                let _ = events.send(Event::Level {
                    source: block.source,
                    rms: vad::rms(&block.samples),
                });
                if let Some(chunker) = live[i].as_mut() {
                    let utts = match (block.source, denoiser.as_mut()) {
                        (Source::Mic, Some(d)) => chunker.push(&d.process(&block.samples)),
                        _ => chunker.push(&block.samples),
                    };
                    let finished = !utts.is_empty();
                    run(&mut engine, block.source, utts);
                    if !interim {
                        continue;
                    }
                    if finished && interim_len[i] > 0 {
                        interim_len[i] = 0;
                        interim_text[i].clear();
                        let _ = events.send(Event::Interim {
                            source: block.source,
                            start_secs: 0.0,
                            text: String::new(),
                        });
                    }
                    let chunker = live[i].as_ref().expect("chunker");
                    let grew = chunker.open_len() > interim_len[i];
                    // Only when the backlog is small, so live audio isn't starved.
                    let caught_up = block_rx.len() < 4;
                    if !finished && grew && caught_up && interim_at[i].elapsed().as_millis() >= 1000 {
                        if let Some(utt) = chunker.open_chunk().filter(|u| vad::rms(&u.samples) >= chunker::MIN_UTTERANCE_RMS) {
                            interim_len[i] = utt.samples.len();
                            interim_at[i] = std::time::Instant::now();
                            if let Ok(segments) = engine.transcribe(&utt.samples) {
                                let text = segments.iter().map(|s| s.text.trim()).collect::<Vec<_>>().join(" ");
                                // A pass that ends mid-word can come back as a
                                // filler phrase or much shorter; keep the last one then.
                                let filler = chunker::is_filler_hallucination(&text, &[]);
                                let regressed = text.chars().count() * 2 < interim_text[i].chars().count();
                                if !text.is_empty() && !filler && !regressed {
                                    interim_text[i] = text.clone();
                                    let _ = events.send(Event::Interim {
                                        source: block.source,
                                        start_secs: utt.offset_secs,
                                        text,
                                    });
                                }
                            }
                        }
                    }
                }
            }
            if let (Some(d), Some(chunker)) = (denoiser.as_mut(), live[0].as_mut()) {
                let utts = chunker.push(&d.flush());
                run(&mut engine, Source::Mic, utts);
            }
            // Capture ended: flush each source's last partial utterance.
            for (i, source) in [(0, Source::Mic), (1, Source::System)] {
                if let Some(chunker) = live[i].take() {
                    run(&mut engine, source, chunker.finish());
                }
            }

            for c in captures {
                if let Err(e) = c.join().expect("capture thread panicked") {
                    let _ = events.send(Event::Warning(format!("capture error: {e:#}")));
                }
            }
            let duration_secs = *samples_seen.iter().max().unwrap_or(&0) as f64 / SAMPLE_RATE as f64;
            Ok((
                Recording {
                    dir: worker_dir,
                    started_at,
                    duration_secs,
                    tracks,
                },
                engine,
            ))
        });

        Ok(Self {
            stop,
            paused,
            dir,
            worker,
        })
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    pub fn stop_handle(&self) -> StopHandle {
        self.stop.clone()
    }

    pub fn stop(&self) {
        self.stop.stop();
    }

    /// While paused nothing is recorded or transcribed; the tracks just
    /// continue where they left off on resume.
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// Waits for capture to end (after a stop) and the live transcript to flush.
    pub fn join(self) -> Result<(Recording, Engine)> {
        self.worker.join().expect("recorder thread panicked")
    }
}
