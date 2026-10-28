use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, TimeZone};
use clap::{Parser, Subcommand, ValueEnum};
use mostalgia_core::chunker::format_ts;
use mostalgia_core::i18n::{self, tr, trf};
use mostalgia_core::recorder::{Event, RecordConfig, Recorder, Recording};
use mostalgia_core::config::{Config, SttChoice};
use mostalgia_core::connections::{self, Connection};
use mostalgia_core::store::Store;
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
    /// Language to transcribe: a Whisper language code ("en", "de", "fr", …)
    /// or "auto" [default: from the app's settings]
    #[arg(long)]
    language: Option<String>,
    /// Tell several speakers on the system-audio side apart ("Speaker 1",
    /// "Speaker 2", …). Also on when enabled in the app's settings.
    #[arg(long)]
    diarize: bool,
    /// People in the call, counting you (2–4, or 5 for "5 or more"): helps
    /// telling the voices on the system-audio side apart [default: automatic]
    #[arg(long, value_parser = clap::value_parser!(u8).range(2..=5))]
    people: Option<u8>,
    /// Suppress steady background noise (fans, hum) on the mic before
    /// transcribing; the recorded WAV stays untouched
    #[arg(long)]
    denoise: bool,
    /// Speech-to-text engine: "local" (Whisper on this machine) or
    /// CONNECTION/MODEL, e.g. openai/gpt-4o-transcribe or groq/whisper-large-v3-turbo
    /// [default: from the app's settings]. With `start` this is the live engine.
    #[arg(long)]
    engine: Option<String>,
    /// Engine for the final pass after `start` (same format as --engine)
    /// [default: from the app's settings]
    #[arg(long)]
    final_engine: Option<String>,
}

#[derive(clap::Args)]
struct SummaryArgs {
    /// Don't generate a summary after the final pass
    #[arg(long)]
    no_summary: bool,
    #[command(flatten)]
    choice: SummaryChoice,
}

/// Which connection/model/thinking level summarizes; defaults from config.json.
#[derive(clap::Args)]
struct SummaryChoice {
    /// AI connection id, e.g. claude, openai, deepseek, ollama (see `mostalgia connections`)
    #[arg(long)]
    provider: Option<String>,
    /// Model id, e.g. claude-opus-5-5, gpt-5, deepseek-chat, llama3.1
    #[arg(long)]
    summary_model: Option<String>,
    /// Thinking level: low, medium, high, xhigh or max (where the model supports it)
    #[arg(long)]
    effort: Option<String>,
}

impl SummaryChoice {
    fn request(&self) -> Result<summary::SummaryRequest> {
        let config = Config::load();
        let mut req = match &self.provider {
            Some(id) => {
                let connection = connection(&config, id)?;
                let model = if config.summary_provider == connection.id { config.summary_model.clone() } else { connection.preset().default_model.to_string() };
                summary::SummaryRequest { connection, model, language: config.summary_language_code(), effort: None }
            }
            None => config.summary_request()?,
        };
        if let Some(m) = &self.summary_model {
            req.model = m.clone();
        }
        anyhow::ensure!(
            !req.model.is_empty(),
            "{}",
            trf("Kein Modell für {} gewählt — --summary-model angeben", &[&req.connection.name])
        );
        let effort = match &self.effort {
            Some(e) => {
                anyhow::ensure!(summary::EFFORTS.contains(&e.as_str()), "{}", trf("Unbekannte Denktiefe {} (low, medium, high, xhigh, max)", &[&format!("{e:?}")]));
                e.clone()
            }
            None => config.summary_effort.clone(),
        };
        req.effort = mostalgia_core::config::effort_for(&req.connection, &req.model, &effort);
        Ok(req)
    }
}

/// A configured connection by id, or an error listing the ids.
fn connection(config: &Config, id: &str) -> Result<Connection> {
    config.connection(id).cloned().with_context(|| {
        let ids: Vec<&str> = config.connections.iter().map(|c| c.id.as_str()).collect();
        trf("Unbekannte Verbindung {} (vorhanden: {})", &[&format!("{id:?}"), &ids.join(", ")])
    })
}

/// "local" or CONNECTION/MODEL.
fn parse_engine(config: &Config, s: &str) -> Result<SttChoice> {
    if s.eq_ignore_ascii_case("local") || s.eq_ignore_ascii_case("lokal") {
        return Ok(SttChoice::local());
    }
    let (id, model) = s
        .split_once('/')
        .with_context(|| trf("Ungültige Engine {}: \"local\" oder VERBINDUNG/MODELL, z. B. openai/gpt-4o-transcribe", &[&format!("{s:?}")]))?;
    connection(config, id)?;
    Ok(SttChoice { connection: id.into(), model: model.into() })
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
    /// Add a meeting you didn't record from its transcript as text: plain
    /// text, "Name: text" lines, timestamps, Teams/Zoom/Meet exports, WebVTT
    /// or SRT. Then it is summarized and gets notes like a recording.
    ImportText {
        /// The text file (.txt, .md, .vtt, .srt), or - to read standard input
        file: PathBuf,
        /// Title [default: from the summary]
        #[arg(long)]
        title: Option<String>,
        /// When it took place, e.g. "2026-10-27 14:00" or "27.10.2026 14:00" [default: now]
        #[arg(long)]
        date: Option<String>,
        /// How long it took, e.g. 45 (minutes), 45m, 1h30 or 1:30:00
        /// [default: from the timestamps, else estimated from the length]
        #[arg(long)]
        duration: Option<String>,
        /// Language code, e.g. en or de [default: detected from the text]
        #[arg(long)]
        language: Option<String>,
        /// What the notes should focus on [default: the one in the settings]
        #[arg(long)]
        focus: Option<String>,
        /// Note style: "default" or "modern" [default: the one in the settings]
        #[arg(long)]
        style: Option<String>,
        /// Don't write notes
        #[arg(long)]
        no_notes: bool,
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
    /// Write a meeting's notes again from its transcript with Mori's notes
    /// model (pinned notes stay; the replaced ones can be restored in the app)
    Notes {
        id: i64,
        /// What to focus on, e.g. "action items for me, pricing"
        /// [default: the meeting's last focus, else the one in the settings]
        #[arg(long)]
        focus: Option<String>,
        /// Note style: "default" (decisions, action items, questions,
        /// figures) or "modern" (also key points; more notes)
        /// [default: the one in the settings]
        #[arg(long)]
        style: Option<String>,
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
    /// Delete a meeting: transcript, summary, search index and its audio files
    Delete {
        id: i64,
        /// Keep the session's WAV files
        #[arg(long)]
        keep_audio: bool,
    },
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
    /// The documents drive: add files (md, txt, pdf, docx, odt, html) that
    /// answers can search and cite, list, search or remove them
    Docs {
        #[command(subcommand)]
        action: Option<DocsAction>,
    },
    /// Try the live answer helper: suggest an answer to a question from
    /// past meetings and/or documents, as during a recording
    Suggest {
        /// Where to look: meetings, documents or both [default: from the settings]
        #[arg(long)]
        sources: Option<String>,
        #[arg(required = true)]
        question: Vec<String>,
    },
    /// Rebuild the search index used by `ask` (only new/changed meetings unless --all)
    Reindex {
        #[arg(long)]
        all: bool,
    },
    /// List the models an AI connection offers (live, with thinking levels)
    Models {
        /// Connection id, e.g. claude, openai, deepseek, ollama
        #[arg(long, default_value = "claude")]
        provider: String,
    },
    /// Save an API key in the system keyring for an AI connection
    SetApiKey {
        /// Connection id, e.g. claude, openai, deepseek, groq
        #[arg(long, default_value = "claude")]
        provider: String,
    },
    /// AI connections: list them with their status, add, test or remove one
    Connections {
        #[command(subcommand)]
        action: Option<ConnectionsAction>,
    },
    /// Transcribe a saved meeting again from its audio, e.g. after a cloud
    /// engine failed (replaces the transcript; speaker names are reset)
    Retranscribe {
        id: i64,
        #[command(flatten)]
        models: ModelArgs,
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

#[derive(Subcommand)]
enum DocsAction {
    /// List the documents (default)
    List,
    /// Add files and index them
    Add {
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// Search the documents
    Search {
        #[arg(required = true)]
        query: Vec<String>,
        #[arg(long, default_value_t = 8)]
        limit: usize,
    },
    /// Read a document's extracted text again and re-embed it
    Reindex { id: i64 },
    /// Remove a document (and its copy in the data directory)
    Remove { id: i64 },
}

#[derive(Subcommand)]
enum ConnectionsAction {
    /// List connections and check each one (default)
    List,
    /// Add a connection from a preset: claude, openai, deepseek, ollama,
    /// litellm, lmstudio, vllm, openrouter, groq, together or custom
    Add {
        preset: String,
        /// Display name [default: the preset's name]
        #[arg(long)]
        name: Option<String>,
        /// Base URL including /v1 [default: the preset's URL]
        #[arg(long)]
        url: Option<String>,
        /// Model ids to offer when the server has no model list (repeatable)
        #[arg(long = "model")]
        models: Vec<String>,
        /// Ask for an API key and store it in the keyring
        #[arg(long)]
        key: bool,
    },
    /// Check a connection (URL, key, model list)
    Test { id: String },
    /// Remove a connection and its stored API key
    Remove { id: String },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SourceChoice {
    Mic,
    System,
    Both,
}

impl ModelArgs {
    fn language(&self) -> String {
        self.language.clone().unwrap_or_else(|| Config::load().language)
    }

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
            people: self.people.unwrap_or(0),
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

    /// The live engine (`start`), or the only one for one-shot commands.
    fn live_choice(&self) -> Result<SttChoice> {
        let config = Config::load();
        match &self.engine {
            Some(e) => parse_engine(&config, e),
            None => Ok(config.live_engine.clone()),
        }
    }

    /// The final-pass engine; for import/transcribe/retranscribe --engine works too.
    fn final_choice(&self) -> Result<SttChoice> {
        let config = Config::load();
        match self.final_engine.as_ref().or(self.engine.as_ref()) {
            Some(e) => parse_engine(&config, e),
            None => Ok(config.final_engine.clone()),
        }
    }

    fn build_engine(&self, choice: &SttChoice) -> Result<Engine> {
        if choice.is_local() {
            return self.load_engine();
        }
        let config = Config::load();
        let conn = connection(&config, &choice.connection)?;
        let mut engine = Engine::cloud(&conn, &choice.model, &self.language())?;
        engine.set_meeting_languages(&config.meeting_languages);
        let privacy = tr("Audio wird zur Transkription an diesen Anbieter gesendet");
        eprintln!("[mostalgia] {}", trf("Transkription: {} — {}", &[&engine.label(), &privacy]));
        Ok(engine)
    }

    fn load_engine(&self) -> Result<Engine> {
        let model = self.whisper();
        anyhow::ensure!(
            model.exists(),
            "{}",
            trf("Modell nicht gefunden: {} — zuerst scripts/download-model.sh ausführen oder --model angeben", &[&model.display()])
        );
        eprintln!("[mostalgia] {}", trf("Lade Modell {} …", &[&model.display()]));
        let mut engine = Engine::load(&model, &self.language())?;
        engine.set_meeting_languages(&Config::load().meeting_languages);
        match engine.gpu() {
            Some(gpu) => eprintln!("[mostalgia] {}", trf("Nutze GPU: {}", &[&gpu])),
            None => eprintln!("[mostalgia] {}", tr("Keine eigene Grafikkarte gefunden — nutze das Standardgerät")),
        }
        Ok(engine)
    }
}

fn main() -> Result<()> {
    mostalgia_core::i18n::set_lang(mostalgia_core::i18n::resolve(&mostalgia_core::config::Config::load().ui_language));
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
        Command::ImportText { file, title, date, duration, language, focus, style, no_notes, summary } => {
            import_text(&file, TextImportArgs { title, date, duration, language, focus, style, no_notes }, &summary)
        }
        Command::List { limit } => list(limit),
        Command::Show { id, transcript, summary } => show(id, !transcript, !summary),
        Command::Search { query, limit } => search(&query.join(" "), limit),
        Command::Summarize { id, choice } => {
            let store = open_store()?;
            summarize(&store, id, &choice.request()?, true, false)?;
            index_for_ask(id);
            Ok(())
        }
        Command::Notes { id, focus, style } => regenerate_notes(id, focus, style),
        Command::Export { id, format, output } => export(id, format.as_deref(), output),
        Command::Delete { id, keep_audio } => {
            anyhow::ensure!(open_store()?.delete_meeting(id, !keep_audio)?, "{}", trf("Keine Besprechung #{}", &[&id]));
            if keep_audio {
                println!("{}", trf("Besprechung #{} aus der Bibliothek entfernt (WAV-Dateien bleiben).", &[&id]));
            } else {
                println!("{}", trf("Besprechung #{} samt Audiodateien gelöscht.", &[&id]));
            }
            Ok(())
        }
        Command::Toggle => control("toggle", &["--start"]),
        Command::Open => control("show", &[]),
        Command::SetApiKey { provider } => set_api_key(&provider),
        Command::Models { provider } => list_models(&provider),
        Command::Connections { action } => connections_cmd(action),
        Command::Retranscribe { id, models } => retranscribe(id, &models),
        Command::Ask { question } => ask(&question.join(" ")),
        Command::Reindex { all } => reindex(all),
        Command::Docs { action } => docs_cmd(action.unwrap_or(DocsAction::List)),
        Command::Suggest { sources, question } => suggest(&question.join(" "), sources.as_deref()),
        Command::Devices => list_devices(),
        Command::Transcribe { file, models } => transcribe_file(&file, &models),
    }
}

fn regenerate_notes(id: i64, focus: Option<String>, style: Option<String>) -> Result<()> {
    use mostalgia_core::companion;
    let focus = match focus {
        Some(f) => f,
        None => open_store()?.notes_focus(id)?.unwrap_or_else(|| Config::load().companion_notes_focus),
    };
    let config = Config::load();
    let style = match style {
        Some(s) => companion::NoteStyle::parse(&s).with_context(|| trf("Unbekannter Notizstil: {} (default oder modern)", &[&s]))?,
        None => config.notes_style(),
    };
    let req = config.notes_request()?;
    eprintln!("[mostalgia] {}", trf("{} liest die Besprechung mit {} · {} …", &[&config.companion_character().name(), &req.connection.name, &req.model]));
    let done = companion::regenerate_notes(id, style, &focus, &mut |part, parts| {
        if parts > 1 {
            eprintln!("[mostalgia] {}", trf("Teil {} von {}", &[&part, &parts]));
        }
    })?;
    for n in open_store()?.notes(id)? {
        let kind = companion::PointKind::parse(&n.kind).map_or(n.kind.clone(), |k| k.label().to_string());
        println!("[{}] {}{}: {}", format_ts(n.at_secs), kind, if n.pinned { " *" } else { "" }, n.text);
    }
    eprintln!(
        "[mostalgia] {}",
        trf("{} neue Notizen, {} angeheftete behalten, {} Wiederholungen weggelassen ({} · {})", &[&done.added, &done.kept, &done.repeats, &done.provider, &done.model])
    );
    Ok(())
}

fn open_store() -> Result<Store> {
    Store::open(&paths::database())
}

fn start(models: &ModelArgs, sum: &SummaryArgs, cfg: RecordConfig, speakers: bool, no_final: bool) -> Result<()> {
    // Set up the engines first so a bad model path or a missing key fails
    // before anything is recorded.
    let (live_choice, final_choice) = (models.live_choice()?, models.final_choice()?);
    let engine = models.build_engine(&live_choice)?;
    let mut final_engine = if no_final || final_choice == live_choice { None } else { Some(models.build_engine(&final_choice)?) };
    let (tx, rx) = crossbeam_channel::unbounded();
    let recorder = Recorder::start(cfg, engine, tx)?;

    // First Ctrl+C stops recording; a second one aborts the final pass.
    let stop = recorder.stop_handle();
    static PRESSES: AtomicUsize = AtomicUsize::new(0);
    ctrlc::set_handler(move || {
        if PRESSES.fetch_add(1, Ordering::SeqCst) == 0 {
            eprintln!("\n[mostalgia] {}", tr("Beende … (nochmal Strg+C überspringt den letzten Durchgang)"));
            stop.stop();
        } else {
            std::process::exit(130);
        }
    })
    .context("installing Ctrl+C handler")?;

    eprintln!("[mostalgia] {}", trf("Nehme auf nach {} — Strg+C beendet", &[&recorder.dir().display()]));
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
                    println!("[{ts}] {}: {text}", i18n::speaker(who));
                } else {
                    println!("[{ts}] {text}");
                }
            }
            Event::Chunk { audio_secs, millis, .. } => {
                eprintln!("[mostalgia] {}", trf("{} s Abschnitt in {} ms transkribiert", &[&format!("{audio_secs:.1}"), &millis]));
            }
            Event::Warning(w) => eprintln!("[mostalgia] {w}"),
            Event::EngineError { engine, message } => {
                clear_interim(&mut interim_shown);
                eprintln!("[mostalgia] {}", trf("Transkription mit {} fehlgeschlagen (Aufnahme läuft weiter): {}", &[&engine, &message]));
            }
            Event::Stalled { secs: 0 } => eprintln!("[mostalgia] {}", tr("Live-Transkription läuft wieder")),
            Event::Stalled { secs } if secs % 10 == 0 || secs == mostalgia_core::recorder::STALL_SECS => {
                eprintln!("[mostalgia] {}", trf("Live-Transkription hängt seit {} s (Aufnahme läuft weiter)", &[&secs]))
            }
            Event::Stalled { .. } => {}
            Event::Level { .. } => {}
            Event::Language { source, status } => {
                let how = if status.manual { tr("gewählt") } else if status.locked { tr("fest") } else { tr("vorläufig") };
                eprintln!("[mostalgia] {}", trf("Sprache {}: {} ({})", &[&source.as_str(), &mostalgia_core::language::native_name(&status.language), &how]));
            }
        }
    }
    let (recording, mut engine) = recorder.join()?;
    eprintln!("[mostalgia] {}", trf("Spuren gespeichert in {}", &[&recording.dir.display()]));
    if no_final {
        return Ok(());
    }
    finish(final_engine.as_mut().unwrap_or(&mut engine), models, sum, &recording)
}

/// Final pass → library → summary, shared by `start` and `import`. If the
/// final pass fails the meeting is still saved (with the live transcript)
/// and the error says how to transcribe it again.
fn finish(engine: &mut Engine, models: &ModelArgs, sum: &SummaryArgs, rec: &Recording) -> Result<()> {
    eprintln!(
        "[mostalgia] {}",
        trf("Letzter Durchgang über {} s Audio mit {} …", &[&format!("{:.0}", rec.duration_secs), &engine.label()])
    );
    let t = std::time::Instant::now();
    let saved = finalize::save_recording(rec, engine, &models.final_pass_options(), &models.language(), &[])?;
    let id = saved.meeting_id;
    let store = open_store()?;
    let meeting = store.get(id)?.with_context(|| trf("Keine Besprechung #{}", &[&id]))?;
    eprintln!(
        "[mostalgia] {}",
        trf("Fertiges Transkript: {} Zeilen in {} s", &[&meeting.segments.len(), &format!("{:.1}", t.elapsed().as_secs_f64())])
    );
    println!("\n=== {} ===", trf("Besprechung #{}: {}", &[&id, &meeting.title]));
    for s in &meeting.segments {
        println!("[{}] {}: {}", format_ts(s.start_secs), i18n::speaker(s.speaker_label()), s.text);
    }
    if let Some(error) = &saved.error {
        eprintln!("\n[mostalgia] {error}");
        eprintln!(
            "[mostalgia] {}",
            trf("Die Aufnahme ist gespeichert. Erneut transkribieren: `mostalgia retranscribe {} --engine local`", &[&id])
        );
    }

    if !sum.no_summary && !meeting.segments.is_empty() {
        if let Err(e) = sum.choice.request().and_then(|req| summarize(&store, id, &req, false, false)) {
            eprintln!("[mostalgia] {}", trf("Zusammenfassung übersprungen: {}", &[&format!("{e:#}")]));
        }
    }
    index_for_ask(id);
    println!("\n{}", trf("Gespeichert als Besprechung #{} — `mostalgia show {}` zeigt sie wieder an.", &[&id, &id]));
    Ok(())
}

fn retranscribe(id: i64, models: &ModelArgs) -> Result<()> {
    let mut engine = models.build_engine(&models.final_choice()?)?;
    let t = std::time::Instant::now();
    let lines = finalize::retranscribe(id, &mut engine, &models.final_pass_options())?;
    println!(
        "{}",
        trf("Besprechung #{} neu transkribiert mit {}: {} Zeilen in {} s", &[&id, &engine.label(), &lines, &format!("{:.1}", t.elapsed().as_secs_f64())])
    );
    Ok(())
}

/// `keep_title`: don't rename the meeting after the summary's heading.
fn summarize(store: &Store, id: i64, req: &summary::SummaryRequest, required: bool, keep_title: bool) -> Result<()> {
    let meeting = store.get(id)?.with_context(|| trf("Keine Besprechung #{}", &[&id]))?;
    if !req.connection.is_configured() {
        let msg = trf(
            "Kein {}-API-Schlüssel — `mostalgia set-api-key --provider {}` ausführen",
            &[&req.connection.name, &req.connection.id],
        );
        if required {
            anyhow::bail!(msg);
        }
        eprintln!("[mostalgia] {}", trf("{}, danach `mostalgia summarize {}`", &[&msg, &id]));
        return Ok(());
    }
    eprintln!(
        "[mostalgia] {}",
        trf(
            "Fasse zusammen mit {} {}{} …",
            &[
                &req.connection.name,
                &req.model,
                &req.effort.as_deref().map(|e| trf(" (Denktiefe {})", &[&e])).unwrap_or_default(),
            ],
        )
    );
    let t = std::time::Instant::now();
    let s = summary::summarize(req, &meeting)?;
    eprintln!("[mostalgia] {}", trf("Zusammenfassung dauerte {} s", &[&format!("{:.1}", t.elapsed().as_secs_f64())]));
    store.save_summary(id, &s.provider, &s.model, s.effort.as_deref(), &s.markdown)?;
    if let Some(title) = s.title.as_ref().filter(|_| !keep_title) {
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
    anyhow::ensure!(!tracks.is_empty(), "{}", trf("Kein mic.wav oder system.wav in {}", &[&dir.display()]));

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
        live_lines: Vec::new(),
        live_errors: 0,
        languages: [None, None],
    };
    let mut engine = models.build_engine(&models.final_choice()?)?;
    finish(&mut engine, models, sum, &rec)
}

struct TextImportArgs {
    title: Option<String>,
    date: Option<String>,
    duration: Option<String>,
    language: Option<String>,
    focus: Option<String>,
    style: Option<String>,
    no_notes: bool,
}

/// `import-text`: the text → a meeting → search index, summary, notes.
fn import_text(file: &Path, args: TextImportArgs, sum: &SummaryArgs) -> Result<()> {
    use mostalgia_core::{companion, text_import};
    let bytes = if file.as_os_str() == "-" {
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)?;
        buf
    } else {
        std::fs::read(file).with_context(|| trf("{} lässt sich nicht lesen", &[&file.display()]))?
    };
    let text = text_import::decode(&bytes)?;
    let started_at = match &args.date {
        Some(d) => text_import::parse_date_time(d, Local::now())
            .with_context(|| trf("Unbekanntes Datum {} (z. B. \"2026-10-27 14:00\")", &[&format!("{d:?}")]))?,
        None => Local::now(),
    };
    let duration_secs = match &args.duration {
        Some(d) => Some(text_import::parse_duration(d).with_context(|| trf("Unbekannte Dauer {} (z. B. 45, 1h30 oder 1:30:00)", &[&format!("{d:?}")]))?),
        None => None,
    };
    // Check the note style before anything is saved.
    let config = Config::load();
    let style = match &args.style {
        Some(s) => companion::NoteStyle::parse(s).with_context(|| trf("Unbekannter Notizstil: {} (default oder modern)", &[s]))?,
        None => config.notes_style(),
    };
    let mut store = open_store()?;
    let done = text_import::import(
        &mut store,
        &text_import::Request { text: &text, title: args.title.as_deref(), started_at, duration_secs, language: args.language.as_deref() },
    )?;
    let id = done.meeting_id;
    eprintln!(
        "[mostalgia] {}",
        trf(
            "Erkannt: {} · {} Zeilen · {} Sprecher · {} · {} min · {}",
            &[
                &done.format.label(),
                &done.lines,
                &done.speakers,
                &if done.timed { tr("mit Zeiten") } else { tr("ohne Zeiten") },
                &(done.duration_secs / 60.).round(),
                &if done.language == "auto" { tr("Sprache unbekannt").to_string() } else { text_import::language_display(&done.language) },
            ],
        )
    );
    if !sum.no_summary {
        if let Err(e) = sum.choice.request().and_then(|req| summarize(&store, id, &req, false, args.title.is_some())) {
            eprintln!("[mostalgia] {}", trf("Zusammenfassung übersprungen: {}", &[&format!("{e:#}")]));
        }
    }
    if !args.no_notes {
        let focus = args.focus.clone().unwrap_or_else(|| config.companion_notes_focus.clone());
        let notes = config.notes_request().and_then(|req| {
            eprintln!("[mostalgia] {}", trf("{} liest die Besprechung mit {} · {} …", &[&config.companion_character().name(), &req.connection.name, &req.model]));
            companion::regenerate_notes(id, style, &focus, &mut |_, _| {})
        });
        match notes {
            Ok(n) => eprintln!("[mostalgia] {}", trf("{} Notizen ({} · {})", &[&n.added, &n.provider, &n.model])),
            Err(e) => eprintln!("[mostalgia] {}", trf("Notizen übersprungen: {}", &[&format!("{e:#}")])),
        }
    }
    index_for_ask(id);
    // The summary may have renamed it.
    let title = store.title(id)?.map(|(t, _)| t).unwrap_or_default();
    println!("\n{}", trf("Gespeichert als Besprechung #{}: {} — `mostalgia show {}` zeigt sie an.", &[&id, &title, &id]));
    Ok(())
}

fn list(limit: usize) -> Result<()> {
    let meetings = open_store()?.list(limit)?;
    if meetings.is_empty() {
        println!("{}", tr("Noch keine Besprechungen — mit `mostalgia start` die erste aufnehmen."));
    }
    for m in meetings {
        println!(
            "#{:<4} {}  {:>3} min  {:>4} {}  {}  {}",
            m.id,
            i18n::date_time(&m.started_at),
            (m.duration_secs / 60.0).round(),
            m.segment_count,
            tr("Zeilen"),
            if m.has_summary { "✓" } else { " " },
            m.title
        );
    }
    Ok(())
}

fn show(id: i64, with_summary: bool, with_transcript: bool) -> Result<()> {
    let m = open_store()?.get(id)?.with_context(|| trf("Keine Besprechung #{}", &[&id]))?;
    println!(
        "{} #{} — {}\n{} · {} min · {}\n",
        tr("Besprechung"),
        m.id,
        m.title,
        i18n::date_time(&m.started_at),
        (m.duration_secs / 60.0).round(),
        m.dir.display()
    );
    if m.is_text_import() {
        println!("{}", if m.is_untimed() { tr("Aus Text importiert, ohne Audio und ohne Zeiten") } else { tr("Aus Text importiert, ohne Audio") });
    } else if let Some(engine) = &m.transcript_engine {
        println!("{}", trf("Transkribiert mit {}", &[engine]));
    }
    if let Some(error) = &m.transcript_error {
        println!("⚠ {error}\n  {}\n", trf("Erneut transkribieren: `mostalgia retranscribe {} --engine …`", &[&m.id]));
    }
    if with_summary {
        match &m.summary {
            Some(s) => println!(
                "{}\n\n({})\n",
                s.markdown,
                trf(
                    "Zusammenfassung von {} {}{}, {}",
                    &[
                        &summary::provider_display(&s.provider),
                        &s.model,
                        &s.effort.as_deref().map(|e| trf(", Denktiefe {}", &[&e])).unwrap_or_default(),
                        &i18n::date_time(&s.created_at),
                    ],
                )
            ),
            None => println!("({})\n", trf("noch keine Zusammenfassung — `mostalgia summarize {}`", &[&id])),
        }
    }
    if with_transcript {
        println!("{}:", tr("Transkript"));
        for s in &m.segments {
            let time = if m.is_untimed() { String::new() } else { format!("[{}] ", format_ts(s.start_secs)) };
            let who = if m.is_text_import() && s.speaker.is_none() { String::new() } else { format!("{}: ", i18n::speaker(s.speaker_label())) };
            println!("{time}{who}{}", s.text);
        }
    }
    Ok(())
}

fn search(query: &str, limit: usize) -> Result<()> {
    let hits = open_store()?.search(query, limit)?;
    if hits.is_empty() {
        println!("{}", trf("Keine Treffer für „{}“.", &[&query]));
    }
    for h in hits {
        println!(
            "#{:<4} {}  [{}] {}: {}   — {}",
            h.meeting_id,
            i18n::date(&h.started_at),
            format_ts(h.start_secs),
            i18n::speaker(h.source.speaker_label()),
            h.snippet,
            h.title
        );
    }
    Ok(())
}

fn export(id: i64, format: Option<&str>, output: Option<PathBuf>) -> Result<()> {
    use mostalgia_core::export::{self, Format};
    let m = open_store()?.get(id)?.with_context(|| trf("Keine Besprechung #{}", &[&id]))?;
    let format = match format {
        Some(f) => Format::from_extension(f).with_context(|| trf("Unbekanntes Format {} (md, txt, srt, docx)", &[&format!("{f:?}")]))?,
        None => output
            .as_ref()
            .and_then(|p| p.extension()?.to_str())
            .and_then(Format::from_extension)
            .unwrap_or(Format::Markdown),
    };
    let path = output.unwrap_or_else(|| PathBuf::from(export::suggested_name(&m, format)));
    export::write(&m, format, &path)?;
    println!("{}", trf("Besprechung #{} exportiert nach {}", &[&id, &path.display()]));
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
        .with_context(|| trf("{} konnte nicht gestartet werden", &[&app.display()]))?;
    Ok(())
}

/// Updates the `ask` index for a meeting; failures only warn.
fn index_for_ask(id: i64) {
    let t = std::time::Instant::now();
    match mostalgia_core::ask::index_meeting(id) {
        Ok(n) => eprintln!("[mostalgia] {}", trf("Suchindex: {} Abschnitte in {} s", &[&n, &format!("{:.2}", t.elapsed().as_secs_f64())])),
        Err(e) => eprintln!("[mostalgia] {}", trf("Suchindex nicht aktualisiert: {}", &[&format!("{e:#}")])),
    }
}

fn reindex(all: bool) -> Result<()> {
    use mostalgia_core::ask;
    if !ask::embed::available() {
        eprintln!("[mostalgia] {}", tr("Bedeutungsmodell fehlt (scripts/download-model.sh) — nur Stichwortsuche"));
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
    println!("{}", trf("{} Besprechung(en) in {} s indexiert.", &[&n, &format!("{:.2}", t.elapsed().as_secs_f64())]));
    Ok(())
}

fn docs_cmd(action: DocsAction) -> Result<()> {
    use mostalgia_core::docs;
    match action {
        DocsAction::List => {
            let list = docs::list()?;
            if list.is_empty() {
                println!("{}", tr("Noch keine Dokumente."));
            }
            for d in list {
                let state = match (&d.error, d.indexed_at) {
                    (Some(e), _) => trf("Fehler: {}", &[e]),
                    (None, None) => tr("nicht indexiert").to_string(),
                    (None, Some(_)) => trf("{} Abschnitte", &[&d.chunks]),
                };
                let lang = if d.language.is_empty() { "–" } else { d.language.as_str() };
                println!("#{:<4} {:<5} {lang:<3} {}  {}  ({state})", d.id, d.kind.id(), i18n::date(&d.added_at), d.title);
            }
        }
        DocsAction::Add { files } => {
            if !mostalgia_core::ask::embed::available() {
                eprintln!("[mostalgia] {}", tr("Bedeutungsmodell fehlt (scripts/download-model.sh) — nur Stichwortsuche"));
            }
            for f in files {
                match docs::add(&f) {
                    Ok((id, false)) => println!("{}", trf("{} ist schon da (#{})", &[&f.display(), &id])),
                    Ok((id, true)) => match docs::index(id) {
                        Ok(n) => println!("{}", trf("#{} {}: {} Abschnitte", &[&id, &f.display(), &n])),
                        Err(e) => eprintln!("#{id} {}: {e:#}", f.display()),
                    },
                    Err(e) => eprintln!("{e:#}"),
                }
            }
        }
        DocsAction::Search { query, limit } => {
            let hits = docs::search(&query.join(" "), None, limit)?;
            if hits.is_empty() {
                println!("{}", trf("Keine Treffer für „{}“.", &[&query.join(" ")]));
            }
            for h in hits {
                let heading = if h.heading.is_empty() { String::new() } else { format!(" · {}", h.heading) };
                let text: String = h.text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(160).collect();
                println!("#{:<4} {} · {}{heading}\n      {text}", h.document_id, h.title, docs::loc_label(h.kind, h.loc));
            }
        }
        DocsAction::Reindex { id } => println!("{}", trf("{} Abschnitte", &[&docs::index(id)?])),
        DocsAction::Remove { id } => {
            if !docs::delete(id)? {
                anyhow::bail!("{}", tr("Dokument nicht gefunden"));
            }
        }
    }
    Ok(())
}

fn suggest(question: &str, sources: Option<&str>) -> Result<()> {
    use mostalgia_core::assist::{self, Sources};
    let sources = match sources {
        Some(s) => Sources::parse(s).with_context(|| trf("Unbekannte Quelle {} (meetings, documents, both)", &[&s]))?,
        None => Config::load().answer_sources(),
    };
    if let Err(e) = mostalgia_core::ask::index_pending() {
        eprintln!("[mostalgia] {}", trf("Suchindex: {}", &[&format!("{e:#}")]));
    }
    let t = std::time::Instant::now();
    match assist::suggest(question, &[], sources)? {
        Some(s) => {
            let pieces = mostalgia_core::ask::parse_answer(&s.answer);
            let text: String = pieces
                .iter()
                .map(|p| match p {
                    mostalgia_core::ask::Piece::Text(t) => t.clone(),
                    mostalgia_core::ask::Piece::Cite(c) => format!("[{}]", c.label),
                })
                .collect();
            println!("{text}");
            eprintln!("[mostalgia] {} · {} hits · {:.1}s", s.model, s.hits, t.elapsed().as_secs_f64());
        }
        None => println!("{}", tr("Kein Vorschlag (keine passende Antwort in den Quellen).")),
    }
    Ok(())
}

fn ask(question: &str) -> Result<()> {
    use mostalgia_core::ask::{self, AskEvent, Piece};
    // Bring the index up to date first (cheap when nothing changed).
    if let Err(e) = ask::index_pending() {
        eprintln!("[mostalgia] {}", trf("Suchindex: {}", &[&format!("{e:#}")]));
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
                    eprintln!("[mostalgia] {}", tr("Antwort wird geschrieben …"));
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
                    println!("\n{}:", tr("Quellen"));
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
    let conn = connection(&Config::load(), provider)?;
    let models = match conn.require_key() {
        Ok(key) => mostalgia_core::llm::list_models(&conn, key.as_deref())?,
        Err(e) => {
            eprintln!("({e:#} — {})", tr("zeige die eingebaute Liste"));
            mostalgia_core::llm::fallback_models(&conn)
        }
    };
    for m in models {
        let efforts = if m.efforts.is_empty() { "—".to_string() } else { m.efforts.join(", ") };
        println!("{:<28} {:<26} {}: {efforts}", m.id, m.display_name, tr("Denktiefe"));
    }
    for m in conn.stt_models() {
        println!("{:<28} {:<26} {}", m.id, tr("Transkription"), trf("ca. {} $/h", &[&format!("{:.2}", m.usd_per_hour)]));
    }
    Ok(())
}

fn set_api_key(provider: &str) -> Result<()> {
    let conn = connection(&Config::load(), provider)?;
    let key = rpassword::prompt_password(trf("{}-API-Schlüssel: ", &[&conn.name])).context(tr("Schlüssel konnte nicht gelesen werden"))?;
    anyhow::ensure!(!key.trim().is_empty(), "{}", tr("Kein Schlüssel eingegeben"));
    conn.save_api_key(&key)?;
    println!("{}", tr("Im Schlüsselbund gespeichert (eine gesetzte Umgebungsvariable hat weiterhin Vorrang)."));
    Ok(())
}

fn connections_cmd(action: Option<ConnectionsAction>) -> Result<()> {
    let mut config = Config::load();
    match action.unwrap_or(ConnectionsAction::List) {
        ConnectionsAction::List => {
            let uses = |c: &Connection| {
                let mut u = Vec::new();
                if config.summary_provider == c.id {
                    u.push(tr("Zusammenfassungen"));
                }
                if config.ask_provider == c.id || config.ask_provider.is_empty() && config.summary_provider == c.id {
                    u.push(tr("Fragen"));
                }
                if [&config.live_engine, &config.final_engine].iter().any(|e| e.connection == c.id) {
                    u.push(tr("Transkription"));
                }
                u.join(", ")
            };
            for c in &config.connections {
                let status = if !c.is_configured() {
                    tr("Nicht eingerichtet").to_string()
                } else {
                    match mostalgia_core::llm::test(c) {
                        Ok(note) => format!("{} ({note})", tr("Verbunden")),
                        Err(e) => format!("{}: {e:#}", tr("Fehler")),
                    }
                };
                let place = if c.is_local() { tr("lokal") } else { tr("Cloud") };
                println!("{:<12} {:<18} {:<6} {}", c.id, c.name, place, c.base());
                println!("{:<12} {status}", "");
                let used = uses(c);
                if !used.is_empty() {
                    println!("{:<12} {}", "", trf("genutzt für: {}", &[&used]));
                }
            }
            println!("\n{}: {}", tr("Modus"), config.mode().label());
            println!("{}: {}", tr("Live-Transkription"), config.engine_label(&config.live_engine));
            println!("{}: {}", tr("Feinschliff"), config.engine_label(&config.final_engine));
            Ok(())
        }
        ConnectionsAction::Add { preset, name, url, models, key } => {
            anyhow::ensure!(
                connections::PRESETS.iter().any(|p| p.id == preset),
                "{}",
                trf("Unbekannte Vorlage {} (claude, openai, deepseek, ollama, litellm, lmstudio, vllm, openrouter, groq, together, custom)", &[&format!("{preset:?}")])
            );
            let id = connections::new_id(&config.connections, &preset);
            let mut conn = Connection::from_preset(connections::preset(&preset), &id);
            if let Some(name) = name {
                conn.name = name;
            }
            if let Some(url) = url {
                conn.base_url = url;
            }
            anyhow::ensure!(!conn.base().is_empty(), "{}", tr("Für einen eigenen Endpunkt --url angeben"));
            conn.models = models;
            if key {
                let k = rpassword::prompt_password(trf("{}-API-Schlüssel: ", &[&conn.name])).context(tr("Schlüssel konnte nicht gelesen werden"))?;
                if !k.trim().is_empty() {
                    conn.save_api_key(&k)?;
                }
            }
            println!("{}", trf("Verbindung {} hinzugefügt ({}).", &[&conn.id, &conn.base()]));
            config.connections.push(conn);
            config.save()
        }
        ConnectionsAction::Test { id } => {
            let conn = connection(&config, &id)?;
            println!("{}: {}", conn.name, mostalgia_core::llm::test(&conn)?);
            Ok(())
        }
        ConnectionsAction::Remove { id } => {
            let conn = connection(&config, &id)?;
            let in_use = config.summary_provider == id
                || config.ask_provider == id
                || config.live_engine.connection == id
                || config.final_engine.connection == id;
            anyhow::ensure!(!in_use, "{}", trf("{} ist noch in Gebrauch — erst in den Einstellungen etwas anderes wählen", &[&conn.name]));
            conn.delete_api_key()?;
            config.connections.retain(|c| c.id != id);
            config.save()?;
            println!("{}", trf("Verbindung {} entfernt.", &[&id]));
            Ok(())
        }
    }
}

fn list_devices() -> Result<()> {
    let sources = audio::list_sources()?;
    for (title, monitors) in [(tr("Mikrofone (--mic-device)"), false), (tr("Systemaudio (--system-device)"), true)] {
        println!("{title}:");
        for s in sources.iter().filter(|s| s.is_monitor == monitors) {
            let mark = if s.is_default { "*" } else { " " };
            let unplugged = if s.available { String::new() } else { format!("  ({})", tr("nicht angeschlossen")) };
            println!("  {mark} {}{unplugged}\n      {}", s.label, s.name);
        }
        println!();
    }
    println!("{}", tr("* = wird standardmäßig verwendet"));
    Ok(())
}

fn transcribe_file(file: &Path, models: &ModelArgs) -> Result<()> {
    let samples = finalize::read_wav(file)?;
    let mut engine = models.build_engine(&models.final_choice()?)?;
    let t = std::time::Instant::now();
    // Treated as the system track so --diarize applies; --denoise as the mic.
    let source = if models.denoise() { Source::Mic } else { Source::System };
    let segments = finalize::final_pass(&mut engine, &[(source, file.to_path_buf())], &models.final_pass_options())?;
    let elapsed = t.elapsed().as_secs_f64();
    for seg in segments {
        match &seg.speaker {
            Some(who) => println!("[{}] {}: {}", format_ts(seg.start_secs), i18n::speaker(who), seg.text),
            None => println!("[{}] {}", format_ts(seg.start_secs), seg.text),
        }
    }
    let audio_secs = samples.len() as f64 / mostalgia_core::SAMPLE_RATE as f64;
    eprintln!(
        "[mostalgia] {}",
        trf(
            "{} s Audio in {} s transkribiert ({}× Echtzeit)",
            &[&format!("{audio_secs:.1}"), &format!("{elapsed:.2}"), &format!("{:.1}", audio_secs / elapsed)],
        )
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_text_args() {
        let cli = Cli::try_parse_from(["mostalgia", "import-text", "--title", "Kickoff", "--date", "2026-10-27 14:00", "--duration", "45", "--no-summary", "-"]).unwrap();
        let Command::ImportText { file, title, date, duration, language, no_notes, summary, .. } = cli.command else { panic!("not import-text") };
        assert_eq!(file, PathBuf::from("-"));
        assert_eq!((title.as_deref(), date.as_deref(), duration.as_deref(), language), (Some("Kickoff"), Some("2026-10-27 14:00"), Some("45"), None));
        assert!(summary.no_summary && !no_notes);
        // `import` still takes a recording folder.
        let cli = Cli::try_parse_from(["mostalgia", "import", "/tmp/session-1"]).unwrap();
        assert!(matches!(cli.command, Command::Import { .. }));
        assert!(Cli::try_parse_from(["mostalgia", "import-text"]).is_err());
    }
}
