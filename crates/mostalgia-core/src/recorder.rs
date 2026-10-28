use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    /// The language `source` is transcribed in (auto mode): detected,
    /// locked, switched, or picked by the user.
    Language { source: Source, status: crate::language::Status },
    /// Something went wrong but recording continues.
    Warning(String),
    /// The speech-to-text engine failed on a chunk (e.g. a cloud provider
    /// is unreachable). Recording continues to disk; the chunk's text is
    /// missing until the meeting is transcribed again.
    EngineError { engine: String, message: String },
    /// The live transcriber has been stuck on one piece of audio for
    /// `secs` seconds (sent about every second while it lasts); `secs` 0
    /// means it is working again. Recording to disk is not affected.
    Stalled { secs: u64 },
}

/// Seconds the live transcriber may spend on one block before the
/// recording shows it as stalled.
pub const STALL_SECS: u64 = 20;

/// What the watchdog reports at `now_ms`, given when the worker started on
/// its current block (`busy_ms`, 0 while it waits for audio; times are
/// milliseconds + 1 since the recording started) and whether a stall is
/// being shown: `Some(secs)` while stalled, `Some(0)` once when it ends.
fn watch(busy_ms: u64, now_ms: u64, shown: &mut bool) -> Option<u64> {
    let stuck = if busy_ms == 0 { 0 } else { now_ms.saturating_sub(busy_ms - 1) / 1000 };
    if stuck >= STALL_SECS {
        *shown = true;
        Some(stuck)
    } else if std::mem::take(shown) {
        Some(0)
    } else {
        None
    }
}

/// A finished recording on disk.
pub struct Recording {
    pub dir: PathBuf,
    pub started_at: DateTime<Local>,
    pub duration_secs: f64,
    pub tracks: Vec<(Source, PathBuf)>,
    /// The live transcript as it was shown, for when the final pass fails.
    pub live_lines: Vec<crate::finalize::FinalSegment>,
    /// Chunks the live engine failed on.
    pub live_errors: usize,
    /// Languages the user picked per source (mic, system) while recording;
    /// the final pass keeps them.
    pub languages: [Option<String>; 2],
}

/// Records the enabled sources to WAV files and transcribes them live on a
/// worker thread. Stop it with [`Recorder::stop`] (or a [`StopHandle`] from
/// a signal handler), then [`Recorder::join`] for the recording and the
/// engine back.
///
/// Sources can be switched off and on mid-recording
/// ([`Recorder::set_source`]). Both tracks always cover the whole
/// recording, sample for sample: a source that is off writes silence at the
/// rate it captures, and one switched on that wasn't recording yet starts
/// with silence up to where the other track is.
pub struct Recorder {
    shared: Arc<Shared>,
    dir: PathBuf,
    worker: JoinHandle<Result<(Recording, Engine)>>,
}

struct Shared {
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    /// Per source (mic, system): switched off (or not started).
    muted: [Arc<AtomicBool>; 2],
    /// Per source: samples written to its track.
    written: [Arc<AtomicU64>; 2],
    ctl: Mutex<Ctl>,
    /// Language picks from the user not yet applied, per source:
    /// `Some(None)` = back to detection.
    language_picks: Mutex<[Option<Option<String>>; 2]>,
}

struct Ctl {
    /// For captures started mid-recording; dropped on stop, so the worker
    /// ends once the running captures have.
    tx: Option<Sender<audio::Block>>,
    captures: Vec<JoinHandle<Result<()>>>,
    tracks: Vec<(Source, PathBuf)>,
    devices: [Option<String>; 2],
}

#[derive(Clone)]
pub struct StopHandle(Arc<Shared>);

impl StopHandle {
    pub fn stop(&self) {
        self.0.stop.store(true, Ordering::SeqCst);
        self.0.ctl.lock().unwrap_or_else(|e| e.into_inner()).tx = None;
    }
}

fn idx(s: Source) -> usize {
    match s {
        Source::Mic => 0,
        Source::System => 1,
    }
}

/// One source on the live path: its chunker and (mic) noise suppression.
struct LiveTrack {
    chunker: Chunker,
    denoiser: Option<crate::denoise::Denoiser>,
}

impl LiveTrack {
    /// Utterances completed by `block`. Silent blocks (the source is off,
    /// or padding before it joined) never reach the chunker as audio: they
    /// end the utterance in progress and move its clock on.
    fn feed(&mut self, block: &audio::Block) -> Vec<chunker::Utterance> {
        if block.silence > 0 {
            let mut utts = match self.denoiser.as_mut() {
                Some(d) => self.chunker.push(&d.flush()),
                None => Vec::new(),
            };
            utts.extend(self.chunker.skip(block.silence as u64));
            return utts;
        }
        match self.denoiser.as_mut() {
            Some(d) => self.chunker.push(&d.process(&block.samples)),
            None => self.chunker.push(&block.samples),
        }
    }

    fn finish(mut self) -> Vec<chunker::Utterance> {
        let mut utts = match self.denoiser.as_mut() {
            Some(d) => self.chunker.push(&d.flush()),
            None => Vec::new(),
        };
        utts.extend(self.chunker.finish());
        utts
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
        let vad_model = cfg.vad_model.clone();
        let vad_threshold = cfg.vad_threshold;
        let denoise = cfg.denoise;
        let load = move |source: Source| -> Result<LiveTrack> {
            Ok(LiveTrack {
                chunker: Chunker::new(SpeechDetector::load(&vad_model, vad_threshold)?),
                denoiser: (denoise && source == Source::Mic).then(crate::denoise::Denoiser::new),
            })
        };
        let mic_live = cfg.mic.then(|| load(Source::Mic)).transpose()?;
        let sys_live = cfg.system.then(|| load(Source::System)).transpose()?;
        if !cfg.vad_model.exists() {
            let _ = events.send(Event::Warning(crate::i18n::trf(
                "Sprachfilter-Modell {} fehlt — erkenne Sprache nur an der Lautstärke (scripts/download-model.sh)",
                &[&cfg.vad_model.display()],
            )));
        }

        let (block_tx, block_rx) = crossbeam_channel::bounded::<audio::Block>(256);
        let shared = Arc::new(Shared {
            stop: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
            muted: [Arc::new(AtomicBool::new(!cfg.mic)), Arc::new(AtomicBool::new(!cfg.system))],
            written: Default::default(),
            ctl: Mutex::new(Ctl {
                tx: Some(block_tx),
                captures: Vec::new(),
                tracks: Vec::new(),
                devices: [
                    cfg.mic_device.clone(),
                    Some(cfg.system_device.clone().unwrap_or_else(|| "@DEFAULT_MONITOR@".into())),
                ],
            }),
            language_picks: Mutex::new([None, None]),
        });
        let recorder_dir = dir.clone();
        {
            let mut ctl = shared.ctl.lock().unwrap();
            for (on, source) in [(cfg.mic, Source::Mic), (cfg.system, Source::System)] {
                if on && let Err(e) = open_capture(&shared, &mut ctl, &dir, source, false) {
                    // Don't leave the other source capturing.
                    shared.stop.store(true, Ordering::SeqCst);
                    return Err(e);
                }
            }
        }

        let diarize = cfg.diarize;
        let interim = cfg.interim;
        let worker_dir = dir.clone();
        let worker_shared = shared.clone();
        // The watchdog: notices a live transcriber stuck on one block
        // (a hung GPU call, an unanswered cloud request) and says so; it
        // ends with the worker (its clone of `busy` dropped).
        let origin = std::time::Instant::now();
        let busy = Arc::new(AtomicU64::new(0));
        {
            let busy = busy.clone();
            let events = events.clone();
            std::thread::spawn(move || {
                let mut shown = false;
                while Arc::strong_count(&busy) > 1 {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    let now = origin.elapsed().as_millis() as u64 + 1;
                    if let Some(secs) = watch(busy.load(Ordering::SeqCst), now, &mut shown) {
                        let _ = events.send(Event::Stalled { secs });
                    }
                }
            });
        }
        let worker = std::thread::spawn(move || -> Result<(Recording, Engine)> {
            let busy_since = |b: &AtomicU64| b.store(origin.elapsed().as_millis() as u64 + 1, Ordering::SeqCst);
            let mut engine = engine;
            let mut live = [mic_live, sys_live];
            // A source that joins mid-recording gets its live path then.
            let mut tried = [live[0].is_some(), live[1].is_some()];
            let mut speakers: Option<crate::diarize::LiveSpeakers> = None;
            let mut speakers_tried = !diarize;
            let mut samples_seen = [0u64; 2];
            let mut live_lines: Vec<crate::finalize::FinalSegment> = Vec::new();
            let mut live_errors = 0usize;
            // Auto mode: each source's language, detected on its first
            // speech, then locked (see `language`).
            let auto = engine.is_auto();
            let mut locks = [crate::language::LanguageLock::new(), crate::language::LanguageLock::new()];
            let mut run = |engine: &mut Engine, speakers: &mut Option<crate::diarize::LiveSpeakers>, locks: &mut [crate::language::LanguageLock; 2], source: Source, utts: Vec<chunker::Utterance>| {
                for utt in utts {
                    let t = std::time::Instant::now();
                    let lock = &mut locks[idx(source)];
                    let language = if auto && utt.is_loud_enough() {
                        let was_locked = lock.status().is_some_and(|s| s.locked);
                        match lock.choose(&utt.samples, utt.offset_secs, |audio| engine.detect_language(audio)) {
                            Ok((language, changed)) => {
                                let status = lock.status();
                                if let Some(status) = status.filter(|s| changed || s.locked != was_locked) {
                                    let _ = events.send(Event::Language { source, status });
                                }
                                language
                            }
                            Err(e) => {
                                let _ = events.send(Event::Warning(format!("{e:#}")));
                                lock.status().map(|s| s.language)
                            }
                        }
                    } else {
                        lock.status().map(|s| s.language)
                    };
                    match chunker::transcribe_utterance_as(engine, &utt, language.as_deref()) {
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
                                live_lines.push(crate::finalize::FinalSegment {
                                    source,
                                    start_secs: seg.start_secs,
                                    end_secs: seg.end_secs,
                                    text: seg.text.trim().to_string(),
                                    speaker: speaker.clone(),
                                });
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
                            live_errors += 1;
                            let _ = events.send(Event::EngineError {
                                engine: engine.label(),
                                message: format!("{e:#}"),
                            });
                        }
                    }
                }
            };

            // Interim passes: at most one per second per source, only when
            // the open chunk grew, and never ahead of finished chunks.
            let mut interim_at = [std::time::Instant::now(); 2];
            let mut interim_len = [0usize; 2];
            let mut interim_text: [String; 2] = Default::default();
            loop {
                busy.store(0, Ordering::SeqCst);
                let Ok(block) = block_rx.recv() else { break };
                busy_since(&busy);
                let i = idx(block.source);
                samples_seen[i] += (block.samples.len() + block.silence) as u64;
                let _ = events.send(Event::Level {
                    source: block.source,
                    rms: vad::rms(&block.samples),
                });
                if !tried[i] {
                    tried[i] = true;
                    match load(block.source) {
                        Ok(t) => live[i] = Some(t),
                        Err(e) => {
                            let _ = events.send(Event::Warning(format!("{e:#}")));
                        }
                    }
                }
                if block.source == Source::System && !speakers_tried && block.silence == 0 {
                    speakers_tried = true;
                    match crate::diarize::LiveSpeakers::new() {
                        Ok(s) => speakers = Some(s),
                        Err(e) => {
                            let _ = events.send(Event::Warning(crate::i18n::trf("Sprechertrennung aus: {}", &[&format!("{e:#}")])));
                        }
                    }
                }
                let picks = std::mem::take(&mut *worker_shared.language_picks.lock().unwrap_or_else(|e| e.into_inner()));
                for (j, pick) in picks.into_iter().enumerate() {
                    if let Some(pick) = pick {
                        locks[j].set_manual(pick);
                        let source = if j == 0 { Source::Mic } else { Source::System };
                        if let Some(status) = locks[j].status() {
                            let _ = events.send(Event::Language { source, status });
                        }
                    }
                }
                let Some(track) = live[i].as_mut() else { continue };
                let utts = track.feed(&block);
                let finished = !utts.is_empty();
                run(&mut engine, &mut speakers, &mut locks, block.source, utts);
                let clear_interim = |interim_len: &mut [usize; 2], interim_text: &mut [String; 2]| {
                    if interim_len[i] > 0 {
                        interim_len[i] = 0;
                        interim_text[i].clear();
                        let _ = events.send(Event::Interim {
                            source: block.source,
                            start_secs: 0.0,
                            text: String::new(),
                        });
                    }
                };
                // Switched off: whatever was provisional is gone or final now.
                if block.silence > 0 {
                    clear_interim(&mut interim_len, &mut interim_text);
                    continue;
                }
                // Cloud engines bill per request; no provisional passes.
                if !interim || engine.is_cloud() {
                    continue;
                }
                if finished {
                    clear_interim(&mut interim_len, &mut interim_text);
                }
                let chunker = &live[i].as_ref().expect("live track").chunker;
                let grew = chunker.open_len() > interim_len[i];
                // Only when the backlog is small, so live audio isn't starved.
                let caught_up = block_rx.len() < 4;
                if !finished && grew && caught_up && interim_at[i].elapsed().as_millis() >= 1000 {
                    if let Some(utt) = chunker.open_chunk().filter(|u| u.is_loud_enough()) {
                        interim_len[i] = utt.samples.len();
                        interim_at[i] = std::time::Instant::now();
                        let language = locks[i].status().map(|s| s.language);
                        if let Ok(segments) = engine.transcribe_as(&utt.samples, language.as_deref()) {
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
            // Capture ended: flush each source's last partial utterance.
            busy_since(&busy);
            for (i, source) in [(0, Source::Mic), (1, Source::System)] {
                if let Some(track) = live[i].take() {
                    run(&mut engine, &mut speakers, &mut locks, source, track.finish());
                }
            }

            let (captures, tracks) = {
                let mut ctl = worker_shared.ctl.lock().unwrap_or_else(|e| e.into_inner());
                (std::mem::take(&mut ctl.captures), ctl.tracks.clone())
            };
            for c in captures {
                if let Err(e) = c.join().expect("capture thread panicked") {
                    let _ = events.send(Event::Warning(format!("capture error: {e:#}")));
                }
            }
            drop(run);
            let languages = locks.map(|l| l.status().filter(|s| s.manual).map(|s| s.language));
            live_lines.sort_by(|a, b| a.start_secs.total_cmp(&b.start_secs));
            let duration_secs = *samples_seen.iter().max().unwrap_or(&0) as f64 / SAMPLE_RATE as f64;
            Ok((
                Recording {
                    dir: worker_dir,
                    started_at,
                    duration_secs,
                    tracks,
                    live_lines,
                    live_errors,
                    languages,
                },
                engine,
            ))
        });

        Ok(Self {
            shared,
            dir: recorder_dir,
            worker,
        })
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    pub fn stop_handle(&self) -> StopHandle {
        StopHandle(self.shared.clone())
    }

    pub fn stop(&self) {
        self.stop_handle().stop();
    }

    /// While paused nothing is recorded or transcribed; the tracks just
    /// continue where they left off on resume.
    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::SeqCst);
    }

    pub fn is_paused(&self) -> bool {
        self.shared.paused.load(Ordering::SeqCst)
    }

    /// Switches a source on or off for the rest of this recording (or until
    /// switched again). Off keeps its track running with silence and stops
    /// transcribing it; on for a source that wasn't recording opens it now,
    /// its track padded with silence from the start. Returns where on the
    /// recording's timeline (seconds) the switch happened.
    pub fn set_source(&self, source: Source, on: bool) -> Result<f64> {
        let mut ctl = self.shared.ctl.lock().unwrap_or_else(|e| e.into_inner());
        anyhow::ensure!(ctl.tx.is_some() && !self.shared.stop.load(Ordering::SeqCst), "the recording has ended");
        let opened = ctl.tracks.iter().any(|(s, _)| *s == source);
        if on && !opened {
            open_capture(&self.shared, &mut ctl, &self.dir, source, true)?;
        } else {
            self.shared.muted[idx(source)].store(!on, Ordering::SeqCst);
        }
        Ok(self.position_secs())
    }

    /// Transcribes `source` in `language` from its next utterance on
    /// (auto mode only), or goes back to detecting it (`None`).
    pub fn set_language(&self, source: Source, language: Option<String>) {
        self.shared.language_picks.lock().unwrap_or_else(|e| e.into_inner())[idx(source)] = Some(language);
    }

    /// Whether `source` is being recorded right now (open and not off).
    pub fn source_on(&self, source: Source) -> bool {
        !self.shared.muted[idx(source)].load(Ordering::SeqCst)
    }

    /// Recording time so far (without pauses): the longer track.
    pub fn position_secs(&self) -> f64 {
        let n = self.shared.written.iter().map(|w| w.load(Ordering::SeqCst)).max().unwrap_or(0);
        n as f64 / SAMPLE_RATE as f64
    }

    /// Waits for capture to end (after a stop) and the live transcript to flush.
    pub fn join(self) -> Result<(Recording, Engine)> {
        self.worker.join().expect("recorder thread panicked")
    }
}

/// Opens `source`'s capture into its track. `late`: the recording is
/// already running, so the track starts with silence up to the other one.
fn open_capture(shared: &Shared, ctl: &mut Ctl, dir: &std::path::Path, source: Source, late: bool) -> Result<()> {
    let i = idx(source);
    let tx = ctl.tx.clone().context("the recording has ended")?;
    let path = dir.join(source.file_name());
    shared.muted[i].store(false, Ordering::SeqCst);
    let capture = audio::spawn_capture(
        source,
        ctl.devices[i].clone(),
        path.clone(),
        tx,
        audio::CaptureCtl {
            stop: shared.stop.clone(),
            paused: shared.paused.clone(),
            muted: shared.muted[i].clone(),
            written: shared.written[i].clone(),
            join_at: late.then(|| shared.written[1 - i].clone()),
        },
    );
    match capture {
        Ok(handle) => {
            ctl.captures.push(handle);
            ctl.tracks.push((source, path));
            Ok(())
        }
        Err(e) => {
            shared.muted[i].store(true, Ordering::SeqCst);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchdog_reports_a_stall_and_its_end() {
        let mut shown = false;
        let s = 1000 + 1; // busy since 1 s in
        assert_eq!(watch(0, 60_000, &mut shown), None, "waiting for audio is fine");
        assert_eq!(watch(s, 1000 + 19_999, &mut shown), None);
        assert_eq!(watch(s, 1000 + STALL_SECS * 1000, &mut shown), Some(STALL_SECS));
        assert_eq!(watch(s, 1000 + 45_500, &mut shown), Some(45));
        assert_eq!(watch(0, 50_000, &mut shown), Some(0), "working again");
        assert_eq!(watch(0, 51_000, &mut shown), None);
    }

    fn track(denoise: bool) -> LiveTrack {
        LiveTrack {
            chunker: Chunker::new(SpeechDetector::load(std::path::Path::new("/nonexistent/vad.bin"), 0.5).unwrap()),
            denoiser: denoise.then(crate::denoise::Denoiser::new),
        }
    }

    fn live(secs: f32) -> audio::Block {
        let n = (secs * SAMPLE_RATE as f32) as usize;
        audio::Block { source: Source::Mic, samples: (0..n).map(|i| 0.3 * (i as f32 * 0.07).sin()).collect(), silence: 0 }
    }

    fn off(secs: f32) -> audio::Block {
        audio::Block { source: Source::Mic, samples: Vec::new(), silence: (secs * SAMPLE_RATE as f32) as usize }
    }

    /// Nothing is transcribed while a source is off, and what comes after
    /// keeps its time.
    #[test]
    fn nothing_reaches_the_transcriber_while_off() {
        for denoise in [false, true] {
            let mut t = track(denoise);
            // Padding before a late start, then a long stretch switched off.
            assert!(t.feed(&off(3.0)).is_empty());
            for _ in 0..100 {
                assert!(t.feed(&off(0.1)).is_empty(), "silence never makes an utterance");
            }
            let mut utts = t.feed(&live(2.0));
            utts.extend(t.feed(&off(4.0)));
            assert_eq!(utts.len(), 1, "speech before the switch is flushed");
            assert!((utts[0].offset_secs - 13.0).abs() < 0.4, "starts after 3 s + 10 s off: {}", utts[0].offset_secs);
            assert!(t.finish().is_empty());
        }
    }
}
