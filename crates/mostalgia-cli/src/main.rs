use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, TimeZone};
use clap::{Parser, Subcommand, ValueEnum};
use mostalgia_core::chunker::format_ts;
use mostalgia_core::recorder::{Event, RecordConfig, Recorder, Recording};
use mostalgia_core::store::{NewMeeting, Store};
use mostalgia_core::transcribe::Engine;
use mostalgia_core::{Source, audio, finalize, paths, summary};

#[derive(Parser)]
#[command(name = "mostalgia", version, about = "Local meeting recorder with live Whisper transcription")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct ModelArgs {
    /// Path to a ggml Whisper model [default: models/ggml-large-v3-turbo.bin,
    /// else ~/.local/share/mostalgia/models/]
    #[arg(long)]
    model: Option<PathBuf>,
    /// Silero VAD model used to find speech [default: next to the Whisper model]
    #[arg(long)]
    vad_model: Option<PathBuf>,
    /// Language to transcribe ("de", "en", or "auto")
    #[arg(long, default_value = "de")]
    language: String,
    /// Tell several speakers on the system-audio side apart ("Sprecher 1",
    /// "Sprecher 2", …). Also on when enabled in the app's settings.
    #[arg(long)]
    diarize: bool,
    /// Suppress steady background noise (fans, hum) on the mic before
    /// transcribing; the recorded WAV stays untouched
    #[arg(long)]
    denoise: bool,
}

#[derive(clap::Args)]
struct SummaryArgs {
    /// Don't generate a summary after the final pass
    #[arg(long)]
    no_summary: bool,
    #[command(flatten)]
    choice: SummaryChoice,
}

/// Which provider/model/thinking level summarizes; defaults from config.json.
#[derive(clap::Args)]
struct SummaryChoice {
    /// Summary provider: claude or deepseek
    #[arg(long)]
    provider: Option<String>,
    /// Model id, e.g. claude-opus-5-5, claude-sonnet-5-5, deepseek-chat
    #[arg(long)]
    summary_model: Option<String>,
    /// Thinking level: low, medium, high, xhigh or max (Claude only)
    #[arg(long)]
    effort: Option<String>,
}

impl SummaryChoice {
    fn request(&self) -> Result<summary::SummaryRequest> {
        let config = mostalgia_core::config::Config::load();
        let mut req = config.summary_request();
        if let Some(p) = &self.provider {
            let provider = summary::Provider::parse(p).with_context(|| format!("unknown provider {p:?} (claude, deepseek)"))?;
            if provider != req.provider {
                req.provider = provider;
                req.model = provider.default_model().into();
            }
        }
        if let Some(m) = &self.summary_model {
            req.model = m.clone();
        }
        if req.provider == summary::Provider::DeepSeek {
            req.effort = None;
        } else if let Some(e) = &self.effort {
            anyhow::ensure!(summary::EFFORTS.contains(&e.as_str()), "unknown effort {e:?} (low, medium, high, xhigh, max)");
            req.effort = Some(e.clone());
        } else if req.effort.is_none() {
            req.effort = Some(config.summary_effort.clone());
        }
        Ok(req)
    }
}

#[derive(Subcommand)]
enum Command {
    /// Record, print a live transcript, then save a final transcript and summary. Ctrl+C stops.
    Start {
        #[command(flatten)]
        models: ModelArgs,
        #[command(flatten)]
        summary: SummaryArgs,
        /// Directory for the recordings' WAV tracks [default: ~/.local/share/mostalgia/recordings]
        #[arg(long)]
        out: Option<PathBuf>,
        /// What to record: your microphone, system audio (the other side
        /// of a call), or both
        #[arg(long, value_enum, default_value_t = SourceChoice::Both)]
        source: SourceChoice,
        /// Deprecated: same as --source system
        #[arg(long, hide = true, conflicts_with_all = ["source", "no_system"])]
        no_mic: bool,
        /// Deprecated: same as --source mic
        #[arg(long, hide = true, conflicts_with = "source")]
        no_system: bool,
        /// Label live lines by who spoke: "Ich" (mic) or "Andere" (system audio)
        #[arg(long)]
        speakers: bool,
        /// Pulse source name for the mic (defaults to the default source)
        #[arg(long)]
        mic_device: Option<String>,
        /// Pulse source name for system audio (defaults to @DEFAULT_MONITOR@)
        #[arg(long)]
        system_device: Option<String>,
        /// Only record and show the live transcript; skip the final pass,
        /// library and summary
        #[arg(long)]
        no_final: bool,
    },
    /// Run the final pass on an existing recording folder and add it to the library
    Import {
        /// A session folder containing mic.wav and/or system.wav
        dir: PathBuf,
        #[command(flatten)]
        models: ModelArgs,
        #[command(flatten)]
        summary: SummaryArgs,
    },
    /// List recorded meetings, newest first
    List {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Show a meeting's summary and transcript
    Show {
        id: i64,
        /// Only the transcript
        #[arg(long, conflicts_with = "summary")]
        transcript: bool,
        /// Only the summary
        #[arg(long)]
        summary: bool,
    },
    /// Search all transcripts
    Search {
        #[arg(required = true)]
        query: Vec<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Generate (or regenerate) a meeting's summary
    Summarize {
        id: i64,
        #[command(flatten)]
        choice: SummaryChoice,
    },
    /// Export a meeting (summary + transcript) as Markdown, text, SRT or Word
    Export {
        id: i64,
        /// md, txt, srt or docx (default: from --output's extension, else md)
        #[arg(long)]
        format: Option<String>,
        /// Output file [default: <date>-<title>.<ext> in the current directory]
        #[arg(long, short)]
        output: Option<PathBuf>,
    },
    /// Remove a meeting from the library (its WAV files are kept)
    Delete { id: i64 },
    /// Start or stop recording in the desktop app (launching it if needed).
    /// Bind this to a GNOME keyboard shortcut.
    Toggle,
    /// Bring the desktop app to the front (launching it if needed)
    Open,
    /// Ask a question about your meetings; answers cite the moments they come from
    Ask {
        /// The question, e.g. "Was haben wir über das Budget entschieden?"
        #[arg(required = true)]
        question: Vec<String>,
    },
    /// Rebuild the search index used by `ask` (only new/changed meetings unless --all)
    Reindex {
        #[arg(long)]
        all: bool,
    },
    /// List the models a summary provider offers (live, with thinking levels)
    Models {
        /// claude or deepseek
        #[arg(long, default_value = "claude")]
        provider: String,
    },
    /// Save an API key in the system keyring (for summaries)
    SetApiKey {
        /// claude (Anthropic) or deepseek
        #[arg(long, default_value = "claude")]
        provider: String,
    },
    /// List capture devices (for --mic-device / --system-device)
    Devices,
    /// Transcribe a 16 kHz mono WAV file
    Transcribe {
        /// WAV file to transcribe
        file: PathBuf,
        #[command(flatten)]
        models: ModelArgs,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SourceChoice {
    Mic,
    System,
    Both,
}

impl ModelArgs {
    fn diarize(&self) -> bool {
        self.diarize || mostalgia_core::config::Config::load().diarize
    }

    fn denoise(&self) -> bool {
        self.denoise || mostalgia_core::config::Config::load().denoise
    }

    fn final_pass_options(&self) -> finalize::FinalPassOptions {
        let vad = self.vad();
        finalize::FinalPassOptions {
            vad_model: vad.exists().then_some(vad),
            diarize: self.diarize(),
            denoise_mic: self.denoise(),
        }
    }

    fn whisper(&self) -> PathBuf {
        self.model.clone().unwrap_or_else(|| paths::model(paths::WHISPER_MODEL))
    }

    /// The VAD model if it exists (explicit path, else next to the Whisper
    /// model, else the usual locations).
    fn vad(&self) -> PathBuf {
        if let Some(p) = &self.vad_model {
            return p.clone();
        }
        let beside = self.whisper().with_file_name(paths::VAD_MODEL);
        if beside.exists() { beside } else { paths::model(paths::VAD_MODEL) }
    }

    fn load_engine(&self) -> Result<Engine> {
        let model = self.whisper();
        anyhow::ensure!(
            model.exists(),
            "model not found at {} — run scripts/download-model.sh first or pass --model",
            model.display()
        );
        eprintln!("[mostalgia] loading model {} …", model.display());
        let engine = Engine::load(&model, &self.language)?;
        match engine.gpu() {
            Some(gpu) => eprintln!("[mostalgia] using GPU: {gpu}"),
            None => eprintln!("[mostalgia] no discrete GPU found — using the default device"),
        }
        Ok(engine)
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Start {
            models,
            summary,
            out,
            source,
            no_mic,
            no_system,
            speakers,
            mic_device,
            system_device,
            no_final,
        } => {
            let source = match (no_mic, no_system) {
                (true, _) => SourceChoice::System,
                (_, true) => SourceChoice::Mic,
                _ => source,
            };
            let cfg = RecordConfig {
                vad_model: models.vad(),
                vad_threshold: mostalgia_core::config::Config::load().vad_threshold,
                out_dir: out.unwrap_or_else(paths::recordings_dir),
                mic: source != SourceChoice::System,
                system: source != SourceChoice::Mic,
                mic_device,
                system_device,
                diarize: models.diarize(),
                denoise: models.denoise(),
                interim: mostalgia_core::config::Config::load().interim_text,
            };
            start(&models, &summary, cfg, speakers, no_final)
        }
        Command::Import { dir, models, summary } => import(&dir, &models, &summary),
        Command::List { limit } => list(limit),
        Command::Show { id, transcript, summary } => show(id, !transcript, !summary),
        Command::Search { query, limit } => search(&query.join(" "), limit),
        Command::Summarize { id, choice } => {
            let store = open_store()?;
            summarize(&store, id, &choice.request()?, true)?;
            index_for_ask(id);
            Ok(())
        }
        Command::Export { id, format, output } => export(id, format.as_deref(), output),
        Command::Delete { id } => {
            anyhow::ensure!(open_store()?.delete(id)?, "no meeting #{id}");
            println!("Deleted meeting #{id} from the library (WAV files kept).");
            Ok(())
        }
        Command::Toggle => control("toggle", &["--start"]),
        Command::Open => control("show", &[]),
        Command::SetApiKey { provider } => set_api_key(&provider),
        Command::Models { provider } => list_models(&provider),
        Command::Ask { question } => ask(&question.join(" ")),
        Command::Reindex { all } => reindex(all),
        Command::Devices => list_devices(),
        Command::Transcribe { file, models } => transcribe_file(&file, &models),
    }
}

fn open_store() -> Result<Store> {
    Store::open(&paths::database())
}

fn start(models: &ModelArgs, sum: &SummaryArgs, cfg: RecordConfig, speakers: bool, no_final: bool) -> Result<()> {
    // Load the model first so a bad path fails before we start recording.
    let engine = models.load_engine()?;
    let (tx, rx) = crossbeam_channel::unbounded();
    let recorder = Recorder::start(cfg, engine, tx)?;

    // First Ctrl+C stops recording; a second one aborts the final pass.
    let stop = recorder.stop_handle();
    static PRESSES: AtomicUsize = AtomicUsize::new(0);
    ctrlc::set_handler(move || {
        if PRESSES.fetch_add(1, Ordering::SeqCst) == 0 {
            eprintln!("\n[mostalgia] stopping … (Ctrl+C again to skip the final pass)");
            stop.stop();
        } else {
            std::process::exit(130);
        }
    })
    .context("installing Ctrl+C handler")?;

    eprintln!("[mostalgia] recording to {} — press Ctrl+C to stop", recorder.dir().display());
    // MOSTALGIA_TIMING=1: log when each line becomes visible (latency checks).
    let debug = std::env::var_os("MOSTALGIA_TIMING").is_some();
    let t0 = std::time::Instant::now();
    if debug {
        let epoch = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs_f64();
        eprintln!("[timing] recording started at epoch {epoch:.3}");
    }
    // Interim text lives on one terminal line that gets overwritten.
    let tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
    let mut interim_shown = false;
    let clear_interim = |shown: &mut bool| {
        if *shown {
            print!("\r\x1b[2K");
            *shown = false;
        }
    };
    for event in rx {
        match event {
            Event::Interim { source, start_secs, text } => {
                if debug && !text.is_empty() {
                    eprintln!("[timing] interim at {:6.2}s (speech from {start_secs:.2}s): {text}", t0.elapsed().as_secs_f64());
                }
                if tty {
                    clear_interim(&mut interim_shown);
                    if !text.is_empty() {
                        let who = if speakers { format!("{}: ", source.speaker_label()) } else { String::new() };
                        let mut line = format!("[{}] {who}{text} …", format_ts(start_secs));
                        // Keep it on one line.
                        if line.chars().count() > 160 {
                            line = format!("…{}", line.chars().rev().take(158).collect::<Vec<_>>().into_iter().rev().collect::<String>());
                        }
                        print!("\x1b[2m{line}\x1b[0m");
                        let _ = std::io::Write::flush(&mut std::io::stdout());
                        interim_shown = true;
                    }
                }
            }
            Event::Line { source, speaker, start_secs, text, .. } => {
                clear_interim(&mut interim_shown);
                if debug {
                    eprintln!("[timing] final  at {:6.2}s (speech from {start_secs:.2}s) {}: {text}", t0.elapsed().as_secs_f64(), source.as_str());
                }
                let ts = format_ts(start_secs);
                if speakers {
                    let who = speaker.as_deref().unwrap_or(source.speaker_label());
                    println!("[{ts}] {who}: {text}");
                } else {
                    println!("[{ts}] {text}");
                }
            }
            Event::Chunk { audio_secs, millis, .. } => {
                eprintln!("[mostalgia] {audio_secs:.1} s chunk transcribed in {millis} ms");
            }
            Event::Warning(w) => eprintln!("[mostalgia] {w}"),
            Event::Level { .. } => {}
        }
    }
    let (recording, mut engine) = recorder.join()?;
    eprintln!("[mostalgia] tracks saved in {}", recording.dir.display());
    if no_final {
        return Ok(());
    }
    finish(&mut engine, models, sum, &recording)
}

/// Final pass → library → summary, shared by `start` and `import`.
fn finish(engine: &mut Engine, models: &ModelArgs, sum: &SummaryArgs, rec: &Recording) -> Result<()> {
    eprintln!("[mostalgia] final pass over {:.0} s of audio …", rec.duration_secs);
    let t = std::time::Instant::now();
    let segments = finalize::final_pass(engine, &rec.tracks, &models.final_pass_options())?;
    eprintln!(
        "[mostalgia] final transcript: {} lines in {:.1} s",
        segments.len(),
        t.elapsed().as_secs_f64()
    );

    let mut store = open_store()?;
    let title = format!("Besprechung {}", rec.started_at.format("%d.%m.%Y %H:%M"));
    let id = store.insert_meeting(
        &NewMeeting {
            title: &title,
            started_at: rec.started_at,
            duration_secs: rec.duration_secs,
            language: &models.language,
            dir: &rec.dir,
        },
        &segments,
    )?;
    println!("\n=== Meeting #{id}: {title} ===");
    for s in &segments {
        let who = s.speaker.as_deref().unwrap_or(s.source.speaker_label());
        println!("[{}] {who}: {}", format_ts(s.start_secs), s.text);
    }

    if !sum.no_summary && !segments.is_empty() {
        if let Err(e) = sum.choice.request().and_then(|req| summarize(&store, id, &req, false)) {
            eprintln!("[mostalgia] summary skipped: {e:#}");
        }
    }
    index_for_ask(id);
    println!("\nSaved as meeting #{id} — `mostalgia show {id}` to view it again.");
    Ok(())
}

fn summarize(store: &Store, id: i64, req: &summary::SummaryRequest, required: bool) -> Result<()> {
    let meeting = store.get(id)?.with_context(|| format!("no meeting #{id}"))?;
    let Some(key) = summary::api_key(req.provider)? else {
        let msg = format!(
            "no {} API key — run `mostalgia set-api-key --provider {}`",
            req.provider.name(),
            req.provider.id()
        );
        if required {
            anyhow::bail!(msg);
        }
        eprintln!("[mostalgia] {msg}, then `mostalgia summarize {id}`");
        return Ok(());
    };
    eprintln!(
        "[mostalgia] summarizing with {} {}{} …",
        req.provider.name(),
        req.model,
        req.effort.as_deref().map(|e| format!(" (effort {e})")).unwrap_or_default()
    );
    let t = std::time::Instant::now();
    let s = summary::summarize(req, &key, &meeting)?;
    eprintln!("[mostalgia] summary took {:.1} s", t.elapsed().as_secs_f64());
    store.save_summary(id, s.provider.id(), &s.model, s.effort.as_deref(), &s.markdown)?;
    if let Some(title) = &s.title {
        store.set_title(id, title)?;
    }
    println!("\n{}", s.markdown);
    Ok(())
}

fn import(dir: &Path, models: &ModelArgs, sum: &SummaryArgs) -> Result<()> {
    let tracks: Vec<(Source, PathBuf)> = [Source::Mic, Source::System]
        .into_iter()
        .map(|s| (s, dir.join(s.file_name())))
        .filter(|(_, p)| p.exists())
        .collect();
    anyhow::ensure!(!tracks.is_empty(), "no mic.wav or system.wav in {}", dir.display());

    // session-<unix ts> folders carry their start time; else use the file time.
    let started_at = dir
        .file_name()
        .and_then(|n| n.to_str()?.strip_prefix("session-")?.parse::<i64>().ok())
        .and_then(|ts| Local.timestamp_opt(ts, 0).single())
        .or_else(|| {
            let modified = std::fs::metadata(&tracks[0].1).ok()?.modified().ok()?;
            Some(DateTime::<Local>::from(modified))
        })
        .unwrap_or_else(Local::now);
    let duration_secs = tracks
        .iter()
        .filter_map(|(_, p)| hound::WavReader::open(p).ok())
        .map(|r| r.duration() as f64 / mostalgia_core::SAMPLE_RATE as f64)
        .fold(0.0, f64::max);
    let rec = Recording {
        dir: std::fs::canonicalize(dir)?,
        started_at,
        duration_secs,
        tracks,
    };
    let mut engine = models.load_engine()?;
    finish(&mut engine, models, sum, &rec)
}

fn list(limit: usize) -> Result<()> {
    let meetings = open_store()?.list(limit)?;
    if meetings.is_empty() {
        println!("No meetings yet — record one with `mostalgia start`.");
    }
    for m in meetings {
        println!(
            "#{:<4} {}  {:>3} min  {:>4} lines  {}  {}",
            m.id,
            m.started_at.format("%d.%m.%Y %H:%M"),
            (m.duration_secs / 60.0).round(),
            m.segment_count,
            if m.has_summary { "✓" } else { " " },
            m.title
        );
    }
    Ok(())
}

fn show(id: i64, with_summary: bool, with_transcript: bool) -> Result<()> {
    let m = open_store()?.get(id)?.with_context(|| format!("no meeting #{id}"))?;
    println!(
        "Meeting #{} — {}\n{} · {} min · {}\n",
        m.id,
        m.title,
        m.started_at.format("%d.%m.%Y %H:%M"),
        (m.duration_secs / 60.0).round(),
        m.dir.display()
    );
    if with_summary {
        match &m.summary {
            Some(s) => println!(
                "{}\n\n(summary by {} {}{}, {})\n",
                s.markdown,
                summary::Provider::parse(&s.provider).map_or(s.provider.as_str(), |p| p.name()),
                s.model,
                s.effort.as_deref().map(|e| format!(", effort {e}")).unwrap_or_default(),
                s.created_at.format("%d.%m.%Y %H:%M")
            ),
            None => println!("(no summary yet — `mostalgia summarize {id}`)\n"),
        }
    }
    if with_transcript {
        println!("Transcript:");
        for s in &m.segments {
            println!("[{}] {}: {}", format_ts(s.start_secs), s.speaker_label(), s.text);
        }
    }
    Ok(())
}

fn search(query: &str, limit: usize) -> Result<()> {
    let hits = open_store()?.search(query, limit)?;
    if hits.is_empty() {
        println!("No matches for “{query}”.");
    }
    for h in hits {
        println!(
            "#{:<4} {}  [{}] {}: {}   — {}",
            h.meeting_id,
            h.started_at.format("%d.%m.%Y"),
            format_ts(h.start_secs),
            h.source.speaker_label(),
            h.snippet,
            h.title
        );
    }
    Ok(())
}

fn export(id: i64, format: Option<&str>, output: Option<PathBuf>) -> Result<()> {
    use mostalgia_core::export::{self, Format};
    let m = open_store()?.get(id)?.with_context(|| format!("no meeting #{id}"))?;
    let format = match format {
        Some(f) => Format::from_extension(f).with_context(|| format!("unknown format {f:?} (md, txt, srt, docx)"))?,
        None => output
            .as_ref()
            .and_then(|p| p.extension()?.to_str())
            .and_then(Format::from_extension)
            .unwrap_or(Format::Markdown),
    };
    let path = output.unwrap_or_else(|| PathBuf::from(export::suggested_name(&m, format)));
    export::write(&m, format, &path)?;
    println!("Exported meeting #{id} to {}", path.display());
    Ok(())
}

/// Sends a command to the running app, or launches it.
fn control(cmd: &str, launch_args: &[&str]) -> Result<()> {
    use std::io::{Read, Write};
    let socket = paths::control_socket();
    if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&socket) {
        stream.write_all(format!("{cmd}\n").as_bytes())?;
        let mut reply = String::new();
        let _ = stream.read_to_string(&mut reply);
        anyhow::ensure!(reply.trim() == "ok", "mostalgia-app: {}", reply.trim());
        return Ok(());
    }
    // Not running: start the app (next to this binary, else from PATH).
    let beside = std::env::current_exe()?.with_file_name("mostalgia-app");
    let app = if beside.exists() { beside } else { PathBuf::from("mostalgia-app") };
    std::process::Command::new(&app)
        .args(launch_args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("starting {}", app.display()))?;
    Ok(())
}

/// Updates the `ask` index for a meeting; failures only warn.
fn index_for_ask(id: i64) {
    let t = std::time::Instant::now();
    match mostalgia_core::ask::index_meeting(id) {
        Ok(n) => eprintln!("[mostalgia] search index: {n} passages in {:.2} s", t.elapsed().as_secs_f64()),
        Err(e) => eprintln!("[mostalgia] search index not updated: {e:#}"),
    }
}

fn reindex(all: bool) -> Result<()> {
    use mostalgia_core::ask;
    if !ask::embed::available() {
        eprintln!("[mostalgia] embedding model missing (scripts/download-model.sh) — keyword search only");
    }
    let t = std::time::Instant::now();
    let n = if all {
        let ids: Vec<i64> = open_store()?.list(100_000)?.into_iter().map(|m| m.id).collect();
        for id in &ids {
            ask::index_meeting(*id)?;
        }
        ids.len()
    } else {
        ask::index_pending()?
    };
    println!("Indexed {n} meeting(s) in {:.2} s.", t.elapsed().as_secs_f64());
    Ok(())
}

fn ask(question: &str) -> Result<()> {
    use mostalgia_core::ask::{self, AskEvent, Piece};
    // Bring the index up to date first (cheap when nothing changed).
    if let Err(e) = ask::index_pending() {
        eprintln!("[mostalgia] search index: {e:#}");
    }
    let (tx, rx) = crossbeam_channel::unbounded();
    let q = question.to_string();
    let worker = std::thread::spawn(move || ask::ask(&q, &[], &tx));
    // The answer is printed whole at the end, with readable citations
    // instead of the raw markers it streams with.
    let mut writing = false;
    for ev in rx {
        match ev {
            AskEvent::Status(s) => eprintln!("[mostalgia] {s}"),
            AskEvent::TextDelta(_) => {
                if !writing {
                    eprintln!("[mostalgia] Antwort wird geschrieben …");
                    writing = true;
                }
            }
            AskEvent::ClearText => writing = false,
            AskEvent::Done { answer, citations } => {
                println!();
                let text: String = ask::parse_answer(&answer)
                    .into_iter()
                    .map(|p| match p {
                        Piece::Text(t) => t,
                        Piece::Cite(c) => format!("[{}]", c.label),
                    })
                    .collect();
                println!("{text}");
                if !citations.is_empty() {
                    println!("\nQuellen:");
                    let mut seen = Vec::new();
                    for c in citations {
                        if !seen.contains(&c.label) {
                            println!("  [{}]  →  mostalgia show {}", c.label, c.meeting_id);
                            seen.push(c.label);
                        }
                    }
                }
            }
            AskEvent::Error(e) => {
                let _ = worker.join();
                anyhow::bail!(e);
            }
        }
    }
    let _ = worker.join();
    Ok(())
}

fn list_models(provider: &str) -> Result<()> {
    let provider = summary::Provider::parse(provider).with_context(|| format!("unknown provider {provider:?}"))?;
    let models = match summary::api_key(provider)? {
        Some(key) => summary::list_models(provider, &key)?,
        None => {
            eprintln!("(no {} API key — showing the built-in list)", provider.name());
            summary::fallback_models(provider)
        }
    };
    for m in models {
        let efforts = if m.efforts.is_empty() { "—".to_string() } else { m.efforts.join(", ") };
        println!("{:<28} {:<26} thinking: {efforts}", m.id, m.display_name);
    }
    Ok(())
}

fn set_api_key(provider: &str) -> Result<()> {
    let provider = summary::Provider::parse(provider).with_context(|| format!("unknown provider {provider:?}"))?;
    let key = rpassword::prompt_password(format!("{} API key: ", provider.name())).context("reading the key")?;
    anyhow::ensure!(!key.trim().is_empty(), "no key entered");
    summary::save_api_key(provider, &key)?;
    println!("Saved in the system keyring (an environment variable, if set, still takes precedence).");
    Ok(())
}

fn list_devices() -> Result<()> {
    let sources = audio::list_sources()?;
    for (title, monitors) in [("Microphones (--mic-device)", false), ("System audio (--system-device)", true)] {
        println!("{title}:");
        for s in sources.iter().filter(|s| s.is_monitor == monitors) {
            let mark = if s.is_default { "*" } else { " " };
            let unplugged = if s.available { "" } else { "  (nicht angeschlossen)" };
            println!("  {mark} {}{unplugged}\n      {}", s.label, s.name);
        }
        println!();
    }
    println!("* = used by default");
    Ok(())
}

fn transcribe_file(file: &Path, models: &ModelArgs) -> Result<()> {
    let samples = finalize::read_wav(file)?;
    let mut engine = models.load_engine()?;
    let t = std::time::Instant::now();
    // Treated as the system track so --diarize applies; --denoise as the mic.
    let source = if models.denoise() { Source::Mic } else { Source::System };
    let segments = finalize::final_pass(&mut engine, &[(source, file.to_path_buf())], &models.final_pass_options())?;
    let elapsed = t.elapsed().as_secs_f64();
    for seg in segments {
        match &seg.speaker {
            Some(who) => println!("[{}] {who}: {}", format_ts(seg.start_secs), seg.text),
            None => println!("[{}] {}", format_ts(seg.start_secs), seg.text),
        }
    }
    let audio_secs = samples.len() as f64 / mostalgia_core::SAMPLE_RATE as f64;
    eprintln!(
        "[mostalgia] {audio_secs:.1} s of audio transcribed in {elapsed:.2} s ({:.1}x real time)",
        audio_secs / elapsed
    );
    Ok(())
}
