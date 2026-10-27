//! The recording screen: set up, record with a live transcript, finalize.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::Receiver;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, StyledExt, h_flex, switch::Switch, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mostalgia_core::config::Config;
use mostalgia_core::finalize;
use mostalgia_core::recorder::{Event, RecordConfig, Recorder};
use mostalgia_core::store::{Bookmark, NewMeeting, Store};
use mostalgia_core::transcribe::Engine;
use mostalgia_core::{Source, paths};

use crate::ui;

/// The Whisper engine, kept loaded between recordings.
pub type SharedEngine = Arc<Mutex<Option<Engine>>>;

pub enum RecordingEvent {
    /// A meeting was recorded and saved; open it.
    Saved(i64),
}

impl EventEmitter<RecordingEvent> for RecordingView {}

struct LiveLine {
    source: Source,
    speaker: Option<String>,
    start_secs: f64,
    text: SharedString,
}

enum Phase {
    Setup,
    Loading,
    Recording(Box<Live>),
    Finalizing { what: &'static str },
}

struct Live {
    /// `None` only for the screenshot demo.
    recorder: Option<Recorder>,
    rx: Receiver<Event>,
    started: Instant,
    paused_total: Duration,
    paused_since: Option<Instant>,
    lines: Vec<LiveLine>,
    /// Provisional text per source (mic, system) for the utterance still
    /// being spoken: (start seconds, text).
    interim: [Option<(f64, SharedString)>; 2],
    /// Smoothed meter levels (mic, system), 0..1.
    levels: [f32; 2],
    bookmarks: Vec<Bookmark>,
    last_latency_ms: Option<u128>,
}

impl Live {
    fn elapsed(&self) -> Duration {
        let paused_now = self.paused_since.map(|t| t.elapsed()).unwrap_or_default();
        self.started.elapsed().saturating_sub(self.paused_total + paused_now)
    }
}

pub struct RecordingView {
    config: Config,
    engine: SharedEngine,
    gpu: Option<String>,
    phase: Phase,
    error: Option<SharedString>,
    scroll: ScrollHandle,
    /// Keep the newest line in view; turned off when the user scrolls up.
    follow: bool,
    focus: FocusHandle,
}

impl RecordingView {
    pub fn new(engine: SharedEngine, cx: &mut Context<Self>) -> Self {
        Self {
            config: Config::load(),
            engine,
            gpu: None,
            phase: Phase::Setup,
            error: None,
            scroll: ScrollHandle::new(),
            follow: true,
            focus: cx.focus_handle(),
        }
    }

    /// Shows a recording in progress with the given lines, without any
    /// audio — for screenshots of the UI.
    #[doc(hidden)]
    pub fn demo(&mut self, secs: u64, lines: &[(Source, f64, &str)], cx: &mut Context<Self>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        drop(tx);
        self.gpu = Some("NVIDIA RTX PRO 3000 Blackwell Generation Laptop GPU".into());
        self.phase = Phase::Recording(Box::new(Live {
            recorder: None,
            rx,
            started: Instant::now() - Duration::from_secs(secs),
            paused_total: Duration::ZERO,
            paused_since: None,
            lines: lines
                .iter()
                .map(|(source, start_secs, text)| LiveLine {
                    source: *source,
                    speaker: None,
                    start_secs: *start_secs,
                    text: SharedString::from(text.to_string()),
                })
                .collect(),
            interim: [None, Some((58.3, "Und noch eine Sache zum Termin mit dem Kunden".into()))],
            levels: [0.62, 0.48],
            bookmarks: vec![Bookmark {
                at_secs: 41.0,
                label: "Markierung 1".into(),
            }],
            last_latency_ms: Some(212),
        }));
        cx.notify();
    }

    pub fn is_active(&self) -> bool {
        !matches!(self.phase, Phase::Setup)
    }

    pub fn is_recording(&self) -> bool {
        matches!(self.phase, Phase::Recording(_))
    }

    /// Re-read settings (after the settings screen changed them).
    pub fn reload_config(&mut self, cx: &mut Context<Self>) {
        if !self.is_active() {
            self.config = Config::load();
            cx.notify();
        }
    }

    fn save_config(&self) {
        if let Err(e) = self.config.save() {
            eprintln!("[mostalgia] saving settings: {e:#}");
        }
    }

    pub fn start(&mut self, cx: &mut Context<Self>) {
        if self.is_active() {
            return;
        }
        if !self.config.record_mic && !self.config.record_system {
            self.error = Some("Mikrofon oder Desktop-Audio einschalten.".into());
            cx.notify();
            return;
        }
        self.error = None;
        self.phase = Phase::Loading;
        cx.notify();

        let engine = self.engine.clone();
        let model = self.config.whisper_model_path();
        let language = self.config.language.clone();
        let load = cx.background_spawn(async move {
            let mut slot = engine.lock().unwrap();
            if slot.is_none() {
                anyhow::ensure!(
                    model.exists(),
                    "Whisper-Modell nicht gefunden: {} — scripts/download-model.sh ausführen",
                    model.display()
                );
                *slot = Some(Engine::load(&model, &language)?);
            }
            Ok::<_, anyhow::Error>(slot.as_ref().and_then(|e| e.gpu().map(str::to_string)))
        });
        cx.spawn(async move |this, cx| {
            let gpu = load.await;
            this.update(cx, |this, cx| match gpu {
                Ok(gpu) => {
                    this.gpu = gpu;
                    this.begin(cx);
                }
                Err(e) => this.fail(e, cx),
            })
            .ok();
        })
        .detach();
    }

    fn begin(&mut self, cx: &mut Context<Self>) {
        let Some(mut engine) = self.engine.lock().unwrap().take() else {
            return self.fail(anyhow::anyhow!("Modell nicht geladen"), cx);
        };
        if let Err(e) = engine.set_language(&self.config.language) {
            return self.fail(e, cx);
        }
        let cfg = RecordConfig {
            vad_model: self.config.vad_model_path(),
            vad_threshold: self.config.vad_threshold,
            out_dir: paths::recordings_dir(),
            mic: self.config.record_mic,
            system: self.config.record_system,
            mic_device: self.config.mic_device.clone(),
            system_device: self.config.system_device.clone(),
            diarize: self.config.diarize,
            denoise: self.config.denoise,
            interim: self.config.interim_text,
        };
        let (tx, rx) = crossbeam_channel::unbounded();
        match Recorder::start(cfg, engine, tx) {
            Ok(recorder) => {
                self.follow = true;
                self.phase = Phase::Recording(Box::new(Live {
                    recorder: Some(recorder),
                    rx,
                    started: Instant::now(),
                    paused_total: Duration::ZERO,
                    paused_since: None,
                    lines: Vec::new(),
                    interim: [None, None],
                    levels: [0.; 2],
                    bookmarks: Vec::new(),
                    last_latency_ms: None,
                }));
                self.poll_loop(cx);
            }
            // Recorder::start consumed the engine; it reloads next time.
            Err(e) => self.fail(e, cx),
        }
        cx.notify();
    }

    fn fail(&mut self, e: anyhow::Error, cx: &mut Context<Self>) {
        self.error = Some(format!("{e:#}").into());
        self.phase = Phase::Setup;
        cx.notify();
    }

    /// Drains recorder events ~30×/s while recording.
    fn poll_loop(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(33)).await;
                let keep = this.update(cx, |this, cx| this.poll(cx)).unwrap_or(false);
                if !keep {
                    break;
                }
            }
        })
        .detach();
    }

    fn poll(&mut self, cx: &mut Context<Self>) -> bool {
        let Phase::Recording(live) = &mut self.phase else {
            return false;
        };
        let at_bottom = {
            let max = self.scroll.max_offset().y;
            max <= px(1.) || -self.scroll.offset().y >= max - px(24.)
        };
        let mut new_lines = false;
        let mut fresh = [0f32; 2];
        for event in live.rx.try_iter() {
            match event {
                Event::Interim { source, start_secs, text } => {
                    live.interim[source as usize] = (!text.is_empty()).then(|| (start_secs, text.into()));
                    new_lines = true;
                }
                Event::Line { source, speaker, start_secs, text, .. } => {
                    live.interim[source as usize] = None;
                    live.lines.push(LiveLine {
                        source,
                        speaker,
                        start_secs,
                        text: text.into(),
                    });
                    new_lines = true;
                }
                Event::Level { source, rms } => {
                    let i = source as usize;
                    fresh[i] = fresh[i].max(ui::meter_level(rms));
                }
                Event::Chunk { millis, .. } => live.last_latency_ms = Some(millis),
                Event::Warning(w) => eprintln!("[mostalgia] {w}"),
            }
        }
        // Meters: rise quickly but not instantly, fall slowly — smooth at 30 Hz.
        for i in 0..2 {
            let prev = live.levels[i];
            live.levels[i] = if fresh[i] > prev { prev + (fresh[i] - prev) * 0.6 } else { (prev * 0.85).max(fresh[i]) };
        }
        if new_lines {
            self.follow = at_bottom || self.follow && at_bottom;
            if self.follow {
                self.scroll.scroll_to_bottom();
            }
        } else if !at_bottom {
            self.follow = false;
        } else {
            self.follow = true;
        }
        cx.notify();
        true
    }

    fn toggle_pause(&mut self, cx: &mut Context<Self>) {
        if let Phase::Recording(live) = &mut self.phase {
            match live.paused_since.take() {
                Some(since) => {
                    live.paused_total += since.elapsed();
                    if let Some(r) = &live.recorder {
                        r.set_paused(false);
                    }
                }
                None => {
                    live.paused_since = Some(Instant::now());
                    if let Some(r) = &live.recorder {
                        r.set_paused(true);
                    }
                }
            }
            cx.notify();
        }
    }

    fn mark_moment(&mut self, cx: &mut Context<Self>) {
        if let Phase::Recording(live) = &mut self.phase {
            let at = live.elapsed().as_secs_f64();
            let n = live.bookmarks.len() + 1;
            live.bookmarks.push(Bookmark {
                at_secs: at,
                label: format!("Markierung {n}"),
            });
            cx.notify();
        }
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        let Phase::Recording(live) = std::mem::replace(&mut self.phase, Phase::Finalizing { what: "Transkript wird fertiggestellt …" }) else {
            return;
        };
        let live = *live;
        let Some(recorder) = live.recorder else {
            self.phase = Phase::Setup;
            return;
        };
        recorder.stop();
        let bookmarks = live.bookmarks;
        let config = self.config.clone();
        let engine_slot = self.engine.clone();
        cx.notify();

        let task = cx.background_spawn(async move {
            let (rec, mut engine) = recorder.join()?;
            let vad = config.vad_model_path();
            let opts = finalize::FinalPassOptions {
                vad_model: vad.exists().then_some(vad),
                diarize: config.diarize,
                denoise_mic: config.denoise,
            };
            let result = (|| -> Result<i64> {
                let segments = finalize::final_pass(&mut engine, &rec.tracks, &opts)?;
                let mut store = Store::open(&paths::database())?;
                let title = format!("Besprechung {}", rec.started_at.format("%d.%m.%Y %H:%M"));
                let id = store.insert_meeting(
                    &NewMeeting {
                        title: &title,
                        started_at: rec.started_at,
                        duration_secs: rec.duration_secs,
                        language: &config.language,
                        dir: &rec.dir,
                    },
                    &segments,
                )?;
                store.add_bookmarks(id, &bookmarks)?;
                if let Err(e) = mostalgia_core::ask::index_meeting(id) {
                    eprintln!("[mostalgia] search index: {e:#}");
                }
                Ok(id)
            })();
            *engine_slot.lock().unwrap() = Some(engine);
            result
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| match result {
                Ok(id) => {
                    this.phase = Phase::Setup;
                    cx.emit(RecordingEvent::Saved(id));
                    cx.notify();
                }
                Err(e) => this.fail(e, cx),
            })
            .ok();
        })
        .detach();
    }

    fn render_setup(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let source_row = |id: &'static str, icon: IconName, title: &'static str, detail: String, on: bool, cx: &mut Context<Self>| {
            let theme = cx.theme();
            h_flex()
                .gap_3()
                .p_3()
                .rounded(theme.radius)
                .border(ui::border_width(cx))
                .border_color(if on { theme.primary.opacity(0.5) } else { theme.border })
                .child(Icon::new(icon).text_color(if on { theme.primary } else { theme.muted_foreground }))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().font_medium().child(title))
                        .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(detail)),
                )
                .child(Switch::new(id).checked(on).on_change(cx.listener(move |this, checked: &bool, _, cx| {
                    match id {
                        "mic" => this.config.record_mic = *checked,
                        _ => this.config.record_system = *checked,
                    }
                    this.save_config();
                    cx.notify();
                })))
        };
        let mic_detail = self.config.mic_device.clone().unwrap_or_else(|| "Standard-Mikrofon".into());
        let sys_detail = self.config.system_device.clone().unwrap_or_else(|| "Alles, was gerade abgespielt wird".into());
        let lang = self.config.language.clone();
        let lang_button = |code: &'static str, label: &'static str, cx: &mut Context<Self>| {
            Button::new(SharedString::from(format!("lang-{code}")))
                .label(label)
                .small()
                .when(lang == code, |b| b.primary())
                .when(lang != code, |b| b.ghost())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.config.language = code.into();
                    this.save_config();
                    cx.notify();
                }))
        };

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .p_8()
            .child(
                v_flex()
                    .w(px(460.))
                    .gap_5()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_2xl().font_semibold().child("Neue Aufnahme"))
                            .child(
                                div()
                                    .text_color(theme.muted_foreground)
                                    .child("Nimmt dein Mikrofon und den Ton deines Rechners auf und schreibt live mit."),
                            ),
                    )
                    .child(source_row("mic", IconName::Mic, "Mikrofon", mic_detail, self.config.record_mic, cx))
                    .child(source_row("system", IconName::Monitor, "Desktop-Audio", sys_detail, self.config.record_system, cx))
                    .child(
                        h_flex()
                            .justify_between()
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(div().text_sm().text_color(cx.theme().muted_foreground).mr_2().child("Sprache"))
                                    .child(lang_button("de", "Deutsch", cx))
                                    .child(lang_button("en", "English", cx))
                                    .child(lang_button("auto", "Automatisch", cx)),
                            )
                            .child(
                                Switch::new("labels")
                                    .label("Sprecher")
                                    .checked(self.config.speaker_labels)
                                    .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                        this.config.speaker_labels = *checked;
                                        this.save_config();
                                        cx.notify();
                                    })),
                            ),
                    )
                    .when_some(self.error.clone(), |el, err| {
                        el.child(
                            div()
                                .p_3()
                                .rounded(cx.theme().radius)
                                .bg(cx.theme().danger.opacity(0.1))
                                .text_color(cx.theme().danger)
                                .text_sm()
                                .child(err),
                        )
                    })
                    .child(
                        Button::new("start")
                            .primary()
                            .large()
                            .w_full()
                            .icon(IconName::Mic)
                            .label("Aufnahme starten")
                            .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
                    ),
            )
    }

    fn render_busy(&self, text: &'static str, stopping: bool, cx: &App) -> impl IntoElement {
        let spec = crate::themes::active(cx);
        let burst = stopping && spec.animations && !self.config.reduce_motion && spec.scene.as_deref() == Some("blocks");
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .when(burst, |el| el.child(anim::block_break(palette(&spec, cx))))
            .child(gpui_kit::component::spinner::Spinner::new().large())
            .child(div().text_color(cx.theme().muted_foreground).child(text))
    }

    fn render_live(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let speaker_labels = self.config.speaker_labels;
        let follow = self.follow;
        let gpu = self.gpu.clone();
        let Phase::Recording(live) = &self.phase else {
            unreachable!()
        };
        let spec = crate::themes::active(cx);
        let motion = spec.animations && !self.config.reduce_motion;
        let playful = motion && spec.playful;
        let colors = palette(&spec, cx);
        let theme = cx.theme();
        let paused = live.paused_since.is_some();
        let elapsed = live.elapsed().as_secs_f64();
        let enabled = [self.config.record_mic, self.config.record_system];

        let meter = |source: Source, cx: &App| {
            let i = source as usize;
            h_flex()
                .gap_2()
                .w(px(190.))
                .child(
                    Icon::new(if source == Source::Mic { IconName::Mic } else { IconName::Monitor })
                        .small()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(ui::level_meter(live.levels[i], enabled[i] && !paused, cx))
        };

        let header = h_flex()
            .px_6()
            .py_4()
            .gap_6()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_3()
                    .child({
                        let dot = div().size(px(12.)).rounded_full().bg(if paused { theme.warning } else { theme.danger });
                        if motion && !paused { anim::pulse(dot) } else { dot.into_any_element() }
                    })
                    .child(
                        div()
                            .text_3xl()
                            .font_semibold()
                            .font_family(theme.mono_font_family.clone())
                            .child(ui::hms(elapsed)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(if paused { "Pausiert" } else { "Aufnahme läuft" }),
                    ),
            )
            .child(v_flex().gap_2().child(meter(Source::Mic, cx)).child(meter(Source::System, cx)))
            .child(div().flex_1())
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("mark")
                            .icon(IconName::Bookmark)
                            .label("Markieren")
                            .ghost()
                            .tooltip("Moment markieren")
                            .on_click(cx.listener(|this, _, _, cx| this.mark_moment(cx))),
                    )
                    .child(
                        Button::new("pause")
                            .icon(if paused { IconName::Play } else { IconName::Pause })
                            .label(if paused { "Fortsetzen" } else { "Pause" })
                            .outline()
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_pause(cx))),
                    )
                    .child(
                        Button::new("stop")
                            .icon(IconName::Square)
                            .label("Beenden")
                            .danger()
                            .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                    ),
            );

        let mut rows: Vec<AnyElement> = Vec::new();
        let mut marks = live.bookmarks.iter().peekable();
        for (ix, line) in live.lines.iter().enumerate() {
            while let Some(b) = marks.next_if(|b| b.at_secs <= line.start_secs) {
                rows.push(bookmark_row(b, cx).into_any_element());
            }
            let label = line.speaker.as_deref().unwrap_or(line.source.speaker_label());
            let row = h_flex()
                    .id(("line", ix))
                    .items_start()
                    .gap_3()
                    .py_1p5()
                    .child(
                        div()
                            .w(px(52.))
                            .flex_none()
                            .pt_0p5()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .font_family(theme.mono_font_family.clone())
                            .child(ui::hms(line.start_secs)),
                    )
                    .when(speaker_labels, |el| el.child(ui::speaker_chip(label, cx)))
                    .child(ui::bidi_text(line.text.clone(), true).flex_1().line_height(rems(1.5)));
            rows.push(if motion { anim::arrive(row, ("line-in", ix), playful) } else { row.into_any_element() });
        }
        for b in marks {
            rows.push(bookmark_row(b, cx).into_any_element());
        }
        // Provisional text for what's still being said, greyed and italic.
        for (i, slot) in live.interim.iter().enumerate() {
            let Some((start, text)) = slot else { continue };
            let source = if i == 0 { Source::Mic } else { Source::System };
            let row = h_flex()
                    .id(("interim", i))
                    .items_start()
                    .gap_3()
                    .py_1p5()
                    .child(
                        div()
                            .w(px(52.))
                            .flex_none()
                            .pt_0p5()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .font_family(theme.mono_font_family.clone())
                            .child(ui::hms(*start)),
                    )
                    .when(speaker_labels, |el| el.child(ui::speaker_chip(source.speaker_label(), cx)))
                    .child(div().flex_1().italic().line_height(rems(1.5)).text_color(theme.muted_foreground).child(format!("{text} …")));
            rows.push(if motion { anim::breathe(row, ("interim-breathe", i)) } else { row.opacity(0.6).into_any_element() });
        }
        let empty = live.lines.is_empty();

        let footer = h_flex()
            .px_6()
            .py_2()
            .gap_4()
            .border_t_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(format!("{} Zeilen", live.lines.len()))
            .when_some(live.last_latency_ms, |el, ms| el.child(format!("Latenz {ms} ms")))
            .when_some(gpu, |el, gpu| el.child(gpu))
            .child(div().flex_1())
            .when(!follow, |el| {
                el.child(
                    Button::new("follow")
                        .xsmall()
                        .ghost()
                        .icon(IconName::ArrowDown)
                        .label("Zum Ende")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.follow = true;
                            this.scroll.scroll_to_bottom();
                            cx.notify();
                        })),
                )
            });

        v_flex()
            .size_full()
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .when_some(
                        spec.scene.as_deref().filter(|_| empty && motion).and_then(|kind| anim::scene(kind, colors)),
                        |el, scene| el.child(scene),
                    )
                    .child(
                    v_flex()
                        .id("live-transcript")
                        .size_full()
                        .px_6()
                        .py_4()
                        .track_scroll(&self.scroll)
                        .overflow_y_scroll()
                        .children(rows)
                        .when(empty, |el| {
                            el.child(
                                v_flex()
                                    .flex_1()
                                    .items_center()
                                    .justify_center()
                                    .gap_2()
                                    .pt_24()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if playful {
                                        anim::bob(Icon::new(IconName::AudioLines).large().text_color(cx.theme().primary))
                                    } else {
                                        Icon::new(IconName::AudioLines).large().into_any_element()
                                    })
                                    .child("Sobald jemand spricht, erscheint hier der Text."),
                            )
                        }),
                ),
            )
            .child(footer)
    }
}

fn bookmark_row(b: &Bookmark, cx: &App) -> Div {
    let theme = cx.theme();
    h_flex()
        .gap_3()
        .py_1()
        .text_xs()
        .text_color(theme.warning)
        .child(
            div()
                .w(px(52.))
                .flex_none()
                .font_family(theme.mono_font_family.clone())
                .child(ui::hms(b.at_secs)),
        )
        .child(Icon::new(IconName::Bookmark).xsmall())
        .child(b.label.clone())
        .child(div().flex_1().h(px(1.)).bg(theme.warning.opacity(0.3)))
}

impl Focusable for RecordingView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for RecordingView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.phase {
            Phase::Setup => self.render_setup(cx).into_any_element(),
            Phase::Loading => self.render_busy("Modell wird geladen …", false, cx).into_any_element(),
            Phase::Finalizing { what } => self.render_busy(what, true, cx).into_any_element(),
            Phase::Recording(_) => self.render_live(cx).into_any_element(),
        };
        div().size_full().track_focus(&self.focus).child(content)
    }
}

/// Motion for the recording screen. Everything here is decoration: cheap
/// (capped at 30 fps, a few dozen plain divs), behind or around the
/// transcript, never on the text itself beyond a short fade-in, and off
/// when the theme or the "reduce motion" setting says so.
mod anim {
    use std::f32::consts::TAU;
    use std::time::Duration;

    use gpui_kit::component::{h_flex, v_flex};
    use gpui_kit::prelude::*;
    use gpui_kit::*;

    /// Overshooting ease-out: past the target, then back — a cartoon "pop".
    fn back_out(t: f32) -> f32 {
        let (c1, c3) = (1.70158, 2.70158);
        let x = t - 1.0;
        1.0 + c3 * x * x * x + c1 * x * x
    }

    /// A transcript row arriving: fades in while sliding up a little;
    /// playful themes pop with a small overshoot.
    pub fn arrive(row: Stateful<Div>, id: impl Into<ElementId>, playful: bool) -> AnyElement {
        let row = row.relative();
        if playful {
            row.with_animation(id, Animation::new(Duration::from_millis(420)).with_easing(back_out), |el, d| {
                el.opacity(d.clamp(0., 1.)).top(px((1. - d) * 14.))
            })
            .into_any_element()
        } else {
            row.with_animation(id, Animation::new(Duration::from_millis(300)).with_easing(ease_out_quint()), |el, d| {
                el.opacity(d).top(px((1. - d) * 6.))
            })
            .into_any_element()
        }
    }

    /// The breathing recording dot.
    pub fn pulse(dot: Div) -> AnyElement {
        dot.with_animation(
            "rec-dot",
            Animation::new(Duration::from_millis(1400)).repeat().with_easing(pulsating_between(0.35, 1.0)),
            |el, d| el.opacity(d),
        )
        .into_any_element()
    }

    /// Provisional text gently breathing so it reads as "still changing".
    pub fn breathe(row: Stateful<Div>, id: impl Into<ElementId>) -> AnyElement {
        row.with_animation(
            id,
            Animation::new(Duration::from_millis(2000)).repeat().with_easing(pulsating_between(0.45, 0.75)),
            |el, d| el.opacity(d),
        )
        .into_any_element()
    }

    /// An icon bobbing up and down (playful empty state).
    pub fn bob(el: impl IntoElement + 'static) -> AnyElement {
        div()
            .relative()
            .child(el)
            .with_animation(
                "bob",
                Animation::new(Duration::from_millis(1100)).repeat().with_easing(bounce(ease_in_out)),
                |el, d| el.top(px(-10. * d)),
            )
            .into_any_element()
    }

    /// Repeating ambient layer: `draw` adds children for the phase `t` (0..1).
    fn ambient(id: &'static str, secs: u64, draw: impl Fn(Div, f32) -> Div + 'static) -> AnyElement {
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .overflow_hidden()
            .with_animation(id, Animation::new(Duration::from_secs(secs)).repeat().with_max_fps(30.), move |el, t| draw(el, t))
            .into_any_element()
    }

    /// Deterministic pseudo-random 0..1 for decoration layouts.
    fn rnd(i: usize, salt: u32) -> f32 {
        let mut x = (i as u32).wrapping_mul(2_654_435_761) ^ salt.wrapping_mul(40_503);
        x ^= x >> 13;
        x = x.wrapping_mul(1_274_126_177);
        (x >> 8) as f32 / (1u32 << 24) as f32
    }

    fn dot(size: f32, color: Hsla) -> Div {
        div().absolute().size(px(size)).rounded_full().bg(color)
    }

    #[derive(Clone, Copy)]
    pub struct Palette {
        pub bg: Hsla,
        pub fg: Hsla,
        pub primary: Hsla,
        pub muted: Hsla,
        pub surface: Hsla,
        pub accents: [Hsla; 4],
    }

    /// The illustration behind the empty live transcript, by theme scene.
    pub fn scene(kind: &str, p: Palette) -> Option<AnyElement> {
        Some(match kind {
            // Synthwave: a striped sun on the horizon and grid lines
            // sliding toward the viewer.
            "grid" => ambient("scene-grid", 4, move |mut el, t| {
                let horizon = 0.52;
                el = el.child(
                    div()
                        .absolute()
                        .top(relative(horizon - 0.28))
                        .left(relative(0.5))
                        .ml(px(-80.))
                        .size(px(160.))
                        .rounded_full()
                        .overflow_hidden()
                        .bg(p.primary.opacity(0.35))
                        .children((0..5).map(|k| {
                            div().absolute().left_0().w_full().top(px(92. + k as f32 * 14.)).h(px(4. + k as f32)).bg(p.bg)
                        })),
                );
                el = el.child(div().absolute().left_0().w_full().top(relative(horizon)).h(px(2.)).bg(p.primary.opacity(0.6)));
                for k in 0..12 {
                    let f = (k as f32 + t) / 12.;
                    el = el.child(
                        div()
                            .absolute()
                            .left_0()
                            .w_full()
                            .h(px(1.))
                            .top(relative(horizon + f * f * (1. - horizon)))
                            .bg(p.accents[1].opacity(0.08 + 0.3 * f)),
                    );
                }
                el
            }),
            // Blockwelt: a row of grass blocks hopping in turn.
            "blocks" => ambient("scene-blocks", 2, move |mut el, t| {
                for i in 0..5 {
                    let hop = ((t + i as f32 / 5.) * TAU).sin().max(0.) * 16.;
                    el = el.child(
                        v_flex()
                            .absolute()
                            .top(relative(0.26))
                            .mt(px(-hop))
                            .left(relative(0.5))
                            .ml(px(-110. + i as f32 * 46.))
                            .size(px(34.))
                            .border_2()
                            .border_color(hsla(0., 0., 0.08, 1.))
                            .child(div().h(px(9.)).w_full().bg(rgb(0x5ea836)))
                            .child(div().flex_1().w_full().bg(rgb(0x7b5534))),
                    );
                }
                el
            }),
            // Comic: a thought bubble with three dots bouncing in turn.
            "comic" => ambient("scene-comic", 1, move |el, t| {
                let bubble = h_flex()
                    .absolute()
                    .top(relative(0.14))
                    .left(relative(0.5))
                    .ml(px(-80.))
                    .w(px(160.))
                    .h(px(80.))
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .rounded(px(40.))
                    .border_3()
                    .border_color(p.fg)
                    .bg(p.surface)
                    .children((0..3).map(|i| {
                        let lift = ((t - i as f32 / 6.) * TAU).sin().max(0.) * 10.;
                        div().size(px(14.)).mb(px(lift)).rounded_full().bg(p.accents[i % 4])
                    }));
                el.child(bubble)
                    .child(dot(18., p.fg).top(relative(0.14)).mt(px(86.)).left(relative(0.5)).ml(px(-58.)))
                    .child(dot(10., p.fg).top(relative(0.14)).mt(px(108.)).left(relative(0.5)).ml(px(-72.)))
            }),
            // Weltraum: twinkling stars and a ringed planet.
            "stars" => ambient("scene-stars", 6, move |mut el, t| {
                for i in 0..48 {
                    let tw = 0.25 + 0.75 * ((t * (1. + rnd(i, 3) * 2.) + rnd(i, 4)) * TAU).sin().abs();
                    el = el.child(dot(1.5 + rnd(i, 5) * 2.5, p.fg.opacity(tw * 0.8)).left(relative(rnd(i, 1))).top(relative(rnd(i, 2))));
                }
                let drift = (t * TAU).sin() * 6.;
                el.child(dot(76., p.primary.opacity(0.35)).right(relative(0.18)).top(relative(0.18)).mt(px(drift)))
                    .child(
                        div()
                            .absolute()
                            .right(relative(0.18))
                            .mr(px(-28.))
                            .top(relative(0.18))
                            .mt(px(drift + 26.))
                            .w(px(132.))
                            .h(px(24.))
                            .rounded_full()
                            .border_2()
                            .border_color(p.accents[0].opacity(0.5)),
                    )
            }),
            // Aurora: soft bands swaying across the sky.
            "aurora" => ambient("scene-aurora", 10, move |mut el, t| {
                for i in 0..3 {
                    let sway = ((t + i as f32 * 0.3) * TAU).sin() * 0.06;
                    el = el.child(
                        div()
                            .absolute()
                            .left(relative(0.04 + i as f32 * 0.08 + sway))
                            .top(relative(0.18 + i as f32 * 0.09))
                            .w(relative(0.7))
                            .h(px(54.))
                            .rounded_full()
                            .bg(p.accents[i].opacity(0.08)),
                    );
                }
                el
            }),
            // Ozean: layered waves rolling along the bottom.
            "waves" => ambient("scene-waves", 7, move |mut el, t| {
                for i in 0..4 {
                    let roll = ((t + i as f32 * 0.25) * TAU).sin() * 0.05;
                    el = el.child(
                        div()
                            .absolute()
                            .left(relative(-0.1 + (i % 2) as f32 * 0.25 + roll))
                            .top(relative(0.74 + i as f32 * 0.06))
                            .w(relative(0.85))
                            .h(px(46.))
                            .rounded_full()
                            .bg(p.primary.opacity(0.05 + i as f32 * 0.025)),
                    );
                }
                el
            }),
            // Wald / Sakura: leaves or petals drifting down.
            "leaves" | "petals" => {
                let petals = kind == "petals";
                ambient(if petals { "scene-petals" } else { "scene-leaves" }, 9, move |mut el, t| {
                    for i in 0..14 {
                        let y = (t + rnd(i, 7)) % 1.;
                        let x = rnd(i, 8) + (y * TAU * 2. + rnd(i, 9) * TAU).sin() * 0.03;
                        let color = if petals { p.primary.opacity(0.35) } else { p.accents[i % 2].opacity(0.4) };
                        el = el.child(
                            div()
                                .absolute()
                                .left(relative(x))
                                .top(relative(y * 1.1 - 0.05))
                                .size(px(8. + rnd(i, 10) * 6.))
                                .rounded_tl(px(10.))
                                .rounded_br(px(10.))
                                .bg(color),
                        );
                    }
                    el
                })
            }
            // Kaffee: a cup with steam curling up.
            "steam" => ambient("scene-steam", 3, move |mut el, t| {
                for i in 0..3 {
                    for k in 0..5 {
                        let f = (t + k as f32 / 5.) % 1.;
                        let x = (f * TAU + i as f32).sin() * 6.;
                        el = el.child(
                            dot(10. - f * 6., p.muted.opacity((1. - f) * 0.9))
                                .left(relative(0.5))
                                .ml(px(-28. + i as f32 * 22. + x))
                                .top(relative(0.3))
                                .mt(px(-20. - f * 70.)),
                        );
                    }
                }
                el.child(
                    div()
                        .absolute()
                        .left(relative(0.5))
                        .ml(px(-45.))
                        .top(relative(0.3))
                        .w(px(90.))
                        .h(px(56.))
                        .rounded_b(px(22.))
                        .bg(p.primary.opacity(0.55)),
                )
                .child(
                    div()
                        .absolute()
                        .left(relative(0.5))
                        .ml(px(40.))
                        .top(relative(0.3))
                        .mt(px(10.))
                        .size(px(26.))
                        .rounded_full()
                        .border_4()
                        .border_color(p.primary.opacity(0.55)),
                )
            }),
            // Retro 90er: Memphis-style shapes bobbing in place.
            "memphis" => ambient("scene-memphis", 5, move |mut el, t| {
                for i in 0..10 {
                    let bobv = ((t + rnd(i, 11)) * TAU).sin() * 8.;
                    let size = 14. + rnd(i, 12) * 22.;
                    let shape = div()
                        .absolute()
                        .left(relative(0.08 + rnd(i, 13) * 0.84))
                        .top(relative(0.1 + rnd(i, 14) * 0.8))
                        .mt(px(bobv))
                        .size(px(size));
                    let color = p.accents[i % 4].opacity(0.45);
                    el = el.child(match i % 3 {
                        0 => shape.rounded_full().bg(color),
                        1 => shape.bg(color),
                        _ => shape.rounded_full().border_4().border_color(color),
                    });
                }
                el
            }),
            _ => return None,
        })
    }

    /// Blockwelt: a little burst of pixel debris when recording stops.
    pub fn block_break(p: Palette) -> AnyElement {
        div()
            .relative()
            .size(px(120.))
            .with_animation("block-break", Animation::new(Duration::from_millis(700)).with_easing(ease_out_quint()), move |mut el, t| {
                for i in 0..12 {
                    let a = i as f32 / 12. * TAU;
                    let r = t * 50.;
                    let fall = t * t * 30.;
                    el = el.child(
                        div()
                            .absolute()
                            .left(px(56. + a.cos() * r))
                            .top(px(56. + a.sin() * r + fall))
                            .size(px(8.))
                            .opacity(1. - t)
                            .bg(if i % 3 == 0 { rgb(0x5ea836).into() } else { p.accents[(i % 2) + 1] }),
                    );
                }
                el
            })
            .into_any_element()
    }
}

/// Colours the decorations draw with, from the active theme.
fn palette(spec: &crate::themes::ThemeSpec, cx: &App) -> anim::Palette {
    let theme = cx.theme();
    let pick = |i: usize| crate::themes::hex(&spec.speakers[i % spec.speakers.len()]);
    anim::Palette {
        bg: theme.background,
        fg: theme.foreground,
        primary: theme.primary,
        muted: theme.muted_foreground,
        surface: theme.popover,
        accents: [pick(0), pick(1), pick(2), pick(3)],
    }
}
