//! Mori's controls: recording from her (a record button with a choice of
//! sources, then pause / resume / stop with the elapsed time), the "Notes"
//! tab of her bubble, her right-click menu, and the collapsed bar that
//! stands in for her while she's hidden.

use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, Selectable, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mostalgia_core::companion::PointKind;
use mostalgia_core::config::{Config, NotesMode};
use mostalgia_core::i18n::{tr, trf};
use mostalgia_core::store::{Note, Store};

use super::{Companion, MoriEvent, Point2, ShowLiveMoment};
use crate::recording::LiveState;
use crate::ui;

/// The collapsed bar's size (logical px).
pub(super) const BAR_W: f32 = 264.;
pub(super) const BAR_H: f32 = 40.;
const MENU_W: f32 = 210.;
const MENU_ITEM_H: f32 = 30.;
/// A start that never answered (the app is gone) stops spinning after this.
const START_TIMEOUT: Duration = Duration::from_secs(180);

/// Where the source choice is open.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ChoiceIn {
    Bubble,
    Bar,
}

/// How her bar shows that a recording runs (`Config::companion_bar_indicator`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BarIndicator {
    /// Soft twinkling sparkles in her colour: friendly, not alarming.
    #[default]
    Sparkles,
    Spinner,
    /// The red recording dot.
    Dot,
}

impl BarIndicator {
    pub const ALL: [BarIndicator; 3] = [BarIndicator::Sparkles, BarIndicator::Spinner, BarIndicator::Dot];

    pub fn parse(s: &str) -> Self {
        match s {
            "spinner" => BarIndicator::Spinner,
            "dot" => BarIndicator::Dot,
            _ => BarIndicator::Sparkles,
        }
    }

    pub fn as_config(self) -> &'static str {
        match self {
            BarIndicator::Sparkles => "sparkles",
            BarIndicator::Spinner => "spinner",
            BarIndicator::Dot => "dot",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            BarIndicator::Sparkles => tr("Funkeln"),
            BarIndicator::Spinner => tr("Kreisel"),
            BarIndicator::Dot => tr("Aufnahmepunkt"),
        }
    }
}

/// Three small sparkles twinkling one after another (`motion`), or
/// resting dimmed while the recording is paused; one still sparkle when
/// motion is reduced.
fn sparkles(color: Hsla, motion: bool, paused: bool) -> AnyElement {
    if !motion || paused {
        return Icon::new(IconName::Sparkles)
            .xsmall()
            .text_color(color.opacity(if paused { 0.4 } else { 0.85 }))
            .into_any_element();
    }
    let star = |i: usize, size: f32, lift: f32| {
        div()
            .relative()
            .top(px(lift))
            .child(Icon::new(IconName::Sparkle).size(px(size)).text_color(color))
            .with_animation(
                ("mori-bar-sparkle", i),
                Animation::new(Duration::from_millis(2400)).repeat(),
                move |el, t| {
                    // Each one brightens in turn, gently.
                    let phase = (t + i as f32 / 3.) % 1.;
                    let glow = 0.5 - 0.5 * (phase * std::f32::consts::TAU).cos();
                    el.opacity(0.25 + 0.75 * glow)
                },
            )
    };
    h_flex()
        .gap(px(1.))
        .items_center()
        .child(star(0, 9., -2.))
        .child(star(1, 12., 1.))
        .child(star(2, 8., -3.))
        .into_any_element()
}

#[derive(Default)]
pub(crate) struct ControlsState {
    /// How her bar shows a running recording.
    pub bar_indicator: BarIndicator,
    /// The "which sources" choice after the record button.
    pub choice: Option<ChoiceIn>,
    /// She asked for a recording and waits for it to run.
    pub starting: Option<Instant>,
    /// Why the recording she asked for didn't start.
    pub error: Option<String>,
    /// The bubble shows the notes instead of the chat.
    pub notes_tab: bool,
    /// The open meeting's saved notes (loaded when needed).
    saved: Option<SavedNotes>,
    notes_scroll: ScrollHandle,
    /// Her menu, at this offset from her top-left.
    pub menu: Option<(f32, f32)>,
    /// The elapsed second and sources shown last (re-render when they change).
    elapsed_shown: Option<(u64, [bool; 2])>,
}

struct SavedNotes {
    meeting_id: i64,
    notes: Result<Vec<Note>, String>,
}

/// One row of the Notes tab.
struct NoteRow {
    key: NoteKey,
    kind: Option<PointKind>,
    kind_raw: String,
    text: String,
    at: f64,
    pinned: bool,
}

#[derive(Clone, Copy)]
enum NoteKey {
    /// A point of the recording in progress (`LivePoint::id`).
    Live(usize),
    /// A note stored with a meeting.
    Saved { meeting_id: i64, note_id: i64 },
}

impl Companion {
    fn live(&self, cx: &App) -> Option<LiveState> {
        self.recording.read(cx).live_state()
    }

    /// Bookkeeping for the record controls each tick; returns whether to
    /// re-render (the elapsed time ticked on, a start went through).
    pub(super) fn track_record_controls(&mut self, live: Option<LiveState>) -> bool {
        let mut changed = false;
        if let Some(since) = self.ctl.starting
            && (live.is_some() || since.elapsed() > START_TIMEOUT)
        {
            self.ctl.starting = None;
            changed = true;
        }
        if live.is_some() && self.ctl.choice.is_some() {
            self.ctl.choice = None;
            changed = true;
        }
        let shown = live.map(|l| (l.elapsed_secs as u64, l.sources));
        if shown != self.ctl.elapsed_shown {
            self.ctl.elapsed_shown = shown;
            changed = true;
        }
        changed
    }

    fn start_recording(&mut self, mic: bool, system: bool, cx: &mut Context<Self>) {
        self.ctl.choice = None;
        self.ctl.error = None;
        self.ctl.starting = Some(Instant::now());
        self.idle_since = Instant::now();
        cx.emit(MoriEvent::StartRecording { mic, system });
        cx.notify();
    }

    /// The recording she asked for didn't start: the bubble says why.
    pub fn recording_start_failed(&mut self, message: String, cx: &mut Context<Self>) {
        if self.ctl.starting.take().is_none() {
            return;
        }
        self.ctl.error = Some(message);
        self.ctl.notes_tab = false;
        if !self.open {
            self.toggle_bubble(cx);
        }
        cx.notify();
    }

    /// Hides her figure (the bar stays) or shows it again; remembered.
    pub(super) fn set_hidden(&mut self, hidden: bool, cx: &mut Context<Self>) {
        self.ctl.menu = None;
        self.ctl.choice = None;
        if self.hidden == hidden {
            return;
        }
        self.hidden = hidden;
        self.drag = None;
        let mut config = Config::load();
        config.companion_hidden = hidden;
        if let Err(e) = config.save() {
            eprintln!("[mostalgia] saving Mori's settings: {e:#}");
        }
        let size = self.size();
        if let Some(p) = &mut self.desktop {
            p.drag = None;
            p.settle(size);
            p.save();
        }
        cx.notify();
    }

    /// Opens her menu where she was right-clicked.
    pub(super) fn open_menu(&mut self, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        let o = self.origin();
        self.ctl.menu = Some((f32::from(ev.position.x) - o.x, f32::from(ev.position.y) - o.y));
        self.ctl.choice = None;
        cx.stop_propagation();
        cx.notify();
    }

    fn menu_items(&self) -> usize {
        3
    }

    /// The menu's rectangle (window coordinates), kept inside the window.
    pub(super) fn menu_rect(&self, o: Point2) -> Option<(f32, f32, f32, f32)> {
        let (dx, dy) = self.ctl.menu?;
        let (vw, vh) = self.viewport;
        let h = self.menu_items() as f32 * MENU_ITEM_H + 10.;
        let x = (o.x + dx).min(vw - MENU_W - 4.).max(4.);
        let y = (o.y + dy).min(vh - h - 4.).max(4.);
        Some((x, y, MENU_W, h))
    }

    pub(super) fn render_menu(&mut self, o: Point2, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (x, y, w, _) = self.menu_rect(o)?;
        let theme = cx.theme().clone();
        let item = |id: &'static str, icon: IconName, label: String, cx: &mut Context<Self>, act: fn(&mut Companion, &mut Context<Companion>)| {
            h_flex()
                .id(id)
                .h(px(MENU_ITEM_H))
                .px_2()
                .gap_2()
                .rounded(theme.radius)
                .text_sm()
                .cursor_pointer()
                .hover(|s| s.bg(theme.accent).text_color(theme.accent_foreground))
                .child(Icon::new(icon).small().text_color(theme.muted_foreground))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.ctl.menu = None;
                    act(this, cx);
                    cx.notify();
                }))
        };
        let hide = if self.hidden {
            item("mori-menu-show", IconName::Eye, trf("{} einblenden", &[&self.name()]), cx, |this, cx| this.set_hidden(false, cx))
        } else {
            item("mori-menu-hide", IconName::EyeOff, trf("{} ausblenden", &[&self.name()]), cx, |this, cx| this.set_hidden(true, cx))
        };
        let place = if self.desktop_mode {
            item("mori-menu-window", IconName::AppWindow, tr("Zurück ins Fenster").into(), cx, |this, cx| {
                this.open = false;
                cx.emit(MoriEvent::BackToApp);
            })
        } else {
            item("mori-menu-desktop", IconName::MonitorUp, tr("Auf den Desktop").into(), cx, |this, cx| {
                this.open = false;
                cx.emit(MoriEvent::PopOut);
            })
        };
        let settings = item("mori-menu-settings", IconName::Settings, tr("Einstellungen").into(), cx, |_, cx| cx.emit(MoriEvent::OpenMoriSettings));
        Some(
            v_flex()
                .id("mori-menu")
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(w))
                .p_1()
                .rounded(theme.radius_lg)
                .border(ui::border_width(cx))
                .border_color(theme.border)
                .bg(theme.popover)
                .text_color(theme.popover_foreground)
                .shadow_lg()
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.ctl.menu = None;
                    cx.notify();
                }))
                .child(hide)
                .child(place)
                .child(settings)
                .into_any_element(),
        )
    }

    /// The record button, the starting spinner, or pause / resume, stop
    /// and the elapsed time. `place` is where a source choice opens (the
    /// bar shows it inline, the bubble as a popup).
    pub(super) fn render_record_controls(&mut self, place: ChoiceIn, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let p = match place {
            ChoiceIn::Bubble => "bubble",
            ChoiceIn::Bar => "bar",
        };
        let id = |name: &str| SharedString::from(format!("mori-{p}-{name}"));
        if let Some(live) = self.live(cx) {
            let paused = live.paused;
            let mic_on = live.sources[0];
            let color = if paused { theme.warning } else { theme.danger };
            // Her bar keeps it friendly: sparkles (or a spinner) and the
            // time, small and muted; the app and the tray keep the red dot.
            let calm = place == ChoiceIn::Bar && self.ctl.bar_indicator != BarIndicator::Dot;
            let spec = crate::themes::active(cx);
            let motion = spec.animations && !self.reduce_motion;
            let indicator = if calm {
                h_flex()
                    .gap_1p5()
                    .px_1()
                    .child(match self.ctl.bar_indicator {
                        BarIndicator::Spinner if !paused => gpui_kit::component::spinner::Spinner::new().xsmall().color(theme.primary).into_any_element(),
                        BarIndicator::Spinner => Icon::new(IconName::Pause).xsmall().text_color(theme.muted_foreground).into_any_element(),
                        _ => sparkles(theme.primary, motion, paused),
                    })
                    .child(
                        div()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .child(ui::hms(live.elapsed_secs)),
                    )
            } else { h_flex()
                .gap_1p5()
                .px_1()
                .child(if paused {
                    Icon::new(IconName::Pause).xsmall().text_color(color).into_any_element()
                } else {
                    div().size(px(9.)).rounded_full().bg(color).into_any_element()
                })
                .child(
                    div()
                        .text_sm()
                        .font_medium()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(if paused { theme.muted_foreground } else { theme.foreground })
                        .child(ui::hms(live.elapsed_secs)),
                ) };
            return h_flex()
                .gap_0p5()
                .child(indicator)
                .child(
                    Button::new(id("mic"))
                        .xsmall()
                        .ghost()
                        .icon(if mic_on {
                            Icon::new(IconName::Mic)
                        } else {
                            Icon::new(IconName::MicOff).text_color(theme.danger)
                        })
                        .tooltip(if mic_on { tr("Mikrofon ausschalten") } else { tr("Mikrofon einschalten") })
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(MoriEvent::SetSource { source: mostalgia_core::Source::Mic, on: !mic_on })
                        })),
                )
                .child(
                    Button::new(id("pause"))
                        .xsmall()
                        .ghost()
                        .icon(if paused { IconName::Play } else { IconName::Pause })
                        .tooltip(if paused { tr("Fortsetzen") } else { tr("Pause") })
                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(MoriEvent::SetPaused { paused: !paused }))),
                )
                .child(
                    Button::new(id("stop"))
                        .xsmall()
                        .ghost()
                        .icon(Icon::new(IconName::Square).text_color(if calm { theme.muted_foreground } else { theme.danger }))
                        .tooltip(tr("Aufnahme beenden"))
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(MoriEvent::StopRecording))),
                )
                .into_any_element();
        }
        if self.ctl.starting.is_some() {
            return h_flex()
                .gap_1p5()
                .px_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(gpui_kit::component::spinner::Spinner::new().xsmall())
                .child(tr("Aufnahme startet …"))
                .into_any_element();
        }
        if place == ChoiceIn::Bar && self.ctl.choice == Some(ChoiceIn::Bar) {
            return h_flex()
                .gap_0p5()
                .child(self.choice_buttons(p, cx))
                .child(Button::new(id("choice-cancel")).xsmall().ghost().icon(IconName::X).tooltip(tr("Abbrechen")).on_click(cx.listener(|this, _, _, cx| {
                    this.ctl.choice = None;
                    cx.notify();
                })))
                .into_any_element();
        }
        let open = self.ctl.choice == Some(place);
        Button::new(id("record"))
            .xsmall()
            .ghost()
            .selected(open)
            .child(div().size(px(10.)).rounded_full().bg(theme.danger))
            .tooltip(tr("Aufnahme starten"))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.ctl.choice = if this.ctl.choice == Some(place) { None } else { Some(place) };
                this.ctl.menu = None;
                cx.notify();
            }))
            .into_any_element()
    }

    /// The two source buttons: microphone and desktop audio, or desktop
    /// audio only.
    fn choice_buttons(&mut self, p: &str, cx: &mut Context<Self>) -> Div {
        h_flex()
            .gap_1()
            .child(
                Button::new(SharedString::from(format!("mori-{p}-rec-both")))
                    .small()
                    .outline()
                    .child(
                        h_flex()
                            .gap_0p5()
                            .child(Icon::new(IconName::Mic).small())
                            .child(div().text_xs().child("+"))
                            .child(Icon::new(IconName::Monitor).small()),
                    )
                    .tooltip(tr("Mikrofon + Desktop-Audio aufnehmen"))
                    .on_click(cx.listener(|this, _, _, cx| this.start_recording(true, true, cx))),
            )
            .child(
                Button::new(SharedString::from(format!("mori-{p}-rec-system")))
                    .small()
                    .outline()
                    .icon(IconName::Monitor)
                    .tooltip(tr("Nur Desktop-Audio aufnehmen"))
                    .on_click(cx.listener(|this, _, _, cx| this.start_recording(false, true, cx))),
            )
    }

    /// The source choice under the bubble's record button.
    pub(super) fn render_choice_popup(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.ctl.choice != Some(ChoiceIn::Bubble) || self.live(cx).is_some() || self.ctl.starting.is_some() {
            return None;
        }
        let theme = cx.theme().clone();
        let buttons = self.choice_buttons("bubble", cx);
        Some(
            v_flex()
                .id("mori-rec-choice")
                .absolute()
                .top(px(42.))
                .right(px(8.))
                .gap_1p5()
                .p_2()
                .rounded(theme.radius_lg)
                .border(ui::border_width(cx))
                .border_color(theme.border)
                .bg(theme.popover)
                .shadow_lg()
                .occlude()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    if this.ctl.choice == Some(ChoiceIn::Bubble) {
                        this.ctl.choice = None;
                        cx.notify();
                    }
                }))
                .child(div().text_xs().text_color(theme.muted_foreground).child(tr("Aufnehmen von:")))
                .child(buttons)
                .when(self.notes_mode != mostalgia_core::config::NotesMode::Off, |el| {
                    el.child(div().pt_1().text_xs().text_color(theme.muted_foreground).child(trf("Worauf soll {} achten? (optional)", &[&self.name()])))
                        .child(gpui_kit::component::input::Input::new(&self.focus_input).xsmall().w(px(240.)))
                })
                .into_any_element(),
        )
    }

    /// Notes for the Notes tab and the bar's counter: the recording's
    /// points while one runs, else the open meeting's saved notes. Oldest
    /// first (the newest is nearest to her: at the bottom).
    fn note_rows(&mut self, cx: &App) -> Result<Vec<NoteRow>, String> {
        if self.live(cx).is_some() {
            return Ok(self
                .points
                .iter()
                .filter(|p| !p.dismissed)
                .map(|p| NoteRow {
                    key: NoteKey::Live(p.id),
                    kind: Some(p.point.kind),
                    kind_raw: p.point.kind.as_str().into(),
                    text: p.point.text.clone(),
                    at: p.point.at_secs,
                    pinned: p.pinned,
                })
                .collect());
        }
        let Some((meeting_id, _)) = self.open_meeting.clone() else { return Ok(Vec::new()) };
        if self.ctl.saved.as_ref().is_none_or(|s| s.meeting_id != meeting_id) {
            let notes = Store::open(&mostalgia_core::paths::database()).and_then(|s| s.notes(meeting_id)).map_err(|e| format!("{e:#}"));
            self.ctl.saved = Some(SavedNotes { meeting_id, notes });
        }
        let saved = self.ctl.saved.as_ref().expect("just loaded");
        let mut notes = saved.notes.clone()?;
        notes.sort_by_key(|n| n.id);
        Ok(notes
            .into_iter()
            .map(|n| NoteRow {
                key: NoteKey::Saved { meeting_id, note_id: n.id },
                kind: PointKind::parse(&n.kind),
                kind_raw: n.kind,
                text: n.text,
                at: n.at_secs,
                pinned: n.pinned,
            })
            .collect())
    }

    /// Forgets the loaded saved notes (they're read again when shown).
    pub(super) fn invalidate_notes(&mut self) {
        self.ctl.saved = None;
    }

    pub(super) fn notes_count(&mut self, cx: &App) -> usize {
        self.note_rows(cx).map(|r| r.len()).unwrap_or(0)
    }

    pub(super) fn show_notes_tab(&mut self, cx: &mut Context<Self>) {
        self.ctl.notes_tab = true;
        self.invalidate_notes();
        self.ctl.notes_scroll.scroll_to_bottom();
        if !self.open {
            self.toggle_bubble(cx);
        }
        cx.notify();
    }

    fn toggle_note_pin(&mut self, key: NoteKey, pinned: bool, cx: &mut Context<Self>) {
        match key {
            NoteKey::Live(id) => {
                if let Some(p) = self.points.iter_mut().find(|p| p.id == id) {
                    p.pinned = !pinned;
                }
            }
            NoteKey::Saved { meeting_id, note_id } => {
                let result = Store::open(&mostalgia_core::paths::database()).and_then(|s| s.set_note_pinned(note_id, !pinned));
                self.after_note_change(meeting_id, result, cx);
            }
        }
        cx.notify();
    }

    fn delete_note(&mut self, key: NoteKey, cx: &mut Context<Self>) {
        match key {
            // Off the list and not highlighted (still saved with the meeting).
            NoteKey::Live(id) => {
                if let Some(p) = self.points.iter_mut().find(|p| p.id == id) {
                    p.dismissed = true;
                    p.pinned = false;
                }
            }
            NoteKey::Saved { meeting_id, note_id } => {
                let result = Store::open(&mostalgia_core::paths::database()).and_then(|s| s.delete_note(note_id).map(|_| ()));
                self.after_note_change(meeting_id, result, cx);
            }
        }
        cx.notify();
    }

    fn after_note_change(&mut self, meeting_id: i64, result: anyhow::Result<()>, cx: &mut Context<Self>) {
        self.invalidate_notes();
        match result {
            // The meeting screen shows them too.
            Ok(()) => cx.emit(MoriEvent::NotesSaved { meeting_id }),
            Err(e) => self.ctl.error = Some(trf("Die Notiz konnte nicht geändert werden: {}", &[&format!("{e:#}")])),
        }
    }

    fn open_note(&mut self, key: NoteKey, at: f64, cx: &mut Context<Self>) {
        match key {
            NoteKey::Live(_) => cx.emit(ShowLiveMoment { secs: at }),
            NoteKey::Saved { meeting_id, .. } => cx.emit(MoriEvent::OpenCitation { meeting_id, secs: at }),
        }
    }

    /// The Notes tab's content (the scrolling list).
    pub(super) fn render_notes_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let live = self.live(cx).is_some();
        let rows = self.note_rows(cx);
        let mut items: Vec<AnyElement> = Vec::new();
        let hint = |text: String| div().text_sm().text_color(theme.muted_foreground).child(text).into_any_element();
        match &rows {
            Err(e) => items.push(div().p_2().rounded(theme.radius).bg(theme.danger.opacity(0.1)).text_color(theme.danger).text_xs().child(e.clone()).into_any_element()),
            Ok(rows) if rows.is_empty() => items.push(hint(if live {
                trf("Noch keine Notizen – {} notiert Entscheidungen, Aufgaben, offene Fragen und Zahlen, während ihr sprecht.", &[&self.name()])
            } else if self.open_meeting.is_some() {
                tr("Diese Besprechung hat keine Notizen.").to_string()
            } else {
                tr("Hier stehen die Notizen der laufenden Aufnahme oder der geöffneten Besprechung.").to_string()
            })),
            Ok(_) => {}
        }
        for row in rows.unwrap_or_default() {
            items.push(self.render_note_row(row, live, cx));
        }
        // While recording: when she looks next, so a quiet interval isn't
        // a mystery.
        if let Some(status) = self.live(cx).and_then(|l| self.notes_status(&l)) {
            items.push(div().text_xs().text_color(theme.muted_foreground).child(status).into_any_element());
        }
        let source = if live {
            tr("Live-Besprechung").to_string()
        } else {
            self.open_meeting.as_ref().map(|(_, t)| t.clone()).unwrap_or_default()
        };
        v_flex()
            .size_full()
            .child(
                v_flex()
                    .id("mori-notes")
                    .size_full()
                    .px_3()
                    .py_2()
                    .gap_1p5()
                    .track_scroll(&self.ctl.notes_scroll)
                    .overflow_y_scroll()
                    .when(!source.is_empty(), |el| el.child(div().text_xs().text_color(theme.muted_foreground).truncate().child(source)))
                    .children(items),
            )
            .into_any_element()
    }

    /// The live notes' state for the Notes tab: a request running, or (with
    /// an interval) how much more speech until the next one.
    fn notes_status(&self, live: &LiveState) -> Option<String> {
        if self.notes_mode == NotesMode::Off || self.error.as_ref().is_some_and(|e| e.points) {
            return None;
        }
        if self.points_rx.is_some() {
            return Some(trf("{} sucht gerade nach Notizen …", &[&self.name()]));
        }
        match self.notes_mode {
            NotesMode::Every(secs) => {
                let left = (secs as f64 - (live.speech_secs - self.points_mark)).max(0.);
                Some(if left < 1. { tr("Nächste Notizen in Kürze").to_string() } else { trf("Nächste Notizen nach etwa {} s weiterer Sprache", &[&(left.ceil() as u64)]) })
            }
            _ => None,
        }
    }

    fn render_note_row(&mut self, row: NoteRow, live: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let id: SharedString = match row.key {
            NoteKey::Live(i) => format!("live-{i}"),
            NoteKey::Saved { note_id, .. } => format!("saved-{note_id}"),
        }
        .into();
        let color = row.kind.map_or(theme.muted_foreground, |k| super::points::kind_color(k, &theme));
        let label = row.kind.map_or(row.kind_raw.clone(), |k| k.label().to_string());
        let (key, at, pinned) = (row.key, row.at, row.pinned);
        let jump_tip: SharedString = if live { trf("{} – zu dieser Stelle springen", &[&ui::hms(at)]) } else { trf("{} – öffnen und ab hier abspielen", &[&ui::hms(at)]) }.into();
        v_flex()
            .id(SharedString::from(format!("mori-note-{id}")))
            .flex_shrink_0()
            .w_full()
            .px_2()
            .pt_1()
            .pb_1p5()
            .gap_0p5()
            .rounded(theme.radius_lg)
            .border(ui::border_width(cx))
            .border_color(if pinned { color } else { theme.border })
            .bg(theme.background)
            .cursor_pointer()
            .hover(|s| s.bg(theme.accent.opacity(0.5)))
            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(jump_tip.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _, _, cx| this.open_note(key, at, cx)))
            .child(
                h_flex()
                    .gap_1p5()
                    .text_xs()
                    .child(div().flex_shrink_0().size(px(7.)).rounded_full().bg(color))
                    .child(div().text_color(color).font_semibold().child(label))
                    .child(div().text_color(theme.muted_foreground).font_family(theme.mono_font_family.clone()).child(ui::hms(at)))
                    .child(div().flex_1())
                    .child(
                        div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(
                            Button::new(SharedString::from(format!("mori-note-pin-{id}")))
                                .xsmall()
                                .ghost()
                                .icon(Icon::new(IconName::Bookmark).text_color(if pinned { color } else { theme.muted_foreground }))
                                .tooltip(if pinned { tr("Lösen") } else { tr("Anheften") })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.toggle_note_pin(key, pinned, cx);
                                })),
                        ),
                    )
                    .child(
                        div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(
                            Button::new(SharedString::from(format!("mori-note-delete-{id}")))
                                .xsmall()
                                .ghost()
                                .icon(IconName::Trash)
                                .tooltip(if live { tr("Aus der Liste nehmen (bleibt nicht angeheftet in den Notizen der Besprechung)") } else { tr("Notiz löschen") })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.delete_note(key, cx);
                                })),
                        ),
                    ),
            )
            .child(ui::bidi_text(row.text, true).text_sm().line_height(relative(1.3)))
            .into_any_element()
    }

    /// The bar that stands in for her while she's hidden: record controls,
    /// the notes counter, the chat (expand) and the menu. Dragged like her.
    pub(super) fn render_bar(&mut self, o: Point2, dragging: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let recording = self.live(cx).is_some();
        let choosing = self.ctl.choice == Some(ChoiceIn::Bar);
        let busy = recording || choosing || self.ctl.starting.is_some();
        let controls = self.render_record_controls(ChoiceIn::Bar, cx);
        let notes = self.notes_count(cx);
        let open = self.open;
        // Buttons keep their press (no drag, no bubble toggle).
        let keep = |el: AnyElement| div().flex_none().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(el);
        h_flex()
            .id("mori-bar")
            .absolute()
            .left(px(o.x))
            .top(px(o.y))
            .w(px(BAR_W))
            .h(px(BAR_H))
            .pl_2()
            .pr_1()
            .gap_1()
            .rounded_full()
            .border(ui::border_width(cx))
            .border_color(theme.border)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .shadow_md()
            .when(!dragging, |el| el.cursor_grab())
            .when(dragging, |el| el.cursor_grabbing())
            .on_mouse_down(MouseButton::Left, cx.listener(|this, ev: &MouseDownEvent, _, cx| this.mouse_down(ev, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, cx| this.mouse_up(cx)))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, window, cx| this.mouse_move(ev, window, cx)))
            .on_mouse_down(MouseButton::Right, cx.listener(|this, ev: &MouseDownEvent, _, cx| this.open_menu(ev, cx)))
            .when(!busy, |el| el.child(div().flex_none().text_sm().font_semibold().child(self.name())))
            .child(keep(controls))
            .child(div().flex_1())
            .child(keep(
                Button::new("mori-bar-notes")
                    .xsmall()
                    .ghost()
                    .icon(IconName::StickyNote)
                    .label(format!("{notes}"))
                    .selected(open && self.ctl.notes_tab)
                    .tooltip(trf("{} Notizen – anzeigen", &[&notes]))
                    .on_click(cx.listener(|this, _, _, cx| this.show_notes_tab(cx)))
                    .into_any_element(),
            ))
            .child(keep(
                Button::new("mori-bar-expand")
                    .xsmall()
                    .ghost()
                    .icon(if open { IconName::ChevronUp } else { IconName::ChevronDown })
                    .tooltip(if open { tr("Chat schließen") } else { tr("Chat öffnen") })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_bubble(cx)))
                    .into_any_element(),
            ))
            .child(keep(
                Button::new("mori-bar-more")
                    .xsmall()
                    .ghost()
                    .icon(IconName::Ellipsis)
                    .tooltip(tr("Menü"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.ctl.menu = if this.ctl.menu.is_some() { None } else { Some((BAR_W - MENU_W, BAR_H + 4.)) };
                        this.ctl.choice = None;
                        cx.notify();
                    }))
                    .into_any_element(),
            ))
            .into_any_element()
    }

    /// Shows this bar indicator without saving it (screenshots).
    #[doc(hidden)]
    pub fn demo_bar_indicator(&mut self, indicator: BarIndicator, cx: &mut Context<Self>) {
        self.ctl.bar_indicator = indicator;
        cx.notify();
    }

    /// Hides or shows her figure without saving it (screenshots, demo).
    #[doc(hidden)]
    pub fn demo_hidden(&mut self, hidden: bool, cx: &mut Context<Self>) {
        self.hidden = hidden;
        self.ctl.menu = None;
        cx.notify();
    }

    /// Opens her menu at this offset from her top-left (screenshots).
    #[doc(hidden)]
    pub fn demo_menu(&mut self, at: Option<(f32, f32)>, cx: &mut Context<Self>) {
        self.ctl.menu = at;
        cx.notify();
    }

    /// Opens the bubble's source choice, or the bar's (screenshots).
    #[doc(hidden)]
    pub fn demo_record_choice(&mut self, bar: bool, cx: &mut Context<Self>) {
        if !bar && !self.open {
            self.toggle_bubble(cx);
        }
        self.ctl.choice = Some(if bar { ChoiceIn::Bar } else { ChoiceIn::Bubble });
        cx.notify();
    }

    /// Opens the bubble on the Notes tab, or the chat (screenshots).
    #[doc(hidden)]
    pub fn demo_notes_tab(&mut self, notes: bool, cx: &mut Context<Self>) {
        if notes {
            self.show_notes_tab(cx);
        } else {
            self.ctl.notes_tab = false;
            cx.notify();
        }
    }

    /// Shows a failed start in the bubble (screenshots).
    #[doc(hidden)]
    pub fn demo_start_failed(&mut self, message: &str, cx: &mut Context<Self>) {
        self.ctl.starting = Some(Instant::now());
        self.ctl.choice = None;
        self.recording_start_failed(message.to_string(), cx);
    }

    /// Closes the bubble (screenshots).
    #[doc(hidden)]
    pub fn demo_close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.ctl.choice = None;
        cx.notify();
    }
}
