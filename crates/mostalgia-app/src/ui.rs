//! Look and shared bits: theme, speaker colours, level meters, formatting.

use gpui_kit::component::{ActiveTheme, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mostalgia_core::Source;
use mostalgia_core::i18n::{tr, trf};

use crate::themes;

/// Applies the configured theme; "system" follows GNOME's light/dark. A
/// chosen UI font overrides the theme's, and themes on the old default mono
/// font get JetBrains Mono for the timer and timestamps.
/// The configured text sizes are applied here too (Settings → Appearance,
/// Ctrl+= / Ctrl+- / Ctrl+0), so they survive theme changes.
pub fn apply_theme(window: &mut Window, cx: &mut App) {
    let dark = matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
    let config = mostalgia_core::config::Config::load();
    set_text_scales(&config);
    let mut spec = themes::resolve(&config.theme, dark);
    if !config.font.is_empty() {
        spec.font = config.font.clone();
    }
    if spec.mono_font == "DejaVu Sans Mono" {
        spec.mono_font = MONO_FONT.into();
    }
    themes::apply(&spec, cx);
    cx.refresh_windows();
}

use std::sync::atomic::{AtomicU32, Ordering};

/// App-wide and transcript text size as f32 bits (0 = not read yet).
static UI_SCALE: AtomicU32 = AtomicU32::new(0);
static TRANSCRIPT_SCALE: AtomicU32 = AtomicU32::new(0);

/// Takes the text sizes from `config` (done by [`apply_theme`]).
pub fn set_text_scales(config: &mostalgia_core::config::Config) {
    UI_SCALE.store(config.ui_scale().to_bits(), Ordering::Relaxed);
    TRANSCRIPT_SCALE.store(config.transcript_scale().to_bits(), Ordering::Relaxed);
}

fn scale_of(cell: &AtomicU32) -> f32 {
    if cell.load(Ordering::Relaxed) == 0 {
        set_text_scales(&mostalgia_core::config::Config::load());
    }
    f32::from_bits(cell.load(Ordering::Relaxed))
}

/// The app-wide text size (1.0 = 100 %). Text and rem-based spacing follow
/// it through the theme's rem size; fixed pixel widths that hold text
/// (Mori's bubble, note cards …) multiply by it.
pub fn ui_scale() -> f32 {
    scale_of(&UI_SCALE)
}

/// The transcript's own text size on top of the app's (1.0 = 100 %).
pub fn transcript_scale() -> f32 {
    scale_of(&TRANSCRIPT_SCALE)
}

/// Text size for transcript and notes text: `base` rems (1.0 = the body
/// size) times the transcript scale.
pub fn transcript_rems(base: f32) -> Rems {
    rems(base * transcript_scale())
}

pub const MONO_FONT: &str = "JetBrains Mono";

/// Fonts selectable in the settings: (family, label). "" = theme default.
pub const UI_FONTS: [(&str, &str); 4] = [
    ("", "Theme-Standard"),
    ("Inter", "Inter"),
    ("IBM Plex Sans", "IBM Plex Sans"),
    ("Atkinson Hyperlegible", "Atkinson Hyperlegible"),
];

/// Fonts tried for characters the main font lacks (Arabic script).
pub fn font_fallbacks() -> FontFallbacks {
    FontFallbacks::from_fonts(vec!["Noto Sans Arabic".into()])
}

/// The theme's UI font with the fallback chain, for the window root (it
/// cascades to everything below).
pub fn ui_font(cx: &App) -> Font {
    Font {
        family: cx.theme().font_family.clone(),
        fallbacks: Some(font_fallbacks()),
        ..font(cx.theme().font_family.clone())
    }
}

/// Whether text reads right to left: its first strong letter is Arabic or
/// Hebrew script.
pub fn is_rtl(text: &str) -> bool {
    text.chars()
        .find(|c| c.is_alphabetic())
        .is_some_and(|c| matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF))
}

fn is_ltr_word(word: &str) -> bool {
    word.chars().find(|c| c.is_alphanumeric()).is_some_and(|c| !is_rtl(&c.to_string()))
}

/// Text that may be right-to-left. gpui shapes each word correctly but
/// lays words out left-to-right, so an Arabic sentence would read
/// backwards. For RTL text the words go into a right-to-left wrapping flow
/// instead, with embedded left-to-right runs (English, numbers) kept in
/// their own order. Left-to-right text is a plain div.
pub fn bidi_text(text: impl Into<SharedString>, wrap: bool) -> Div {
    let text: SharedString = text.into();
    if !is_rtl(&text) {
        return div().child(text);
    }
    // Logical word order, with each LTR run reversed so the reversed flow
    // shows it left-to-right.
    let mut words: Vec<String> = Vec::new();
    let mut ltr_run: Vec<String> = Vec::new();
    for w in text.split_whitespace() {
        if is_ltr_word(w) {
            ltr_run.push(w.to_string());
        } else {
            words.extend(ltr_run.drain(..).rev());
            words.push(w.to_string());
        }
    }
    words.extend(ltr_run.drain(..).rev());
    h_flex()
        .w_full()
        .flex_row_reverse()
        .gap_x(px(4.))
        .when(wrap, |el| el.flex_wrap())
        .when(!wrap, |el| el.overflow_hidden())
        .children(words.into_iter().map(|w| div().flex_none().child(w)))
}

/// A small Markdown renderer for right-to-left summaries (headings,
/// bullets, checkboxes, paragraphs; `**bold**` markers dropped), since the
/// Markdown view lays RTL words out left-to-right.
pub fn rtl_markdown(markdown: &str, cx: &App) -> Div {
    let theme = cx.theme();
    let mut out = v_flex().w_full().gap_2();
    for line in markdown.lines().map(str::trim_end) {
        let clean = line.replace("**", "");
        out = out.child(if let Some(h) = clean.strip_prefix("## ") {
            bidi_text(h.to_string(), true).mt_3().text_xl().font_semibold()
        } else if let Some(h) = clean.strip_prefix("# ") {
            bidi_text(h.to_string(), true).text_2xl().font_semibold()
        } else if let Some(item) = clean.strip_prefix("- [ ] ") {
            h_flex()
                .flex_row_reverse()
                .items_start()
                .gap_2()
                .child(div().mt_1().size(px(14.)).flex_none().rounded(px(3.)).border_1().border_color(theme.border))
                .child(bidi_text(item.to_string(), true).flex_1())
        } else if let Some(item) = clean.strip_prefix("- ") {
            h_flex()
                .flex_row_reverse()
                .items_start()
                .gap_2()
                .child(div().mt(px(9.)).size(px(5.)).flex_none().rounded_full().bg(theme.muted_foreground))
                .child(bidi_text(item.to_string(), true).flex_1())
        } else if clean.trim().is_empty() {
            div().h(px(4.))
        } else {
            bidi_text(clean, true)
        });
    }
    out
}

/// Border width of cards and outlined rows in the active theme.
pub fn border_width(cx: &App) -> Pixels {
    px(themes::active(cx).border_width)
}

/// Colour for a speaker label from the theme's palette: "Ich", "Andere",
/// then "Sprecher N" in order; other names get a stable pick.
pub fn speaker_color(label: &str, cx: &App) -> Hsla {
    let palette = themes::active(cx).speakers;
    let pick = |i: usize| themes::hex(&palette[i % palette.len()]);
    match label {
        "Ich" => pick(0),
        "Andere" => pick(1),
        other => match other.strip_prefix("Sprecher ").and_then(|n| n.parse::<usize>().ok()) { // i18n: not UI
            Some(n) => pick(n + 1),
            None => {
                let h = other.bytes().fold(7usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize));
                pick(2 + h % palette.len().saturating_sub(2).max(1))
            }
        },
    }
}

pub fn source_label(source: Source) -> &'static str {
    match source {
        Source::Mic => tr("Mikrofon"),
        Source::System => tr("Desktop"),
    }
}

/// How a stored speaker label is shown: "Ich"/"Andere"/"Sprecher N" are
/// data (kept in German in the database) and get translated for display;
/// names people gave stay as they are.
pub fn speaker_display(label: &str) -> String {
    mostalgia_core::i18n::speaker(label)
}

/// A speaker "chip": a small coloured dot plus the name.
pub fn speaker_chip(label: &str, cx: &App) -> Div {
    let color = speaker_color(label, cx);
    h_flex()
        .gap_1p5()
        .flex_none()
        .px_2()
        .py_0p5()
        .rounded(cx.theme().radius * 2.)
        .bg(color.opacity(0.12))
        .text_color(color)
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .child(div().size(px(6.)).rounded_full().bg(color))
        .child(SharedString::from(speaker_display(label)))
}

/// RMS → 0..1 on a −60…0 dBFS scale.
pub fn meter_level(rms: f32) -> f32 {
    if rms <= 0.0 {
        return 0.0;
    }
    ((20.0 * rms.log10() + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// A horizontal level meter.
/// A bookmark row in a transcript: a marked moment (with a rule across
/// when `rule`), or a source switched off / on mid-recording (crossed-out
/// icon while off).
pub fn mark_row(b: &mostalgia_core::store::Bookmark, rule: bool, cx: &App) -> Div {
    use gpui_kit::assets::IconName;
    use gpui_kit::component::{Icon, Sizable};
    use mostalgia_core::store::MarkKind;
    let theme = cx.theme();
    let (icon, color) = match b.kind {
        MarkKind::Moment => (IconName::Bookmark, theme.warning),
        MarkKind::SourceOff(Source::Mic) => (IconName::MicOff, theme.muted_foreground),
        MarkKind::SourceOn(Source::Mic) => (IconName::Mic, theme.muted_foreground),
        MarkKind::SourceOff(Source::System) => (IconName::MonitorOff, theme.muted_foreground),
        MarkKind::SourceOn(Source::System) => (IconName::Monitor, theme.muted_foreground),
    };
    h_flex()
        .gap_3()
        .py_1()
        .text_size(transcript_rems(0.75))
        .text_color(color)
        .child(div().w(transcript_rems(3.75)).flex_none().font_family(theme.mono_font_family.clone()).child(hms(b.at_secs)))
        .child(Icon::new(icon).xsmall())
        .child(b.display_label())
        .when(rule, |el| el.child(div().flex_1().h(px(1.)).bg(color.opacity(0.3))))
        // In a saved meeting, in line with the transcript rows' padding.
        .when(!rule, |el| el.px_2())
}

pub fn level_meter(level: f32, active: bool, cx: &App) -> Div {
    let theme = cx.theme();
    let color = if level > 0.92 { theme.danger } else { theme.primary };
    div()
        .h(px(6.))
        .w_full()
        .rounded(theme.radius)
        .bg(theme.muted)
        .child(
            div()
                .h_full()
                .rounded(theme.radius)
                .bg(if active { color } else { theme.muted_foreground.opacity(0.3) })
                .w(relative(if active { level } else { 0. })),
        )
}

pub fn hms(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

pub fn minutes(secs: f64) -> String {
    let m = (secs / 60.0).round() as u64;
    if m == 0 { tr("< 1 min").into() } else { trf("{} min", &[&m]) }
}

/// Section heading used in settings and the detail view.
pub fn section_title(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

/// A rounded card surface.
pub fn card(cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .p_4()
        .gap_3()
        .rounded(theme.radius_lg)
        .border(border_width(cx))
        .border_color(theme.border)
        .bg(theme.background)
}
