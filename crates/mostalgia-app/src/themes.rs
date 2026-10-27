//! App themes as data. Each theme is a [`ThemeSpec`] (colours as hex,
//! corner radius, border width, fonts, speaker-chip palette). The built-in
//! ones live in `assets/themes.json`; more can be dropped into
//! `~/.config/mostalgia/themes/*.json` (same shape, one theme or a list).

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;
use serde::Deserialize;

/// "Follow GNOME": light or dark depending on the system setting.
pub const SYSTEM: &str = "system";

#[derive(Clone, Debug, Deserialize)]
pub struct ThemeSpec {
    pub id: String,
    /// German display name.
    pub name: String,
    /// English display name (falls back to `name`).
    #[serde(default)]
    pub name_en: Option<String>,
    #[serde(default)]
    pub dark: bool,
    pub background: String,
    pub foreground: String,
    /// Cards, inputs, popovers.
    pub surface: String,
    pub sidebar: String,
    pub border: String,
    pub muted: String,
    pub muted_foreground: String,
    /// The accent: primary buttons, focus, the "Ich" chip.
    pub primary: String,
    pub primary_foreground: String,
    pub danger: String,
    pub warning: String,
    pub success: String,
    #[serde(default = "default_radius")]
    pub radius: f32,
    #[serde(default = "default_border")]
    pub border_width: f32,
    #[serde(default = "default_font")]
    pub font: String,
    #[serde(default = "default_mono")]
    pub mono_font: String,
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    /// Speaker chip colours: "Ich", "Andere", then named speakers in turn.
    pub speakers: Vec<String>,
    /// Bouncier, cartoon-like motion (pop-ins, bobbing icons).
    #[serde(default)]
    pub playful: bool,
    /// Any motion at all; off for the high-contrast theme.
    #[serde(default = "default_true")]
    pub animations: bool,
    /// Ambient illustration behind the empty live transcript: "grid",
    /// "blocks", "comic", "stars", "aurora", "waves", "leaves", "steam",
    /// "petals" or "memphis".
    #[serde(default)]
    pub scene: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_radius() -> f32 {
    8.
}
fn default_border() -> f32 {
    1.
}
fn default_font() -> String {
    "Inter".into()
}
fn default_mono() -> String {
    "DejaVu Sans Mono".into()
}
fn default_font_size() -> f32 {
    14.
}

impl ThemeSpec {
    /// The name in the current app language.
    pub fn display_name(&self) -> &str {
        match (mostalgia_core::i18n::lang(), &self.name_en) {
            (mostalgia_core::i18n::Lang::En, Some(en)) => en,
            _ => &self.name,
        }
    }
}

/// The theme in effect, for the bits gpui-component's theme doesn't cover
/// (border width, speaker colours).
#[derive(Clone)]
pub struct ActiveSpec(pub ThemeSpec);

impl Global for ActiveSpec {}

pub fn hex(s: &str) -> Hsla {
    let s = s.trim_start_matches('#');
    let v = u32::from_str_radix(s, 16).unwrap_or(0xff00ff);
    if s.len() == 8 {
        rgba(v).into()
    } else {
        rgb(v).into()
    }
}

/// Built-in themes followed by the user's own.
pub fn all() -> Vec<ThemeSpec> {
    let mut themes: Vec<ThemeSpec> =
        serde_json::from_str(include_str!("../assets/themes.json")).expect("valid built-in themes");
    let dir = mostalgia_core::config::Config::path().with_file_name("themes");
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut files: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        files.sort();
        for path in files.into_iter().filter(|p| p.extension().is_some_and(|e| e == "json")) {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let parsed = serde_json::from_str::<Vec<ThemeSpec>>(&text)
                .or_else(|_| serde_json::from_str::<ThemeSpec>(&text).map(|t| vec![t]));
            match parsed {
                Ok(user) => {
                    for t in user {
                        themes.retain(|b| b.id != t.id);
                        themes.push(t);
                    }
                }
                Err(e) => eprintln!("[mostalgia] ignoring theme {}: {e}", path.display()),
            }
        }
    }
    themes
}

/// The theme to use for a config value; "system" picks light or dark.
pub fn resolve(id: &str, system_dark: bool) -> ThemeSpec {
    let themes = all();
    let id = if id == SYSTEM { if system_dark { "dunkel" } else { "hell" } } else { id };
    themes
        .iter()
        .find(|t| t.id == id)
        .or_else(|| themes.first())
        .cloned()
        .expect("at least one theme")
}

pub fn apply(spec: &ThemeSpec, cx: &mut App) {
    Theme::change(if spec.dark { ThemeMode::Dark } else { ThemeMode::Light }, None, cx);
    let bg = hex(&spec.background);
    let fg = hex(&spec.foreground);
    let surface = hex(&spec.surface);
    let sidebar = hex(&spec.sidebar);
    let border = hex(&spec.border);
    let muted = hex(&spec.muted);
    let muted_fg = hex(&spec.muted_foreground);
    let primary = hex(&spec.primary);
    let primary_fg = hex(&spec.primary_foreground);
    let danger = hex(&spec.danger);
    let warning = hex(&spec.warning);
    let success = hex(&spec.success);
    // Text on a solid colour: black or white, whichever reads better.
    let on = |c: Hsla| if c.l > 0.6 { hsla(0., 0., 0.06, 1.) } else { hsla(0., 0., 1., 1.) };
    let hover = |c: Hsla| c.opacity(0.88);
    let active = |c: Hsla| c.opacity(0.76);

    Theme::update(cx, |t| {
        t.font_family = spec.font.clone().into();
        t.mono_font_family = spec.mono_font.clone().into();
        t.font_size = px(spec.font_size);
        t.radius = px(spec.radius);
        t.radius_lg = px(spec.radius * 1.5);
        t.shadow = spec.radius > 0.;
        let c = &mut t.colors;
        c.background = bg;
        c.foreground = fg;
        c.border = border;
        c.input = border;
        c.ring = primary;
        c.caret = primary;
        c.selection = primary.opacity(0.3);
        c.link = primary;
        c.link_hover = hover(primary);
        c.link_active = active(primary);
        c.muted = muted;
        c.muted_foreground = muted_fg;
        c.accent = muted;
        c.accent_foreground = fg;
        c.secondary = muted;
        c.secondary_hover = hover(muted);
        c.secondary_active = active(muted);
        c.secondary_foreground = fg;
        c.primary = primary;
        c.primary_hover = hover(primary);
        c.primary_active = active(primary);
        c.primary_foreground = primary_fg;
        c.button = surface;
        c.button_hover = muted;
        c.button_active = active(muted);
        c.button_foreground = fg;
        c.button_primary = primary;
        c.button_primary_hover = hover(primary);
        c.button_primary_active = active(primary);
        c.button_primary_foreground = primary_fg;
        c.button_secondary = muted;
        c.button_secondary_hover = hover(muted);
        c.button_secondary_active = active(muted);
        c.button_secondary_foreground = fg;
        c.danger = danger;
        c.danger_hover = hover(danger);
        c.danger_active = active(danger);
        c.button_danger = danger;
        c.button_danger_hover = hover(danger);
        c.button_danger_active = active(danger);
        c.danger_foreground = on(danger);
        c.button_danger_foreground = on(danger);
        c.warning_foreground = on(warning);
        c.success_foreground = on(success);
        c.warning = warning;
        c.warning_hover = hover(warning);
        c.warning_active = active(warning);
        c.success = success;
        c.success_hover = hover(success);
        c.success_active = active(success);
        c.popover = surface;
        c.popover_foreground = fg;
        c.sidebar = sidebar;
        c.sidebar_foreground = fg;
        c.sidebar_border = border;
        c.sidebar_accent = muted;
        c.sidebar_accent_foreground = fg;
        c.sidebar_primary = primary;
        c.sidebar_primary_foreground = primary_fg;
        c.title_bar = sidebar;
        c.title_bar_border = border;
        c.window_border = border;
        c.tab_bar = bg;
        c.tab = bg;
        c.tab_active = bg;
        c.tab_foreground = muted_fg;
        c.tab_active_foreground = fg;
        c.switch = border;
        c.scrollbar_thumb = border;
        c.scrollbar_thumb_hover = muted_fg;
        c.list_hover = muted;
        c.list_active = primary.opacity(0.15);
        c.list_active_border = primary;
    });
    cx.set_global(ActiveSpec(spec.clone()));
}

/// The active theme's spec (falls back to the light theme before setup).
pub fn active(cx: &App) -> ThemeSpec {
    cx.try_global::<ActiveSpec>()
        .map(|s| s.0.clone())
        .unwrap_or_else(|| resolve("hell", false))
}
