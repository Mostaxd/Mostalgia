//! Settings, grouped into categories (list on the left, a responsive
//! card grid on the right) with a search across all of them.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Sizable, StyledExt, h_flex, switch::Switch, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use gpui_kit::component::menu::DropdownMenu;
use schemars::JsonSchema;
use serde::Deserialize;
use mostalgia_core::audio::{self, SourceInfo};
use mostalgia_core::config::Config;
use mostalgia_core::{paths, summary};

use crate::{themes, ui};

/// Pick a summary model (from the model dropdown).
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = mostalgia, no_json)]
pub struct SelectSummaryModel(pub String);

pub enum SettingsEvent {
    Changed,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Mic,
    System,
}

pub struct SettingsView {
    config: Config,
    sources: Option<Result<Vec<SourceInfo>, String>>,
    /// Result of the last device test, per kind.
    test: [Option<Result<f32, String>>; 2],
    testing: Option<Kind>,
    api_key: Entity<InputState>,
    key_status: SharedString,
    /// Models of the selected provider (live if a key is set, else built-in).
    models: Vec<summary::ModelInfo>,
    models_live: bool,
    gpu: Option<String>,
    scroll: ScrollHandle,
    category: Category,
    search: Entity<InputState>,
    query: String,
    /// GNOME shortcut currently bound to `mostalgia toggle`, if any.
    shortcut: Option<String>,
    /// Whether a StatusNotifier host (the tray) is running.
    tray_available: Option<bool>,
    /// Also list unplugged ports in the device pickers.
    show_all_devices: bool,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Category {
    Aufnahme,
    Sprecher,
    Ki,
    Darstellung,
    Speicher,
    System,
}

impl Category {
    const ALL: [Category; 6] = [
        Category::Aufnahme,
        Category::Sprecher,
        Category::Ki,
        Category::Darstellung,
        Category::Speicher,
        Category::System,
    ];

    fn id(self) -> &'static str {
        match self {
            Category::Aufnahme => "aufnahme",
            Category::Sprecher => "sprecher",
            Category::Ki => "ki",
            Category::Darstellung => "darstellung",
            Category::Speicher => "speicher",
            Category::System => "system",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Category::Aufnahme => "Aufnahme",
            Category::Sprecher => "Sprecher",
            Category::Ki => "KI & Zusammenfassung",
            Category::Darstellung => "Darstellung",
            Category::Speicher => "Speicher & Export",
            Category::System => "Tastenkürzel & System",
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            Category::Aufnahme => "Geräte, Sprache und Spracherkennung",
            Category::Sprecher => "Wer hat was gesagt",
            Category::Ki => "Anbieter, Modell und Denktiefe für Zusammenfassungen",
            Category::Darstellung => "Design, Schrift und Animationen",
            Category::Speicher => "Wo Aufnahmen liegen und wie exportiert wird",
            Category::System => "Tastenkürzel, Tray-Symbol und Systeminfo",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Category::Aufnahme => IconName::Mic,
            Category::Sprecher => IconName::Users,
            Category::Ki => IconName::Sparkles,
            Category::Darstellung => IconName::Palette,
            Category::Speicher => IconName::FolderOpen,
            Category::System => IconName::Keyboard,
        }
    }

    fn parse(id: &str) -> Self {
        Self::ALL.into_iter().find(|c| c.id() == id).unwrap_or(Category::Aufnahme)
    }
}

/// One settings card: where it belongs, what search finds it by, and
/// whether it spans the whole grid.
struct Card {
    category: Category,
    title: &'static str,
    keywords: &'static str,
    wide: bool,
    element: AnyElement,
}

const SHORTCUT_PATH: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/mostalgia/";
const MEDIA_KEYS: &str = "org.gnome.settings-daemon.plugins.media-keys";

fn gsettings(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("gsettings").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The key combination bound to `mostalgia toggle`, if the shortcut exists.
fn read_shortcut() -> Option<String> {
    let list = gsettings(&["get", MEDIA_KEYS, "custom-keybindings"])?;
    if !list.contains(SHORTCUT_PATH) {
        return None;
    }
    let schema = format!("{MEDIA_KEYS}.custom-keybinding:{SHORTCUT_PATH}");
    let binding = gsettings(&["get", &schema, "binding"])?;
    let binding = binding.trim_matches('\'');
    (!binding.is_empty()).then(|| binding.to_string())
}

/// Binds Super+Shift+R to `mostalgia toggle` (the CLI next to this app).
fn install_shortcut() -> anyhow::Result<String> {
    let cli = std::env::current_exe()?.with_file_name("mostalgia");
    anyhow::ensure!(cli.exists(), "mostalgia (CLI) nicht neben der App gefunden: {}", cli.display());
    let list = gsettings(&["get", MEDIA_KEYS, "custom-keybindings"]).unwrap_or_default();
    if !list.contains(SHORTCUT_PATH) {
        let new = if list.is_empty() || list == "@as []" || list == "[]" {
            format!("['{SHORTCUT_PATH}']")
        } else {
            format!("{}, '{SHORTCUT_PATH}']", list.trim_end_matches(']'))
        };
        gsettings(&["set", MEDIA_KEYS, "custom-keybindings", &new]).ok_or_else(|| anyhow::anyhow!("gsettings fehlgeschlagen"))?;
    }
    let schema = format!("{MEDIA_KEYS}.custom-keybinding:{SHORTCUT_PATH}");
    let binding = "<Super><Shift>r";
    for (key, value) in [
        ("name", "mostalgia: Aufnahme starten/stoppen".to_string()),
        ("command", format!("{} toggle", cli.display())),
        ("binding", binding.to_string()),
    ] {
        gsettings(&["set", &schema, key, &value]).ok_or_else(|| anyhow::anyhow!("gsettings fehlgeschlagen"))?;
    }
    Ok(binding.into())
}

fn tray_host_running() -> bool {
    std::process::Command::new("gdbus")
        .args([
            "call", "--session", "--dest", "org.freedesktop.DBus", "--object-path", "/org/freedesktop/DBus",
            "--method", "org.freedesktop.DBus.NameHasOwner", "org.kde.StatusNotifierWatcher",
        ])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("true"))
        .unwrap_or(false)
}

/// "<Super><Shift>r" → "Super + Shift + R".
fn pretty_binding(b: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut rest = b;
    while let Some(start) = rest.find('<') {
        let end = rest[start..].find('>').map(|e| start + e).unwrap_or(rest.len() - 1);
        parts.push(rest[start + 1..end].to_string());
        rest = &rest[end + 1..];
    }
    if !rest.is_empty() {
        parts.push(rest.to_uppercase());
    }
    parts.join(" + ")
}

impl SettingsView {
    pub fn new(gpu: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let api_key = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("sk-ant-…"));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Einstellungen durchsuchen"));
        let sub = cx.subscribe_in(&search, window, |this, input, event: &gpui_kit::component::input::InputEvent, _, cx| {
            if let gpui_kit::component::input::InputEvent::Change = event {
                this.query = input.read(cx).value().to_string();
                cx.notify();
            }
        });
        let config = Config::load();
        let category = Category::parse(&config.settings_category);
        let mut this = Self {
            config,
            sources: None,
            test: [None, None],
            testing: None,
            api_key,
            key_status: SharedString::default(),
            models: Vec::new(),
            models_live: false,
            gpu,
            scroll: ScrollHandle::new(),
            category,
            search,
            query: String::new(),
            shortcut: None,
            tray_available: None,
            show_all_devices: false,
            _subscriptions: vec![sub],
        };
        this.load_system_status(cx);
        this.refresh_key_status();
        this.load_models(cx);
        this.load_sources(cx);
        this
    }

    fn load_system_status(&mut self, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async move { (read_shortcut(), tray_host_running()) });
        cx.spawn(async move |this, cx| {
            let (shortcut, tray) = task.await;
            this.update(cx, |this, cx| {
                this.shortcut = shortcut;
                this.tray_available = Some(tray);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Opens a category by id ("aufnahme", "sprecher", "ki", "darstellung",
    /// "speicher", "system").
    pub fn show_category(&mut self, id: &str, cx: &mut Context<Self>) {
        self.set_category(Category::parse(id), cx);
    }

    pub const CATEGORY_IDS: [&'static str; 6] = ["aufnahme", "sprecher", "ki", "darstellung", "speicher", "system"];

    fn set_category(&mut self, category: Category, cx: &mut Context<Self>) {
        self.category = category;
        self.config.settings_category = category.id().into();
        if let Err(e) = self.config.save() {
            eprintln!("[mostalgia] saving settings: {e:#}");
        }
        self.scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    fn refresh_key_status(&mut self) {
        let provider = self.config.summary_provider;
        self.key_status = match summary::api_key(provider) {
            Ok(Some(_)) if summary::key_from_env(provider) => "Aus der Umgebungsvariable".into(),
            Ok(Some(_)) => "Im GNOME-Schlüsselbund gespeichert".into(),
            Ok(None) => format!("Kein {}-Schlüssel hinterlegt — Zusammenfassungen sind aus", provider.name()).into(),
            Err(e) => format!("Schlüsselbund nicht erreichbar: {e:#}").into(),
        };
    }

    /// Shows the built-in model list now and swaps in the live one if the
    /// provider's API answers.
    fn load_models(&mut self, cx: &mut Context<Self>) {
        let provider = self.config.summary_provider;
        self.models = summary::fallback_models(provider);
        self.models_live = false;
        let task = cx.background_spawn(async move {
            let key = summary::api_key(provider).ok().flatten()?;
            summary::list_models(provider, &key).ok()
        });
        cx.spawn(async move |this, cx| {
            if let Some(models) = task.await {
                this.update(cx, |this, cx| {
                    if this.config.summary_provider == provider {
                        this.models = models;
                        this.models_live = true;
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn set_provider(&mut self, provider: summary::Provider, cx: &mut Context<Self>) {
        if self.config.summary_provider == provider {
            return;
        }
        self.config.summary_provider = provider;
        self.config.summary_model = provider.default_model().into();
        self.refresh_key_status();
        self.load_models(cx);
        self.save(cx);
    }

    fn select_model(&mut self, action: &SelectSummaryModel, _: &mut Window, cx: &mut Context<Self>) {
        self.config.summary_model = action.0.clone();
        // Keep the thinking level valid for the new model.
        if let Some(m) = self.models.iter().find(|m| m.id == action.0) {
            if !m.efforts.is_empty() && !m.efforts.contains(&self.config.summary_effort) {
                self.config.summary_effort = if m.efforts.iter().any(|e| e == "high") { "high".into() } else { m.efforts.last().cloned().unwrap_or_default() };
            }
        }
        self.save(cx);
    }

    fn load_sources(&mut self, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async move { audio::list_sources().map_err(|e| format!("{e:#}")) });
        cx.spawn(async move |this, cx| {
            let sources = task.await;
            this.update(cx, |this, cx| {
                this.sources = Some(sources);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = self.config.save() {
            eprintln!("[mostalgia] saving settings: {e:#}");
        }
        cx.emit(SettingsEvent::Changed);
        cx.notify();
    }

    fn test_device(&mut self, kind: Kind, cx: &mut Context<Self>) {
        if self.testing.is_some() {
            return;
        }
        self.testing = Some(kind);
        let device = match kind {
            Kind::Mic => self.config.mic_device.clone(),
            Kind::System => Some(self.config.system_device.clone().unwrap_or_else(|| "@DEFAULT_MONITOR@".into())),
        };
        cx.notify();
        let task = cx.background_spawn(async move { audio::measure_level(device.as_deref(), 3.0).map_err(|e| format!("{e:#}")) });
        cx.spawn(async move |this, cx| {
            let level = task.await;
            this.update(cx, |this, cx| {
                this.test[kind as usize] = Some(level);
                this.testing = None;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn save_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.api_key.read(cx).value().to_string();
        if key.trim().is_empty() {
            return;
        }
        match summary::save_api_key(self.config.summary_provider, &key) {
            Ok(()) => {
                self.api_key.update(cx, |s, cx| s.set_value("", window, cx));
                self.refresh_key_status();
                self.load_models(cx);
            }
            Err(e) => self.key_status = format!("Speichern fehlgeschlagen: {e:#}").into(),
        }
        cx.notify();
    }

    fn render_devices(&mut self, kind: Kind, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let (title, current, default_label) = match kind {
            Kind::Mic => ("Mikrofon", self.config.mic_device.clone(), "Systemstandard"),
            Kind::System => ("Desktop-Audio", self.config.system_device.clone(), "Standardausgabe (alles, was abgespielt wird)"),
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        let row = |id: SharedString, name: Option<String>, label: String, detail: Option<String>, selected: bool, cx: &mut Context<Self>| {
            let theme = cx.theme().clone();
            // The full technical name on hover, to tell look-alikes apart.
            let tip: Option<SharedString> = name.clone().map(|n| match &detail {
                Some(d) => format!("{d}\n{n}").into(),
                None => n.into(),
            });
            h_flex()
                .id(id)
                .when_some(tip, |el, tip| {
                    el.tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
                })
                .gap_3()
                .px_3()
                .py_2()
                .rounded(theme.radius)
                .cursor_pointer()
                .when(selected, |el| el.bg(theme.primary.opacity(0.1)))
                .hover(|s| s.bg(theme.muted))
                .child(
                    div()
                        .size(px(14.))
                        .flex_none()
                        .rounded_full()
                        .border_2()
                        .border_color(if selected { theme.primary } else { theme.border })
                        .when(selected, |el| el.child(div().m(px(2.)).size(px(6.)).rounded_full().bg(theme.primary))),
                )
                .child(div().min_w_0().text_sm().truncate().child(label))
                .on_click(cx.listener(move |this, _, _, cx| {
                    match kind {
                        Kind::Mic => this.config.mic_device = name.clone(),
                        Kind::System => this.config.system_device = name.clone(),
                    }
                    this.test[kind as usize] = None;
                    this.save(cx);
                }))
        };
        rows.push(row(format!("{title}-default").into(), None, default_label.into(), None, current.is_none(), cx).into_any_element());
        match &self.sources {
            None => rows.push(div().p_2().text_sm().text_color(theme.muted_foreground).child("Geräte werden gesucht …").into_any_element()),
            Some(Err(e)) => rows.push(div().p_2().text_sm().text_color(theme.danger).child(e.clone()).into_any_element()),
            Some(Ok(sources)) => {
                let show_all = self.show_all_devices;
                let mine: Vec<&SourceInfo> = sources.iter().filter(|s| s.is_monitor == (kind == Kind::System)).collect();
                // Unplugged ports (empty headphone jack, unused HDMI) are hidden
                // unless asked for — or currently selected.
                let hidden = mine
                    .iter()
                    .filter(|s| !s.available && current.as_deref() != Some(s.name.as_str()))
                    .count();
                for (i, s) in mine.into_iter().enumerate() {
                    let selected = current.as_deref() == Some(s.name.as_str());
                    if !s.available && !selected && !show_all {
                        continue;
                    }
                    let label = if s.available { s.label.clone() } else { format!("{} (nicht angeschlossen)", s.label) };
                    rows.push(
                        row(format!("{title}-{i}").into(), Some(s.name.clone()), label, Some(s.description.clone()), selected, cx)
                            .into_any_element(),
                    );
                }
                if hidden > 0 {
                    rows.push(
                        Button::new(SharedString::from(format!("{title}-show-all")))
                            .xsmall()
                            .ghost()
                            .label(if show_all {
                                "Nicht angeschlossene ausblenden".to_string()
                            } else {
                                format!("Alle Geräte anzeigen ({hidden} nicht angeschlossen)")
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_all_devices = !this.show_all_devices;
                                cx.notify();
                            }))
                            .into_any_element(),
                    );
                }
            }
        }

        let testing = self.testing == Some(kind);
        let result = self.test[kind as usize].clone();
        let theme = cx.theme().clone();
        ui::card(cx)
            .child(
                h_flex()
                    .justify_between()
                    .child(div().font_semibold().child(title))
                    .child(
                        Button::new(SharedString::from(format!("test-{title}")))
                            .small()
                            .outline()
                            .icon(IconName::AudioLines)
                            .label(if testing { "Höre 3 s zu …" } else { "Testen" })
                            .loading(testing)
                            .on_click(cx.listener(move |this, _, _, cx| this.test_device(kind, cx))),
                    ),
            )
            .when_some(result, |el, r| {
                el.child(match r {
                    Ok(rms) => {
                        let level = ui::meter_level(rms);
                        let text = if level < 0.15 {
                            "Kaum Signal — ist das richtige Gerät gewählt?"
                        } else if level > 0.95 {
                            "Sehr laut — eventuell übersteuert"
                        } else {
                            "Signal ist da"
                        };
                        h_flex()
                            .gap_3()
                            .text_sm()
                            .child(div().w(px(160.)).child(ui::level_meter(level, true, cx)))
                            .child(div().text_color(theme.muted_foreground).child(format!("{:.0} dB · {text}", 20.0 * rms.max(1e-6).log10())))
                            .into_any_element()
                    }
                    Err(e) => div().text_sm().text_color(theme.danger).child(e).into_any_element(),
                })
            })
            .child(v_flex().gap_0p5().children(rows))
    }
}

impl SettingsView {
    fn render_themes(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let selected = self.config.theme.clone();
        let tile = |id: String, name: String, preview: [Hsla; 5], font: SharedString, radius: f32, cx: &mut Context<Self>| {
            let [bg, side, fg, primary, border] = preview;
            let is_selected = selected == id;
            let id_for_click = id.clone();
            v_flex()
                .id(SharedString::from(format!("theme-{id}")))
                .w(px(148.))
                .gap_1p5()
                .cursor_pointer()
                .child(
                    h_flex()
                        .h(px(84.))
                        .rounded(px(radius.min(10.)))
                        .overflow_hidden()
                        .border_2()
                        .border_color(if is_selected { theme.primary } else { border })
                        .child(div().w(px(36.)).h_full().bg(side))
                        .child(
                            v_flex()
                                .flex_1()
                                .h_full()
                                .bg(bg)
                                .p_2()
                                .gap_1p5()
                                .font_family(font.clone())
                                .child(div().text_color(fg).text_sm().font_semibold().child("Aa"))
                                .child(div().h(px(4.)).w(px(56.)).rounded(px(radius.min(2.))).bg(fg.opacity(0.35)))
                                .child(div().h(px(4.)).w(px(40.)).rounded(px(radius.min(2.))).bg(fg.opacity(0.2)))
                                .child(div().mt_1().h(px(12.)).w(px(44.)).rounded(px(radius.min(6.))).bg(primary)),
                        ),
                )
                .child(
                    div()
                        .text_sm()
                        .when(is_selected, |el| el.text_color(theme.primary).font_semibold())
                        .child(name),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.config.theme = id_for_click.clone();
                    this.save(cx);
                    ui::apply_theme(window, cx);
                }))
        };

        let light = themes::resolve("hell", false);
        let dark = themes::resolve("dunkel", true);
        let mut tiles: Vec<AnyElement> = vec![
            tile(
                themes::SYSTEM.into(),
                "System (GNOME)".into(),
                [
                    themes::hex(&light.background),
                    themes::hex(&dark.sidebar),
                    themes::hex(&light.foreground),
                    themes::hex(&light.primary),
                    themes::hex(&light.border),
                ],
                light.font.clone().into(),
                light.radius,
                cx,
            )
            .into_any_element(),
        ];
        for t in themes::all() {
            let preview = [
                themes::hex(&t.background),
                themes::hex(&t.sidebar),
                themes::hex(&t.foreground),
                themes::hex(&t.primary),
                themes::hex(&t.border),
            ];
            tiles.push(tile(t.id.clone(), t.name.clone(), preview, t.font.clone().into(), t.radius, cx).into_any_element());
        }
        let current_font = self.config.font.clone();
        let mut font_buttons: Vec<AnyElement> = Vec::new();
        for (family, label) in ui::UI_FONTS {
            let on = current_font == family;
            let shown_in: SharedString = if family.is_empty() { theme.font_family.clone() } else { family.into() };
            font_buttons.push(
                Button::new(SharedString::from(format!("font-{label}")))
                    .small()
                    .when(on, |b| b.primary())
                    .when(!on, |b| b.outline())
                    .child(div().font_family(shown_in).child(label))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.config.font = family.into();
                        this.save(cx);
                        ui::apply_theme(window, cx);
                    }))
                    .into_any_element(),
            );
        }
        ui::card(cx)
            .child(div().font_semibold().child("Design"))
            .child(h_flex().flex_wrap().gap_4().children(tiles))
            .child(
                h_flex()
                    .justify_between()
                    .gap_4()
                    .child(
                        v_flex()
                            .child(div().text_sm().child("Schriftart"))
                            .child(div().text_xs().text_color(theme.muted_foreground).child("Arabische Schrift nutzt immer Noto Sans Arabic.")),
                    )
                    .child(h_flex().flex_wrap().gap_1().children(font_buttons)),
            )
            .child(
                h_flex()
                    .justify_between()
                    .child(div().text_sm().child("Animationen reduzieren"))
                    .child(Switch::new("set-reduce-motion").checked(self.config.reduce_motion).on_change(cx.listener(|this, v: &bool, _, cx| {
                        this.config.reduce_motion = *v;
                        this.save(cx);
                    }))),
            )
            .child(div().text_xs().text_color(theme.muted_foreground).child(format!(
                "Eigene Designs: JSON-Dateien in {} (Format wie assets/themes.json).",
                mostalgia_core::config::Config::path().with_file_name("themes").display()
            )))
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let lang = self.config.language.clone();
        let threshold = self.config.vad_threshold;
        let choice = |id: &'static str, label: &'static str, on: bool, cx: &mut Context<Self>, f: fn(&mut Config)| {
            Button::new(id)
                .small()
                .label(label)
                .when(on, |b| b.primary())
                .when(!on, |b| b.ghost())
                .on_click(cx.listener(move |this, _, _, cx| {
                    f(&mut this.config);
                    this.save(cx);
                }))
        };
        let theme = cx.theme().clone();
        // A setting row: label (+ hint) on the left, control on the right.
        let row = |label: &str, hint: Option<&str>, control: AnyElement| {
            h_flex()
                .justify_between()
                .gap_4()
                .child(
                    v_flex()
                        .min_w_0()
                        .flex_1()
                        .child(div().text_sm().child(label.to_string()))
                        .when_some(hint, |el, h| el.child(div().text_xs().text_color(theme.muted_foreground).child(h.to_string()))),
                )
                .child(control)
        };
        let switch = |id: &'static str, on: bool, cx: &mut Context<Self>, f: fn(&mut Config, bool)| {
            Switch::new(id)
                .checked(on)
                .on_change(cx.listener(move |this, v: &bool, _, cx| {
                    f(&mut this.config, *v);
                    this.save(cx);
                }))
                .into_any_element()
        };
        let title = |t: &str| div().font_semibold().child(t.to_string());
        let mut cards: Vec<Card> = Vec::new();

        // ── Aufnahme ──
        cards.push(Card {
            category: Category::Aufnahme,
            title: "Mikrofon",
            keywords: "mikrofon gerät eingang test pegel yeti",
            wide: false,
            element: self.render_devices(Kind::Mic, cx).into_any_element(),
        });
        cards.push(Card {
            category: Category::Aufnahme,
            title: "Desktop-Audio",
            keywords: "desktop systemaudio ausgabe monitor gerät test pegel",
            wide: false,
            element: self.render_devices(Kind::System, cx).into_any_element(),
        });
        let recognition = ui::card(cx)
            .child(title("Sprache & Erkennung"))
            .child(row(
                "Sprache",
                None,
                h_flex()
                    .gap_1()
                    .child(choice("set-de", "Deutsch", lang == "de", cx, |c| c.language = "de".into()))
                    .child(choice("set-en", "English", lang == "en", cx, |c| c.language = "en".into()))
                    .child(choice("set-auto", "Auto", lang == "auto", cx, |c| c.language = "auto".into()))
                    .into_any_element(),
            ))
            .child(row(
                "Spracherkennung (VAD)",
                Some("Empfindlicher erkennt leise Stimmen, aber auch mehr Geräusche."),
                h_flex()
                    .gap_1()
                    .child(choice("vad-low", "Zurückhaltend", threshold > 0.55, cx, |c| c.vad_threshold = 0.65))
                    .child(choice("vad-mid", "Normal", (0.45..=0.55).contains(&threshold), cx, |c| c.vad_threshold = 0.5))
                    .child(choice("vad-high", "Empfindlich", threshold < 0.45, cx, |c| c.vad_threshold = 0.35))
                    .into_any_element(),
            ))
            .child(row(
                "Vorläufigen Text anzeigen",
                Some("Grauer Text schon während des Sprechens, ersetzt beim Satzende."),
                switch("set-interim", self.config.interim_text, cx, |c, v| c.interim_text = v),
            ))
            .child(row(
                "Rauschunterdrückung (Mikrofon)",
                Some("Filtert Lüfter und Brummen vor der Erkennung; die Aufnahme bleibt roh."),
                switch("set-denoise", self.config.denoise, cx, |c, v| c.denoise = v),
            ));
        cards.push(Card {
            category: Category::Aufnahme,
            title: "Sprache & Erkennung",
            keywords: "sprache deutsch english automatisch vad spracherkennung empfindlich vorläufig interim rauschunterdrückung lüfter",
            wide: false,
            element: recognition.into_any_element(),
        });

        // ── Sprecher ──
        let diarize_ok = mostalgia_core::diarize::available();
        let dt = self.config.diarize_threshold;
        let speakers = ui::card(cx)
            .child(title("Sprecher"))
            .child(row(
                "Sprecher bei Live-Text anzeigen",
                Some("„Ich“ (Mikrofon) und „Andere“ (Desktop) an jeder Zeile."),
                switch("set-labels", self.config.speaker_labels, cx, |c, v| c.speaker_labels = v),
            ))
            .child(row(
                "Mehrere Sprecher unterscheiden",
                Some(if diarize_ok {
                    "Teilt „Andere“ in Sprecher 1, 2, … auf. Live ungefähr, im fertigen Transkript genau."
                } else {
                    "Modelle fehlen — scripts/download-model.sh ausführen."
                }),
                Switch::new("set-diarize")
                    .checked(self.config.diarize && diarize_ok)
                    .disabled(!diarize_ok)
                    .on_change(cx.listener(|this, v: &bool, _, cx| {
                        this.config.diarize = *v;
                        this.save(cx);
                    }))
                    .into_any_element(),
            ))
            .when(self.config.diarize && diarize_ok, |el| {
                el.child(row(
                    "Trennschärfe",
                    Some("Werden zwei Personen zusammengelegt: „Mehr Sprecher“. Wird eine Person geteilt: „Weniger“."),
                    h_flex()
                        .gap_1()
                        .child(choice("dia-less", "Weniger", dt > 0.55, cx, |c| c.diarize_threshold = 0.6))
                        .child(choice("dia-mid", "Normal", (0.45..=0.55).contains(&dt), cx, |c| c.diarize_threshold = 0.5))
                        .child(choice("dia-more", "Mehr Sprecher", dt < 0.45, cx, |c| c.diarize_threshold = 0.4))
                        .into_any_element(),
                ))
            })
            .child(div().text_xs().text_color(theme.muted_foreground).child(
                "Namen vergeben: im Transkript auf einen Sprecher-Chip klicken — alle Zeilen werden umbenannt.",
            ));
        cards.push(Card {
            category: Category::Sprecher,
            title: "Sprecher",
            keywords: "sprecher ich andere label diarisierung trennen mehrere personen umbenennen",
            wide: false,
            element: speakers.into_any_element(),
        });

        // ── KI & Zusammenfassung ──
        let provider = self.config.summary_provider;
        let current = self.models.iter().find(|m| m.id == self.config.summary_model).cloned();
        let model_label = current.as_ref().map_or(self.config.summary_model.clone(), |m| m.display_name.clone());
        let efforts: Vec<String> = current.as_ref().map(|m| m.efforts.clone()).unwrap_or_default();
        let models = self.models.clone();
        let effort_label = |e: &str| match e {
            "low" => "Niedrig",
            "medium" => "Mittel",
            "high" => "Hoch",
            "xhigh" => "Sehr hoch",
            "max" => "Maximum",
            other => other,
        }
        .to_string();
        let mut effort_buttons: Vec<AnyElement> = Vec::new();
        for e in &efforts {
            let on = &self.config.summary_effort == e;
            let value = e.clone();
            effort_buttons.push(
                Button::new(SharedString::from(format!("effort-{e}")))
                    .small()
                    .label(effort_label(e))
                    .when(on, |b| b.primary())
                    .when(!on, |b| b.ghost())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.config.summary_effort = value.clone();
                        this.save(cx);
                    }))
                    .into_any_element(),
            );
        }
        let provider_button = |p: summary::Provider, cx: &mut Context<Self>| {
            Button::new(SharedString::from(format!("provider-{}", p.id())))
                .small()
                .label(p.name())
                .when(provider == p, |b| b.primary())
                .when(provider != p, |b| b.ghost())
                .on_click(cx.listener(move |this, _, _, cx| this.set_provider(p, cx)))
        };
        let model_card = ui::card(cx)
            .child(title("Modell"))
            .child(row(
                "Anbieter",
                None,
                h_flex()
                    .gap_1()
                    .child(provider_button(summary::Provider::Claude, cx))
                    .child(provider_button(summary::Provider::DeepSeek, cx))
                    .into_any_element(),
            ))
            .child(row(
                "Modell",
                Some(if self.models_live { "Liste vom Anbieter" } else { "Eingebaute Liste (Schlüssel fehlt oder offline)" }),
                Button::new("summary-model")
                    .small()
                    .outline()
                    .label(model_label)
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, _, _| {
                        let mut menu = menu.scrollable(true).max_h(px(360.));
                        for m in &models {
                            menu = menu.menu(m.display_name.clone(), Box::new(SelectSummaryModel(m.id.clone())));
                        }
                        menu
                    })
                    .into_any_element(),
            ))
            .when(!efforts.is_empty(), |el| {
                el.child(
                    v_flex()
                        .gap_2()
                        .child(
                            v_flex()
                                .child(div().text_sm().child("Denktiefe"))
                                .child(div().text_xs().text_color(theme.muted_foreground).child("Mehr Denken = gründlicher, aber langsamer (bis zu Minuten).")),
                        )
                        .child(h_flex().flex_wrap().gap_1().children(effort_buttons)),
                )
            });
        cards.push(Card {
            category: Category::Ki,
            title: "Modell",
            keywords: "ki claude deepseek anbieter modell opus sonnet haiku denktiefe thinking effort zusammenfassung",
            wide: false,
            element: model_card.into_any_element(),
        });
        let key_card = ui::card(cx)
            .child(title(&format!("{}-API-Schlüssel", provider.name())))
            .child(div().text_sm().text_color(theme.muted_foreground).child("Gesendet wird nur der Text der Besprechung, nie Audio."))
            .child(
                h_flex()
                    .gap_2()
                    .child(Input::new(&self.api_key).flex_1())
                    .child(Button::new("save-key").primary().label("Speichern").on_click(cx.listener(|this, _, window, cx| this.save_key(window, cx)))),
            )
            .child(div().text_xs().text_color(theme.muted_foreground).child(self.key_status.clone()))
            .child(row(
                "Automatisch nach jeder Aufnahme",
                None,
                switch("set-auto-sum", self.config.auto_summary, cx, |c, v| c.auto_summary = v),
            ));
        cards.push(Card {
            category: Category::Ki,
            title: "API-Schlüssel",
            keywords: "api schlüssel key anthropic deepseek automatisch zusammenfassung",
            wide: false,
            element: key_card.into_any_element(),
        });

        // ── Darstellung ──
        cards.push(Card {
            category: Category::Darstellung,
            title: "Design",
            keywords: "design theme farbe hell dunkel blockwelt schrift schriftart animationen reduzieren",
            wide: true,
            element: self.render_themes(cx).into_any_element(),
        });

        // ── Speicher & Export ──
        let data_dir = paths::data_dir();
        let storage = ui::card(cx)
            .child(title("Speicherort"))
            .child(
                h_flex()
                    .gap_3()
                    .child(div().flex_1().min_w_0().text_sm().font_family(theme.mono_font_family.clone()).truncate().child(data_dir.display().to_string()))
                    .child(Button::new("open-data").small().outline().icon(IconName::FolderOpen).label("Öffnen").on_click(move |_, _, cx| {
                        cx.open_with_system(&data_dir);
                    })),
            )
            .child(div().text_xs().text_color(theme.muted_foreground).child("Datenbank, Aufnahmen (WAV) und Modelle liegen hier."));
        cards.push(Card {
            category: Category::Speicher,
            title: "Speicherort",
            keywords: "speicher ordner daten datenbank aufnahmen wav modelle",
            wide: false,
            element: storage.into_any_element(),
        });
        let export = ui::card(cx)
            .child(title("Export"))
            .child(div().text_sm().child("In jeder Besprechung über „Exportieren“: Zusammenfassung und Transkript als"))
            .child(
                v_flex()
                    .gap_1()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .children(mostalgia_core::export::Format::ALL.map(|f| h_flex().gap_2().child(Icon::new(IconName::FileText).xsmall()).child(f.label()))),
            )
            .child(div().text_xs().text_color(theme.muted_foreground).child("Im Terminal: mostalgia export <id> --format md|txt|srt|docx"));
        cards.push(Card {
            category: Category::Speicher,
            title: "Export",
            keywords: "export markdown text srt untertitel word docx",
            wide: false,
            element: export.into_any_element(),
        });

        // ── Tastenkürzel & System ──
        let shortcut = ui::card(cx)
            .child(title("Tastenkürzel"))
            .child(div().text_sm().text_color(theme.muted_foreground).child("Startet oder stoppt eine Aufnahme von überall – auch wenn die App geschlossen ist."))
            .child(match &self.shortcut {
                Some(b) => h_flex()
                    .gap_2()
                    .child(Icon::new(IconName::Check).small().text_color(theme.success))
                    .child(div().text_sm().child("Eingerichtet:"))
                    .child(div().px_2().py_0p5().rounded(theme.radius).border_1().border_color(theme.border).text_sm().font_family(theme.mono_font_family.clone()).child(pretty_binding(b)))
                    .into_any_element(),
                None => h_flex()
                    .gap_2()
                    .child(div().text_sm().child("Noch nicht eingerichtet."))
                    .child(Button::new("install-shortcut").small().primary().icon(IconName::Keyboard).label("Super + Shift + R einrichten").on_click(cx.listener(|this, _, _, cx| {
                        match install_shortcut() {
                            Ok(b) => this.shortcut = Some(b),
                            Err(e) => this.key_status = format!("{e:#}").into(),
                        }
                        cx.notify();
                    })))
                    .into_any_element(),
            })
            .child(div().text_xs().text_color(theme.muted_foreground).child("Ändern oder entfernen: GNOME-Einstellungen → Tastatur → Eigene Tastenkürzel."));
        cards.push(Card {
            category: Category::System,
            title: "Tastenkürzel",
            keywords: "tastenkürzel shortcut taste super toggle starten stoppen",
            wide: false,
            element: shortcut.into_any_element(),
        });
        let tray = ui::card(cx)
            .child(title("Tray-Symbol"))
            .child(match self.tray_available {
                Some(true) => h_flex().gap_2().child(Icon::new(IconName::Check).small().text_color(theme.success)).child(div().text_sm().child("Aktiv – oben in der Leiste.")).into_any_element(),
                Some(false) => v_flex()
                    .gap_1()
                    .child(h_flex().gap_2().child(Icon::new(IconName::TriangleAlert).small().text_color(theme.warning)).child(div().text_sm().child("GNOME zeigt gerade keine Tray-Symbole an.")))
                    .child(div().text_xs().text_color(theme.muted_foreground).child("Erweiterung einschalten und einmal ab- und wieder anmelden:"))
                    .child(div().text_xs().font_family(theme.mono_font_family.clone()).child("gnome-extensions enable ubuntu-appindicators@ubuntu.com"))
                    .into_any_element(),
                None => div().text_sm().text_color(theme.muted_foreground).child("Wird geprüft …").into_any_element(),
            });
        cards.push(Card {
            category: Category::System,
            title: "Tray-Symbol",
            keywords: "tray symbol leiste appindicator gnome",
            wide: false,
            element: tray.into_any_element(),
        });
        let model = self.config.whisper_model_path();
        let status_row = |ok: bool, text: String| {
            h_flex()
                .gap_2()
                .text_xs()
                .child(Icon::new(if ok { IconName::Check } else { IconName::TriangleAlert }).xsmall().text_color(if ok { theme.success } else { theme.warning }))
                .child(div().min_w_0().truncate().text_color(theme.muted_foreground).child(text))
        };
        let info = ui::card(cx)
            .child(title("Info"))
            .child(div().text_sm().child(format!("mostalgia {}", env!("CARGO_PKG_VERSION"))))
            .child(status_row(model.exists(), format!("Whisper: {}", model.display())))
            .child(status_row(self.config.vad_model_path().exists(), "Silero VAD".into()))
            .child(status_row(diarize_ok, "Sprechertrennung (sherpa-onnx)".into()))
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(IconName::Cpu).xsmall())
                    .child(self.gpu.clone().unwrap_or_else(|| "GPU wird bei der ersten Aufnahme erkannt".into())),
            );
        cards.push(Card {
            category: Category::System,
            title: "Info",
            keywords: "info version modell whisper gpu vad sherpa status",
            wide: false,
            element: info.into_any_element(),
        });

        // ── Layout: category list | card grid ──
        let query = self.query.trim().to_lowercase();
        let searching = !query.is_empty();
        let visible: Vec<Card> = cards
            .into_iter()
            .filter(|c| {
                if searching {
                    c.title.to_lowercase().contains(&query) || c.keywords.contains(&query) || c.category.title().to_lowercase().contains(&query)
                } else {
                    c.category == self.category
                }
            })
            .collect();
        let empty = visible.is_empty();
        let grid = h_flex().w_full().flex_wrap().items_start().gap_4().children(visible.into_iter().map(|c| {
            let caption = searching.then(|| c.category.title());
            v_flex()
                .gap_1()
                .when(c.wide, |el| el.w_full())
                // Two columns when there's room, one on narrow windows.
                .when(!c.wide, |el| el.flex_1().min_w(px(290.)))
                .when_some(caption, |el, cap| el.child(ui::section_title(cap.to_uppercase(), cx)))
                .child(c.element)
        }));

        let nav = v_flex()
            .w(px(220.))
            .flex_none()
            .h_full()
            .p_3()
            .gap_1()
            .border_r_1()
            .border_color(theme.border)
            .child(div().px_2().pb_2().text_lg().font_semibold().child("Einstellungen"))
            .child(div().pb_2().child(Input::new(&self.search).small().prefix(Icon::new(IconName::Search).small().text_color(theme.muted_foreground)).cleanable(true)))
            .children(Category::ALL.map(|cat| {
                let active = !searching && cat == self.category;
                h_flex()
                    .id(SharedString::from(format!("cat-{}", cat.id())))
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .text_sm()
                    .when(active, |el| el.bg(theme.primary.opacity(0.12)).text_color(theme.primary).font_medium())
                    .when(!active, |el| el.hover(|s| s.bg(theme.muted)))
                    .child(Icon::new(cat.icon()).small())
                    .child(cat.title())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.search.update(cx, |s, cx| s.set_value("", window, cx));
                        this.query.clear();
                        this.set_category(cat, cx);
                    }))
            }));

        let header = if searching {
            v_flex().child(div().text_2xl().font_semibold().child(format!("Suche: „{}“", self.query.trim())))
        } else {
            v_flex()
                .child(div().text_2xl().font_semibold().child(self.category.title()))
                .child(div().text_sm().text_color(theme.muted_foreground).child(self.category.subtitle()))
        };

        h_flex()
            .size_full()
            .key_context("Settings")
            .on_action(cx.listener(Self::select_model))
            .child(nav)
            .child(
                div().flex_1().min_w_0().h_full().child(
                    div()
                        .id("settings")
                        .size_full()
                        .track_scroll(&self.scroll)
                        .overflow_y_scroll()
                        .child(
                            v_flex()
                                .max_w(px(1180.))
                                .px_8()
                                .py_6()
                                .gap_5()
                                .child(header)
                                .child(grid)
                                .when(empty, |el| el.child(div().text_color(theme.muted_foreground).child("Keine passende Einstellung gefunden."))),
                        ),
                ),
            )
    }
}
