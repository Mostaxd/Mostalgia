//! Settings, grouped into categories (list on the left, a responsive
//! card grid on the right) with a search across all of them.

use std::collections::{HashMap, HashSet};

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Sizable, StyledExt, h_flex, switch::Switch, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use mostalgia_core::audio::{self, SourceInfo};
use mostalgia_core::cloud_stt;
use mostalgia_core::config::{Config, Mode, SttChoice};
use mostalgia_core::connections::{self, Api, Connection, PRESETS};
use mostalgia_core::llm::{self, ModelInfo};
use mostalgia_core::paths;

use mostalgia_core::i18n::{tr, trf};

use crate::{themes, ui};

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
    /// Model lists per connection id (built-in until the provider answers).
    models: HashMap<String, Vec<ModelInfo>>,
    models_live: HashSet<String>,
    /// Last connection test per connection id.
    status: HashMap<String, ConnStatus>,
    /// Where each connection's key comes from; `None` until read.
    keys: Option<HashMap<String, KeyState>>,
    auto_tested: bool,
    /// The add/edit connection panel, when open.
    form: Option<ConnForm>,
    form_name: Entity<InputState>,
    form_url: Entity<InputState>,
    form_key: Entity<InputState>,
    form_models: Entity<InputState>,
    /// Free-text model id, for the slot (and connection) it's typed for.
    custom_model: Entity<InputState>,
    custom_target: Option<(Slot, String)>,
    /// Processing mode last clicked, to explain what's still missing.
    chosen_mode: Option<Mode>,
    shortcut_error: Option<String>,
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

#[derive(Clone)]
enum ConnStatus {
    Testing,
    Ok(String),
    Failed(String),
}

#[derive(Clone, PartialEq)]
enum KeyState {
    Env,
    Keyring,
    Missing,
    Error(String),
}

fn key_state(c: &Connection) -> KeyState {
    match c.api_key() {
        Ok(Some(_)) if c.key_from_env() => KeyState::Env,
        Ok(Some(_)) => KeyState::Keyring,
        Ok(None) => KeyState::Missing,
        Err(e) => KeyState::Error(format!("{e:#}")),
    }
}

/// What a model picker sets.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Slot {
    Summary,
    Ask,
    Live,
    Final,
}

struct ConnForm {
    /// Id of the connection being edited; `None` when adding.
    editing: Option<String>,
    preset: &'static str,
    /// Editing: a key is stored (an empty key field keeps it).
    has_key: bool,
    remove_key: bool,
    error: Option<String>,
}

/// "0,36" / "0.36" for the UI language.
fn usd(amount: f64) -> String {
    let s = format!("{amount:.2}");
    if mostalgia_core::i18n::lang() == mostalgia_core::i18n::Lang::De { s.replace('.', ",") } else { s }
}

fn cost_label(conn: &Connection, model: &str) -> Option<String> {
    let cost = cloud_stt::estimated_cost(conn, model, 1.0)?;
    Some(if cost <= 0.0 { tr("kostenlos (eigener Server)").to_string() } else { trf("≈ {} $/Std.", &[&usd(cost)]) })
}

fn effort_label(e: &str) -> String {
    tr(match e {
        "low" => "Niedrig",
        "medium" => "Mittel",
        "high" => "Hoch",
        "xhigh" => "Sehr hoch",
        "max" => "Maximum",
        _ => "Unbekannt",
    })
    .to_string()
}

/// A thinking level valid for `efforts`, keeping `current` if it is.
fn valid_effort(efforts: &[String], current: &str) -> Option<String> {
    if efforts.is_empty() || efforts.iter().any(|e| e == current) {
        return None;
    }
    Some(if efforts.iter().any(|e| e == "high") { "high".into() } else { efforts.last().cloned().unwrap_or_default() })
}

/// Whether a connection's server may offer models we don't know of.
fn free_stt_models(c: &Connection) -> bool {
    matches!(c.preset.as_str(), "custom" | "litellm" | "vllm")
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
        tr(match self {
            Category::Aufnahme => "Aufnahme",
            Category::Sprecher => "Sprecher",
            Category::Ki => "KI & Transkription",
            Category::Darstellung => "Darstellung",
            Category::Speicher => "Speicher & Export",
            Category::System => "Tastenkürzel & System",
        })
    }

    fn subtitle(self) -> &'static str {
        tr(match self {
            Category::Aufnahme => "Geräte, Sprache und Spracherkennung",
            Category::Sprecher => "Wer hat was gesagt",
            Category::Ki => "Verbindungen, Modelle und wo transkribiert wird",
            Category::Darstellung => "Design, Schrift und Animationen",
            Category::Speicher => "Wo Aufnahmen liegen und wie exportiert wird",
            Category::System => "Tastenkürzel, Tray-Symbol und Systeminfo",
        })
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
    anyhow::ensure!(cli.exists(), "{}", trf("mostalgia (CLI) nicht neben der App gefunden: {}", &[&cli.display()]));
    let list = gsettings(&["get", MEDIA_KEYS, "custom-keybindings"]).unwrap_or_default();
    if !list.contains(SHORTCUT_PATH) {
        let new = if list.is_empty() || list == "@as []" || list == "[]" {
            format!("['{SHORTCUT_PATH}']")
        } else {
            format!("{}, '{SHORTCUT_PATH}']", list.trim_end_matches(']'))
        };
        gsettings(&["set", MEDIA_KEYS, "custom-keybindings", &new]).ok_or_else(|| anyhow::anyhow!(tr("gsettings fehlgeschlagen")))?;
    }
    let schema = format!("{MEDIA_KEYS}.custom-keybinding:{SHORTCUT_PATH}");
    let binding = "<Super><Shift>r";
    for (key, value) in [
        ("name", tr("Mostalgia: Aufnahme starten/stoppen").to_string()),
        ("command", format!("{} toggle", cli.display())),
        ("binding", binding.to_string()),
    ] {
        gsettings(&["set", &schema, key, &value]).ok_or_else(|| anyhow::anyhow!(tr("gsettings fehlgeschlagen")))?;
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
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Einstellungen durchsuchen")));
        let sub = cx.subscribe_in(&search, window, |this, input, event: &InputEvent, _, cx| {
            if let InputEvent::Change = event {
                this.query = input.read(cx).value().to_string();
                cx.notify();
            }
        });
        let form_name = cx.new(|cx| InputState::new(window, cx));
        let form_url = cx.new(|cx| InputState::new(window, cx).placeholder("https://…/v1"));
        let form_key = cx.new(|cx| InputState::new(window, cx).masked(true));
        let form_models = cx.new(|cx| InputState::new(window, cx).placeholder("llama3.3, qwen3:32b"));
        let custom_model = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Modell-ID")));
        let custom_sub = cx.subscribe_in(&custom_model, window, |this, _, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.apply_custom_model(window, cx);
            }
        });
        let url_sub = cx.subscribe(&form_url, |_, _, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
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
            models: HashMap::new(),
            models_live: HashSet::new(),
            status: HashMap::new(),
            keys: None,
            auto_tested: false,
            form: None,
            form_name,
            form_url,
            form_key,
            form_models,
            custom_model,
            custom_target: None,
            chosen_mode: None,
            shortcut_error: None,
            gpu,
            scroll: ScrollHandle::new(),
            category,
            search,
            query: String::new(),
            shortcut: None,
            tray_available: None,
            show_all_devices: false,
            _subscriptions: vec![sub, custom_sub, url_sub],
        };
        this.load_system_status(cx);
        this.load_keys(cx);
        this.load_ai_models(cx);
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
        let disk = Config::load();
        self.config.companion_pos = disk.companion_pos;
        self.config.companion_desktop_pos = disk.companion_desktop_pos;
        if let Err(e) = self.config.save() {
            eprintln!("[mostalgia] saving settings: {e:#}");
        }
        self.scroll.set_offset(point(px(0.), px(0.)));
        self.auto_test(cx);
        cx.notify();
    }

    /// Switches the app's UI language live ("system", "de", "en").
    fn set_ui_language(&mut self, value: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.config.ui_language = value.into();
        mostalgia_core::i18n::set_lang(mostalgia_core::i18n::resolve(value));
        self.search.update(cx, |s, cx| s.set_placeholder(tr("Einstellungen durchsuchen"), window, cx));
        self.custom_model.update(cx, |s, cx| s.set_placeholder(tr("Modell-ID"), window, cx));
        self.save(cx);
        cx.refresh_windows();
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

    /// Mori was moved between window and desktop outside the settings.
    pub fn set_companion_desktop(&mut self, desktop: bool, cx: &mut Context<Self>) {
        self.config.companion_desktop = desktop;
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        // Dragging Mori stores her position while this screen is open.
        let disk = Config::load();
        self.config.companion_pos = disk.companion_pos;
        self.config.companion_desktop_pos = disk.companion_desktop_pos;
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

    fn render_devices(&mut self, kind: Kind, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        // `id` keeps element ids stable across languages.
        let (id, title, current, default_label, default_tip) = match kind {
            Kind::Mic => ("mic", tr("Mikrofon"), self.config.mic_device.clone(), tr("Systemstandard"), tr("Das Standard-Mikrofon des Systems")),
            Kind::System => (
                "system",
                tr("Desktop-Audio"),
                self.config.system_device.clone(),
                tr("Standardausgabe"),
                tr("Alles, was gerade über die Standardausgabe abgespielt wird"),
            ),
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
        rows.push(
            row(format!("{id}-default").into(), None, default_label.into(), None, current.is_none(), cx)
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(default_tip).build(window, cx))
                .into_any_element(),
        );
        match &self.sources {
            None => rows.push(div().p_2().text_sm().text_color(theme.muted_foreground).child(tr("Geräte werden gesucht …")).into_any_element()),
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
                    let label = if s.available { s.label.clone() } else { trf("{} (nicht angeschlossen)", &[&s.label]) };
                    rows.push(
                        row(format!("{id}-{i}").into(), Some(s.name.clone()), label, Some(s.description.clone()), selected, cx)
                            .into_any_element(),
                    );
                }
                if hidden > 0 {
                    rows.push(
                        Button::new(SharedString::from(format!("{id}-show-all")))
                            .xsmall()
                            .ghost()
                            .label(if show_all {
                                tr("Nicht angeschlossene ausblenden").to_string()
                            } else {
                                trf("Alle Geräte anzeigen ({} nicht angeschlossen)", &[&hidden])
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
                    .flex_wrap()
                    .gap_2()
                    .justify_between()
                    .child(div().font_semibold().child(title))
                    .child(
                        Button::new(SharedString::from(format!("test-{id}")))
                            .small()
                            .outline()
                            .icon(IconName::AudioLines)
                            .label(if testing { tr("Höre 3 s zu …") } else { tr("Testen") })
                            .loading(testing)
                            .on_click(cx.listener(move |this, _, _, cx| this.test_device(kind, cx))),
                    ),
            )
            .when_some(result, |el, r| {
                el.child(match r {
                    Ok(rms) => {
                        let level = ui::meter_level(rms);
                        let text = tr(if level < 0.15 {
                            "Kaum Signal — ist das richtige Gerät gewählt?"
                        } else if level > 0.95 {
                            "Sehr laut — eventuell übersteuert"
                        } else {
                            "Signal ist da"
                        });
                        h_flex()
                            .flex_wrap()
                            .gap_x_3()
                            .gap_y_1()
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

        let light = themes::resolve("mostalgia", false);
        let dark = themes::resolve("mostalgia-nacht", true);
        let mut tiles: Vec<AnyElement> = vec![
            tile(
                themes::SYSTEM.into(),
                tr("System (GNOME)").into(),
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
            tiles.push(tile(t.id.clone(), theme_name(&t.id, &t.name), preview, t.font.clone().into(), t.radius, cx).into_any_element());
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
        let row = |label: &str, hint: Option<&str>, control: AnyElement| settings_row(label, hint, control, &theme);
        ui::card(cx)
            .child(div().font_semibold().child(tr("Design")))
            .child(h_flex().flex_wrap().gap_4().children(tiles))
            .child(row(
                tr("Schriftart"),
                Some(tr("Arabische Schrift nutzt immer Noto Sans Arabic.")),
                h_flex().flex_wrap().gap_1().children(font_buttons).into_any_element(),
            ))
            .child(row(
                tr("Animationen reduzieren"),
                None,
                Switch::new("set-reduce-motion")
                    .checked(self.config.reduce_motion)
                    .on_change(cx.listener(|this, v: &bool, _, cx| {
                        this.config.reduce_motion = *v;
                        this.save(cx);
                    }))
                    .into_any_element(),
            ))
            .child(div().text_xs().text_color(theme.muted_foreground).child(trf(
                "Eigene Designs: JSON-Dateien in {} (Format wie assets/themes.json).",
                &[&mostalgia_core::config::Config::path().with_file_name("themes").display()],
            )))
    }
}

// ── Darstellung: Mori ──
impl SettingsView {
    fn render_mori(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let row = |label: &str, hint: Option<&str>, control: AnyElement| settings_row(label, hint, control, &theme);
        let on = self.config.companion;
        let scale = self.config.companion_scale;
        let size_button = |id: &'static str, label: &'static str, value: f32, cx: &mut Context<Self>| {
            let selected = (scale - value).abs() < 0.01;
            Button::new(id)
                .small()
                .label(tr(label))
                .when(selected, |b| b.primary())
                .when(!selected, |b| b.ghost())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.config.companion_scale = value;
                    this.save(cx);
                }))
                .into_any_element()
        };
        let interval = self.config.companion_points_interval_secs;
        let interval_button = |secs: u32, cx: &mut Context<Self>| {
            let label = if secs == 0 { tr("Aus").to_string() } else { format!("{secs} s") };
            Button::new(SharedString::from(format!("mori-interval-{secs}")))
                .small()
                .label(label)
                .when(interval == secs, |b| b.primary())
                .when(interval != secs, |b| b.ghost())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.config.companion_points_interval_secs = secs;
                    this.save(cx);
                }))
                .into_any_element()
        };

        // Which connection she talks to: the one for "Fragen".
        let connection = match self.config.ask_request() {
            Ok(req) => {
                let conn = &req.connection;
                let ready = self.keys.is_none() || self.configured(conn);
                let (color, badge) = if conn.is_local() {
                    (theme.success, connections::local_label(conn.base()).to_string())
                } else {
                    (theme.warning, tr("Cloud").to_string())
                };
                v_flex()
                    .gap_1p5()
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_2()
                            .text_sm()
                            .child(Icon::new(if conn.is_local() { IconName::HardDrive } else { IconName::Globe }).small().text_color(theme.muted_foreground))
                            .child(div().child(trf("Nutzt {} · {}", &[&conn.name, &req.model])))
                            .child(chip(badge, color, &theme)),
                    )
                    .when(!ready, |el| {
                        el.child(div().text_xs().text_color(theme.warning).child(trf("{} ist nicht eingerichtet — in Einstellungen → KI hinterlegen.", &[&conn.name])))
                    })
                    .when(!conn.is_local() && interval > 0, |el| {
                        el.child(div().text_xs().text_color(theme.muted_foreground).child(trf(
                            "Live-Punkte senden etwa alle {} s das neue Transkript an {}; jede Frage sendet das Live-Transkript. Das kostet Tokens bei {}.",
                            &[&interval, &conn.name, &conn.name],
                        )))
                    })
                    .when(!conn.is_local() && interval == 0, |el| {
                        el.child(div().text_xs().text_color(theme.muted_foreground).child(trf("Jede Frage sendet das Live-Transkript an {}.", &[&conn.name])))
                    })
                    .child(div().text_xs().text_color(theme.muted_foreground).child(tr("Ändern unter KI & Transkription → Zusammenfassungen & Fragen.")))
                    .into_any_element()
            }
            Err(e) => div().text_xs().text_color(theme.danger).child(format!("{e:#}")).into_any_element(),
        };

        ui::card(cx)
            .child(
                h_flex()
                    .gap_2()
                    .child(Icon::new(IconName::Sparkles).small().text_color(theme.primary))
                    .child(div().flex_1().font_semibold().child(tr("Mori – Notiz-Begleiterin")))
                    .child(
                        Switch::new("set-mori")
                            .checked(on)
                            .on_change(cx.listener(|this, v: &bool, _, cx| {
                                this.config.companion = *v;
                                this.save(cx);
                            })),
                    ),
            )
            .child(div().text_sm().text_color(theme.muted_foreground).child(tr(
                "Eine kleine Figur im Fenster, die mitschreibt, solange gesprochen wird. Klick sie an, um Fragen zur laufenden Besprechung zu stellen; während der Aufnahme zeigt sie Entscheidungen, Aufgaben und offene Fragen, die du anheften kannst.",
            )))
            .when(on, |el| {
                el.child(row(
                    tr("Größe"),
                    None,
                    h_flex()
                        .gap_1()
                        .child(size_button("mori-size-s", "Klein", 0.75, cx))
                        .child(size_button("mori-size-m", "Mittel", 1.0, cx))
                        .child(size_button("mori-size-l", "Groß", 1.3, cx))
                        .into_any_element(),
                ))
                .child(row(
                    tr("Live-Punkte"),
                    Some(tr("Fragt nach so vielen Sekunden neuer Sprache nach neuen Punkten.")),
                    h_flex().flex_wrap().gap_1().children([0, 60, 90, 120, 180].map(|s| interval_button(s, cx))).into_any_element(),
                ))
                .child(row(
                    tr("Wo"),
                    Some(tr("Auf dem Desktop steht sie über allen Fenstern, z. B. über Teams, und lässt sich auf jeden Bildschirm ziehen.")),
                    h_flex()
                        .flex_wrap()
                        .gap_1()
                        .children([(false, "mori-where-window", "Im App-Fenster"), (true, "mori-where-desktop", "Auf dem Desktop (bleibt im Vordergrund)")].map(
                            |(desktop, id, label)| {
                                let selected = self.config.companion_desktop == desktop;
                                Button::new(id)
                                    .small()
                                    .label(tr(label))
                                    .when(selected, |b| b.primary())
                                    .when(!selected, |b| b.ghost())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.config.companion_desktop = desktop;
                                        this.save(cx);
                                    }))
                                    .into_any_element()
                            },
                        ))
                        .into_any_element(),
                ))
                .child(row(
                    tr("Leeren-Knopf im Chat zeigen"),
                    Some(tr("Ein Knopf in Moris Sprechblase, der den Chat leert (rückgängig machbar).")),
                    Switch::new("set-mori-clear")
                        .checked(self.config.companion_clear_button)
                        .on_change(cx.listener(|this, v: &bool, _, cx| {
                            this.config.companion_clear_button = *v;
                            this.save(cx);
                        }))
                        .into_any_element(),
                ))
                .child(row(
                    tr("Position"),
                    Some(tr("Zieh Mori mit der Maus an eine andere Stelle.")),
                    Button::new("mori-reset-pos")
                        .small()
                        .outline()
                        .icon(IconName::RefreshCw)
                        .label(tr("Position zurücksetzen"))
                        .disabled({
                            let disk = Config::load();
                            if self.config.companion_desktop { disk.companion_desktop_pos.is_none() } else { disk.companion_pos.is_none() }
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            let mut disk = Config::load();
                            if this.config.companion_desktop {
                                disk.companion_desktop_pos = None;
                                this.config.companion_desktop_pos = None;
                            } else {
                                disk.companion_pos = None;
                            }
                            if let Err(e) = disk.save() {
                                eprintln!("[mostalgia] saving settings: {e:#}");
                            }
                            this.save(cx);
                        }))
                        .into_any_element(),
                ))
                .child(connection)
            })
            .child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(IconName::Info).xsmall().mt(px(2.)).flex_none())
                    .child(div().flex_1().min_w_0().child(if self.config.companion_desktop {
                        tr("Auf dem Desktop läuft Mori als kleines eigenes Fenster über XWayland: Unter GNOME Wayland dürfen nur solche Fenster im Vordergrund bleiben und frei platziert werden. Klicks neben ihr gehen an das Fenster darunter.").to_string()
                    } else {
                        format!(
                            "{} {}",
                            tr("Mori lebt im Mostalgia-Fenster. Damit sie über anderen Fenstern steht, wähle „Auf dem Desktop“."),
                            tr("Aus dem Fenster ziehen bringt Mori auf den Desktop.")
                        )
                    })),
            )
    }
}

// ── KI & Transkription: state ──
impl SettingsView {
    /// Marks a connection's card as failed (e.g. after a summary or cloud
    /// transcription error elsewhere in the app).
    pub fn set_connection_error(&mut self, id: &str, error: String, cx: &mut Context<Self>) {
        self.status.insert(id.to_string(), ConnStatus::Failed(error));
        cx.notify();
    }

    /// Reads where every connection's key comes from (keyring access can
    /// block, so off the UI thread).
    fn load_keys(&mut self, cx: &mut Context<Self>) {
        let conns = self.config.connections.clone();
        let task = cx.background_spawn(async move { conns.iter().map(|c| (c.id.clone(), key_state(c))).collect::<HashMap<_, _>>() });
        cx.spawn(async move |this, cx| {
            let keys = task.await;
            this.update(cx, |this, cx| {
                this.keys = Some(keys);
                this.auto_test(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Set up enough to be used (`false` while keys are still being read).
    fn configured(&self, c: &Connection) -> bool {
        !c.base().is_empty()
            && (!c.needs_key() || self.keys.as_ref().is_some_and(|k| matches!(k.get(&c.id), Some(KeyState::Env | KeyState::Keyring))))
    }

    /// Tests every configured connection once, the first time the KI
    /// category is shown.
    fn auto_test(&mut self, cx: &mut Context<Self>) {
        if self.auto_tested || self.category != Category::Ki || self.keys.is_none() {
            return;
        }
        self.auto_tested = true;
        let ids: Vec<String> = self.config.connections.iter().filter(|c| self.configured(c)).map(|c| c.id.clone()).collect();
        for id in ids {
            self.test_connection(id, cx);
        }
    }

    fn test_connection(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(conn) = self.config.connection(&id).cloned() else { return };
        if matches!(self.status.get(&id), Some(ConnStatus::Testing)) {
            return;
        }
        self.status.insert(id.clone(), ConnStatus::Testing);
        cx.notify();
        let task = cx.background_spawn(async move { (llm::test(&conn).map_err(|e| format!("{e:#}")), key_state(&conn)) });
        cx.spawn(async move |this, cx| {
            let (result, key) = task.await;
            this.update(cx, |this, cx| {
                if let Some(keys) = &mut this.keys {
                    keys.insert(id.clone(), key);
                }
                let ok = result.is_ok();
                this.status.insert(id.clone(), match result {
                    Ok(note) => ConnStatus::Ok(note),
                    Err(e) => ConnStatus::Failed(e),
                });
                if ok && (this.config.summary_provider == id || this.ask_conn_id() == id) && !this.models_live.contains(&id) {
                    this.load_models(&id, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Connection id used for "ask your meetings".
    fn ask_conn_id(&self) -> String {
        if self.config.ask_provider.is_empty() { self.config.summary_provider.clone() } else { self.config.ask_provider.clone() }
    }

    fn load_ai_models(&mut self, cx: &mut Context<Self>) {
        let summary = self.config.summary_provider.clone();
        self.load_models(&summary, cx);
        let ask = self.ask_conn_id();
        if ask != summary {
            self.load_models(&ask, cx);
        }
    }

    /// Shows the built-in model list now and swaps in the live one if the
    /// provider answers.
    fn load_models(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(conn) = self.config.connection(id).cloned() else { return };
        let id = id.to_string();
        self.models.entry(id.clone()).or_insert_with(|| llm::fallback_models(&conn));
        let task = cx.background_spawn(async move {
            let key = conn.require_key().ok()?;
            llm::list_models(&conn, key.as_deref()).ok()
        });
        cx.spawn(async move |this, cx| {
            if let Some(models) = task.await {
                this.update(cx, |this, cx| {
                    this.models.insert(id.clone(), models);
                    this.models_live.insert(id);
                    this.fill_empty_models(cx);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// A model for a slot whose connection has no default (Ollama & co.),
    /// once the list is known.
    fn fill_empty_models(&mut self, cx: &mut Context<Self>) {
        let first = |this: &Self, id: &str| this.models.get(id).and_then(|m| m.first()).map(|m| m.id.clone());
        let mut changed = false;
        if self.config.summary_model.is_empty() {
            if let Some(m) = first(self, &self.config.summary_provider) {
                self.config.summary_model = m;
                changed = true;
            }
        }
        if !self.config.ask_provider.is_empty() && self.config.ask_model.is_empty() {
            if let Some(m) = first(self, &self.config.ask_provider) {
                self.config.ask_model = m;
                changed = true;
            }
        }
        if changed {
            self.save(cx);
        }
    }

    /// The preset's default model, else the first one listed.
    fn initial_model(&self, conn: &Connection) -> String {
        let default = conn.preset().default_model;
        if !default.is_empty() {
            return default.into();
        }
        self.models
            .get(&conn.id)
            .and_then(|m| m.first())
            .map(|m| m.id.clone())
            .or_else(|| conn.models.first().cloned())
            .unwrap_or_default()
    }

    /// Thinking levels `model` on connection `id` supports.
    fn efforts(&self, id: &str, model: &str) -> Vec<String> {
        let Some(conn) = self.config.connection(id) else { return Vec::new() };
        if let Some(m) = self.models.get(id).and_then(|l| l.iter().find(|m| m.id == model)) {
            return m.efforts.clone();
        }
        match conn.api {
            Api::Anthropic if !model.is_empty() => llm::EFFORTS.iter().map(|e| e.to_string()).collect(),
            Api::Anthropic => Vec::new(),
            Api::OpenAi => llm::openai_efforts(conn, model),
        }
    }

    fn set_summary_connection(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(conn) = self.config.connection(&id).cloned() else { return };
        if self.config.summary_provider != id {
            self.config.summary_provider = id.clone();
            self.load_models(&id, cx);
            self.config.summary_model = self.initial_model(&conn);
        }
        self.fix_efforts();
        self.save(cx);
    }

    fn set_ask_connection(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        match id {
            None => {
                self.config.ask_provider.clear();
                self.config.ask_model.clear();
            }
            Some(id) => {
                let Some(conn) = self.config.connection(&id).cloned() else { return };
                if self.config.ask_provider == id {
                    return;
                }
                self.config.ask_provider = id.clone();
                self.load_models(&id, cx);
                self.config.ask_model = self.initial_model(&conn);
            }
        }
        self.fix_efforts();
        self.save(cx);
    }

    /// Keeps both thinking levels valid for their models.
    fn fix_efforts(&mut self) {
        let summary = self.efforts(&self.config.summary_provider, &self.config.summary_model);
        if let Some(e) = valid_effort(&summary, &self.config.summary_effort) {
            self.config.summary_effort = e;
        }
        let ask_model = if self.config.ask_model.is_empty() { self.config.summary_model.clone() } else { self.config.ask_model.clone() };
        let ask = self.efforts(&self.ask_conn_id(), &ask_model);
        if let Some(e) = valid_effort(&ask, &self.config.ask_effort) {
            self.config.ask_effort = e;
        }
    }

    fn select_model(&mut self, slot: Slot, conn: String, model: String, cx: &mut Context<Self>) {
        match slot {
            Slot::Summary => self.config.summary_model = model,
            Slot::Ask => self.config.ask_model = model,
            Slot::Live => self.config.live_engine = SttChoice { connection: conn, model },
            Slot::Final => self.config.final_engine = SttChoice { connection: conn, model },
        }
        self.fix_efforts();
        self.save(cx);
    }

    fn open_custom_model(&mut self, slot: Slot, conn: String, window: &mut Window, cx: &mut Context<Self>) {
        self.custom_target = Some((slot, conn));
        self.custom_model.update(cx, |s, cx| {
            s.set_value("", window, cx);
            s.focus(window, cx);
        });
        cx.notify();
    }

    fn apply_custom_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.custom_model.read(cx).value().trim().to_string();
        let Some((slot, conn)) = self.custom_target.clone() else { return };
        if value.is_empty() {
            return;
        }
        self.custom_target = None;
        self.custom_model.update(cx, |s, cx| s.set_value("", window, cx));
        self.select_model(slot, conn, value, cx);
    }

    /// The connection cloud transcription would use, with (live, final)
    /// models: OpenAI and Groq first (the summary connection if it's one of
    /// them), then self-hosted Whisper servers.
    fn cloud_stt_default(&self) -> Option<(String, String, String)> {
        let summary = self.config.summary_provider.as_str();
        let mut candidates: Vec<&Connection> =
            self.config.connections.iter().filter(|c| !c.stt_models().is_empty() && self.configured(c)).collect();
        candidates.sort_by_key(|c| (free_stt_models(c), c.id != summary));
        let conn = candidates.first()?;
        let stt = conn.stt_models();
        let (live, fin) = if conn.preset == "openai" { ("gpt-4o-mini-transcribe", "gpt-4o-transcribe") } else { (stt[0].id, stt[0].id) };
        Some((conn.id.clone(), live.into(), fin.into()))
    }

    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.chosen_mode = Some(mode);
        match mode {
            Mode::Local | Mode::Hybrid => {
                self.config.live_engine = SttChoice::local();
                self.config.final_engine = SttChoice::local();
            }
            Mode::Cloud => {
                if let Some((conn, live, fin)) = self.cloud_stt_default() {
                    self.config.live_engine = SttChoice { connection: conn.clone(), model: live };
                    self.config.final_engine = SttChoice { connection: conn, model: fin };
                }
            }
        }
        self.save(cx);
    }

    /// Uses a local connection for summaries and questions.
    fn use_local_ai(&mut self, id: String, cx: &mut Context<Self>) {
        self.set_ask_connection(None, cx);
        self.set_summary_connection(id, cx);
    }

    /// Where a connection is in use: summaries, ask, live, final.
    fn uses(&self, id: &str) -> [bool; 4] {
        [
            self.config.summary_provider == id,
            self.ask_conn_id() == id,
            self.config.live_engine.connection == id,
            self.config.final_engine.connection == id,
        ]
    }

    fn open_form(&mut self, editing: Option<String>, preset: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let existing = editing.as_ref().and_then(|id| self.config.connection(id).cloned());
        let p = existing.as_ref().map_or(connections::preset(preset), |c| c.preset());
        let (name, url, models) = match &existing {
            Some(c) => (c.name.clone(), c.base_url.clone(), c.models.join(", ")),
            None => (tr(p.name).to_string(), p.base_url.to_string(), String::new()),
        };
        let has_key = existing
            .as_ref()
            .is_some_and(|c| matches!(self.keys.as_ref().and_then(|k| k.get(&c.id)), Some(KeyState::Keyring | KeyState::Env)));
        self.form_name.update(cx, |s, cx| s.set_value(name, window, cx));
        self.form_url.update(cx, |s, cx| s.set_value(url, window, cx));
        self.form_models.update(cx, |s, cx| s.set_value(models, window, cx));
        let placeholder = if has_key {
            tr("Leer lassen = gespeicherten Schlüssel behalten")
        } else if p.needs_key {
            tr("API-Schlüssel einfügen")
        } else {
            tr("Optional")
        };
        self.form_key.update(cx, |s, cx| {
            s.set_value("", window, cx);
            s.set_placeholder(placeholder, window, cx);
        });
        self.form = Some(ConnForm { editing, preset: p.id, has_key, remove_key: false, error: None });
        cx.notify();
    }

    fn save_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &self.form else { return };
        let p = connections::preset(form.preset);
        let name = self.form_name.read(cx).value().trim().to_string();
        let url = self.form_url.read(cx).value().trim().to_string();
        let key = self.form_key.read(cx).value().trim().to_string();
        let models: Vec<String> =
            self.form_models.read(cx).value().split([',', '\n']).map(|m| m.trim().to_string()).filter(|m| !m.is_empty()).collect();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            if let Some(form) = &mut self.form {
                form.error = Some(tr("Bitte eine Base-URL mit http:// oder https:// angeben.").into());
            }
            cx.notify();
            return;
        }
        let name = if name.is_empty() { tr(p.name).to_string() } else { name };
        let remove_key = form.remove_key;
        let conn = match form.editing.clone() {
            Some(id) => {
                let Some(c) = self.config.connections.iter_mut().find(|c| c.id == id) else { return };
                c.name = name;
                c.base_url = url;
                c.models = models;
                c.clone()
            }
            None => {
                let id = connections::new_id(&self.config.connections, p.id);
                let mut c = Connection::from_preset(p, &id);
                c.name = name;
                c.base_url = url;
                c.models = models;
                self.config.connections.push(c.clone());
                c
            }
        };
        self.form = None;
        self.form_key.update(cx, |s, cx| s.set_value("", window, cx));
        self.models.remove(&conn.id);
        self.models_live.remove(&conn.id);
        self.save(cx);
        // Keyring writes can block: store the key, then test.
        let id = conn.id.clone();
        self.status.insert(id.clone(), ConnStatus::Testing);
        let task = cx.background_spawn(async move {
            if !key.is_empty() {
                conn.save_api_key(&key)?;
            } else if remove_key {
                conn.delete_api_key()?;
            }
            Ok::<_, anyhow::Error>(key_state(&conn))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.status.remove(&id);
                match result {
                    Ok(state) => {
                        this.keys.get_or_insert_with(HashMap::new).insert(id.clone(), state);
                        if this.uses(&id)[0] || this.uses(&id)[1] {
                            this.load_models(&id, cx);
                        }
                        let configured = this.config.connection(&id).is_some_and(|c| this.configured(c));
                        if configured {
                            this.test_connection(id, cx);
                        }
                    }
                    Err(e) => {
                        this.status.insert(id, ConnStatus::Failed(format!("{e:#}")));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn remove_connection(&mut self, id: String, cx: &mut Context<Self>) {
        if self.uses(&id).iter().any(|u| *u) {
            return;
        }
        let Some(pos) = self.config.connections.iter().position(|c| c.id == id) else { return };
        let conn = self.config.connections.remove(pos);
        self.status.remove(&id);
        self.models.remove(&id);
        if let Some(keys) = &mut self.keys {
            keys.remove(&id);
        }
        if self.form.as_ref().is_some_and(|f| f.editing.as_deref() == Some(id.as_str())) {
            self.form = None;
        }
        self.save(cx);
        cx.background_spawn(async move {
            if let Err(e) = conn.delete_api_key() {
                eprintln!("[mostalgia] removing key of {}: {e:#}", conn.name);
            }
        })
        .detach();
    }
}

/// A choice in one of the KI pickers.
#[derive(Clone)]
enum Pick {
    SummaryConn(String),
    AskConn(Option<String>),
    /// Slot, connection id, model id.
    Model(Slot, String, String),
    /// Type a model id for the slot on this connection.
    Custom(Slot, String),
    /// "meeting", "app" or a language code.
    SummaryLanguage(String),
}

/// Fixed choices for the summary language (code, name in that language).
const SUMMARY_LANGUAGES: [(&str, &str); 12] = [
    ("en", "English"),
    ("de", "Deutsch"),
    ("fr", "Français"),
    ("es", "Español"),
    ("it", "Italiano"),
    ("nl", "Nederlands"),
    ("pt", "Português"),
    ("pl", "Polski"),
    ("tr", "Türkçe"),
    ("ar", "العربية"),
    ("ja", "日本語"),
    ("zh", "中文"),
];

/// A dropdown entry: label, checked, what it does (`None`: a heading).
type Choice = (String, bool, Option<Pick>);

fn chip(text: impl Into<SharedString>, color: Hsla, theme: &gpui_kit::component::Theme) -> Div {
    h_flex()
        .gap_1p5()
        .flex_none()
        .px_2()
        .py_0p5()
        .rounded(theme.radius * 2.)
        .bg(color.opacity(0.12))
        .text_color(color)
        .text_xs()
        .font_semibold()
        .child(div().size(px(6.)).rounded_full().bg(color))
        .child(text.into())
}

// ── KI & Transkription: cards ──
impl SettingsView {
    fn pick(&mut self, pick: Pick, window: &mut Window, cx: &mut Context<Self>) {
        match pick {
            Pick::SummaryConn(id) => self.set_summary_connection(id, cx),
            Pick::AskConn(id) => self.set_ask_connection(id, cx),
            Pick::Model(slot, conn, model) => self.select_model(slot, conn, model, cx),
            Pick::Custom(slot, conn) => self.open_custom_model(slot, conn, window, cx),
            Pick::SummaryLanguage(code) => {
                self.config.summary_language = code;
                self.save(cx);
            }
        }
    }

    fn picker(&self, id: &'static str, label: String, items: Vec<Choice>, cx: &mut Context<Self>) -> AnyElement {
        let weak = cx.entity().downgrade();
        Button::new(id)
            .small()
            .outline()
            .label(label)
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.scrollable(true).max_h(px(360.));
                for (label, checked, pick) in &items {
                    menu = match pick {
                        None => menu.label(label.clone()),
                        Some(pick) => {
                            let (pick, weak) = (pick.clone(), weak.clone());
                            menu.item(PopupMenuItem::new(label.clone()).checked(*checked).on_click(move |_, window, cx| {
                                weak.update(cx, |this, cx| this.pick(pick.clone(), window, cx)).ok();
                            }))
                        }
                    };
                }
                menu
            })
            .into_any_element()
    }

    fn conn_label(&self, c: &Connection) -> String {
        if self.keys.is_some() && !self.configured(c) { trf("{} (nicht eingerichtet)", &[&c.name]) } else { c.name.clone() }
    }

    /// "Bleibt lokal (Ollama)" / "Text der Besprechung geht an OpenAI (Cloud)".
    fn privacy_line(&self, id: &str, cx: &App) -> Div {
        let theme = cx.theme();
        let (local, text) = match self.config.connection(id) {
            Some(c) if c.is_local() => (true, trf("Bleibt lokal ({})", &[&c.name])),
            Some(c) => (false, trf("Text der Besprechung geht an {} (Cloud)", &[&c.name])),
            None => (false, trf("Verbindung \"{}\" gibt es nicht mehr", &[&id])),
        };
        h_flex()
            .gap_2()
            .text_xs()
            .text_color(if local { theme.success } else { theme.muted_foreground })
            .child(Icon::new(if local { IconName::HardDrive } else { IconName::Globe }).xsmall().flex_none())
            .child(div().flex_1().min_w_0().child(text))
    }

    fn custom_model_row(&self, slot: Slot, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (target, conn) = self.custom_target.clone()?;
        if target != slot {
            return None;
        }
        let name = self.config.connection(&conn).map_or(conn.clone(), |c| c.name.clone());
        let theme = cx.theme().clone();
        Some(
            v_flex()
                .gap_1()
                .child(div().text_xs().text_color(theme.muted_foreground).child(trf("Modell-ID bei {} eingeben", &[&name])))
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().min_w(px(140.)).child(Input::new(&self.custom_model).small()))
                        .child(
                            Button::new(SharedString::from(format!("custom-ok-{slot:?}")))
                                .small()
                                .primary()
                                .label(tr("Übernehmen"))
                                .on_click(cx.listener(|this, _, window, cx| this.apply_custom_model(window, cx))),
                        )
                        .child(Button::new(SharedString::from(format!("custom-cancel-{slot:?}"))).small().ghost().label(tr("Abbrechen")).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.custom_target = None;
                                cx.notify();
                            }),
                        )),
                )
                .into_any_element(),
        )
    }

    fn effort_row(&self, slot: Slot, efforts: &[String], cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let current = if slot == Slot::Ask { self.config.ask_effort.clone() } else { self.config.summary_effort.clone() };
        let buttons: Vec<AnyElement> = efforts
            .iter()
            .map(|e| {
                let on = &current == e;
                let value = e.clone();
                Button::new(SharedString::from(format!("effort-{slot:?}-{e}")))
                    .small()
                    .label(effort_label(e))
                    .when(on, |b| b.primary())
                    .when(!on, |b| b.ghost())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        match slot {
                            Slot::Ask => this.config.ask_effort = value.clone(),
                            _ => this.config.summary_effort = value.clone(),
                        }
                        this.save(cx);
                    }))
                    .into_any_element()
            })
            .collect();
        v_flex()
            .gap_2()
            .child(
                v_flex()
                    .child(div().text_sm().child(tr("Denktiefe")))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(tr("Mehr Denken = gründlicher, aber langsamer (bis zu Minuten)."))),
            )
            .child(h_flex().flex_wrap().gap_1().children(buttons))
            .into_any_element()
    }

    /// Model dropdown items for an LLM slot on connection `id`.
    fn model_choices(&self, slot: Slot, id: &str, current: &str) -> Vec<Choice> {
        let mut items: Vec<Choice> = self
            .models
            .get(id)
            .into_iter()
            .flatten()
            .map(|m| (m.display_name.clone(), m.id == current, Some(Pick::Model(slot, id.to_string(), m.id.clone()))))
            .collect();
        if !current.is_empty() && !items.iter().any(|(_, on, _)| *on) {
            items.insert(0, (current.to_string(), true, Some(Pick::Model(slot, id.to_string(), current.to_string()))));
        }
        items.push((tr("Eigenes Modell …").into(), false, Some(Pick::Custom(slot, id.to_string()))));
        items
    }

    fn model_label(&self, id: &str, model: &str) -> String {
        if model.is_empty() {
            return tr("Modell wählen").into();
        }
        self.models.get(id).and_then(|l| l.iter().find(|m| m.id == model)).map_or(model.to_string(), |m| m.display_name.clone())
    }

    fn render_ai_use(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let row = |label: &str, hint: Option<&str>, control: AnyElement| settings_row(label, hint, control, &theme);
        let conns = self.config.connections.clone();
        let summary_id = self.config.summary_provider.clone();
        let summary_model = self.config.summary_model.clone();

        let summary_conn_label = self.config.connection(&summary_id).map_or_else(|| tr("Verbindung wählen").to_string(), |c| self.conn_label(c));
        let summary_conns: Vec<Choice> =
            conns.iter().map(|c| (self.conn_label(c), c.id == summary_id, Some(Pick::SummaryConn(c.id.clone())))).collect();
        let summary_conn_picker = self.picker("summary-conn", summary_conn_label, summary_conns, cx);
        let summary_models = self.model_choices(Slot::Summary, &summary_id, &summary_model);
        let summary_model_picker = self.picker("summary-model", self.model_label(&summary_id, &summary_model), summary_models, cx);
        let summary_efforts = self.efforts(&summary_id, &summary_model);
        let live_list = self.models_live.contains(&summary_id);

        let separate = !self.config.ask_provider.is_empty();
        let ask_id = self.ask_conn_id();
        let ask_model = if self.config.ask_model.is_empty() { summary_model.clone() } else { self.config.ask_model.clone() };
        let ask_label = if separate {
            self.config.connection(&ask_id).map_or(ask_id.clone(), |c| self.conn_label(c))
        } else {
            tr("Wie Zusammenfassungen").to_string()
        };
        let mut ask_conns: Vec<Choice> = vec![(tr("Wie Zusammenfassungen").into(), !separate, Some(Pick::AskConn(None)))];
        ask_conns.extend(conns.iter().map(|c| (self.conn_label(c), separate && c.id == ask_id, Some(Pick::AskConn(Some(c.id.clone()))))));
        let ask_conn_picker = self.picker("ask-conn", ask_label, ask_conns, cx);
        let ask_model_picker = separate.then(|| {
            let items = self.model_choices(Slot::Ask, &ask_id, &ask_model);
            self.picker("ask-model", self.model_label(&ask_id, &ask_model), items, cx)
        });
        let ask_efforts = self.efforts(&ask_id, &ask_model);

        let summary_custom = self.custom_model_row(Slot::Summary, cx);
        let ask_custom = self.custom_model_row(Slot::Ask, cx);
        let summary_effort_row = (!summary_efforts.is_empty()).then(|| self.effort_row(Slot::Summary, &summary_efforts, cx));
        let ask_effort_row = (!ask_efforts.is_empty()).then(|| self.effort_row(Slot::Ask, &ask_efforts, cx));
        let summary_privacy = self.privacy_line(&summary_id, cx);
        let ask_privacy = self.privacy_line(&ask_id, cx);
        let auto = Switch::new("set-auto-sum").checked(self.config.auto_summary).on_change(cx.listener(|this, v: &bool, _, cx| {
            this.config.auto_summary = *v;
            this.save(cx);
        }));

        let summary_language = {
            let current = self.config.summary_language.clone();
            let mut items: Vec<Choice> = vec![
                (tr("Wie in der Besprechung").into(), current == "meeting" || current.is_empty(), Some(Pick::SummaryLanguage("meeting".into()))),
                (tr("In App-Sprache").into(), current == "app", Some(Pick::SummaryLanguage("app".into()))),
            ];
            items.extend(SUMMARY_LANGUAGES.iter().map(|(code, name)| (name.to_string(), current == *code, Some(Pick::SummaryLanguage(code.to_string())))));
            let label = match current.as_str() {
                "" | "meeting" => tr("Wie in der Besprechung").to_string(),
                "app" => tr("In App-Sprache").to_string(),
                code => SUMMARY_LANGUAGES
                    .iter()
                    .find(|(c, _)| *c == code)
                    .map(|(_, n)| n.to_string())
                    .unwrap_or_else(|| mostalgia_core::summary::language_name(code)),
            };
            self.picker("summary-language", label, items, cx)
        };

        ui::card(cx)
            .child(div().font_semibold().child(tr("Zusammenfassungen & Fragen")))
            .child(ui::section_title(tr("ZUSAMMENFASSUNGEN"), cx))
            .child(row(tr("Verbindung"), None, summary_conn_picker))
            .child(row(
                tr("Modell"),
                Some(tr(if live_list { "Liste vom Anbieter" } else { "Eingebaute Liste (Schlüssel fehlt oder offline)" })),
                summary_model_picker,
            ))
            .children(summary_custom)
            .children(summary_effort_row)
            .child(row(tr("Sprache der Zusammenfassung"), None, summary_language))
            .child(summary_privacy)
            .child(row(tr("Automatisch nach jeder Aufnahme"), None, auto.into_any_element()))
            .child(div().h(px(1.)).bg(theme.border))
            .child(ui::section_title(tr("FRAGEN (ASK)"), cx))
            .child(row(tr("Verbindung"), Some(tr("Für „Frag deine Besprechungen“")), ask_conn_picker))
            .children(ask_model_picker.map(|p| row(tr("Modell"), None, p)))
            .children(ask_custom)
            .children(ask_effort_row)
            .child(ask_privacy)
    }

    fn stt_hint(&self, choice: &SttChoice) -> String {
        if choice.is_local() {
            return tr("Auf diesem Rechner (GPU) · kostenlos").into();
        }
        match self.config.connection(&choice.connection) {
            None => trf("Verbindung \"{}\" gibt es nicht mehr", &[&choice.connection]),
            Some(c) => {
                let place = if c.is_local() { trf("Bleibt lokal ({})", &[&c.name]) } else { trf("Audio geht an {} (Cloud)", &[&c.name]) };
                match cost_label(c, &choice.model) {
                    Some(cost) => format!("{place} · {cost}"),
                    None => place,
                }
            }
        }
    }

    fn stt_picker(&self, slot: Slot, id: &'static str, cx: &mut Context<Self>) -> AnyElement {
        let current = if slot == Slot::Live { self.config.live_engine.clone() } else { self.config.final_engine.clone() };
        let mut items: Vec<Choice> =
            vec![(tr("Whisper large-v3-turbo (lokal)").into(), current.is_local(), Some(Pick::Model(slot, String::new(), String::new())))];
        let mut listed = current.is_local();
        for c in self.config.connections.iter().filter(|c| !c.stt_models().is_empty() && self.configured(c)) {
            for m in c.stt_models() {
                let on = current.connection == c.id && current.model == m.id;
                listed |= on;
                let label = match cost_label(c, m.id) {
                    Some(cost) => format!("{} · {} — {cost}", c.name, m.id),
                    None => format!("{} · {}", c.name, m.id),
                };
                items.push((label, on, Some(Pick::Model(slot, c.id.clone(), m.id.into()))));
            }
            if free_stt_models(c) {
                items.push((trf("{}: eigenes Modell …", &[&c.name]), false, Some(Pick::Custom(slot, c.id.clone()))));
            }
        }
        if !listed {
            items.insert(1, (self.config.engine_label(&current), true, Some(Pick::Model(slot, current.connection.clone(), current.model.clone()))));
        }
        self.picker(id, self.config.engine_label(&current), items, cx)
    }

    fn render_processing(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mode = self.config.mode();
        let tile = |m: Mode, icon: IconName, text: &'static str, cx: &mut Context<Self>| {
            let on = mode == m;
            v_flex()
                .id(SharedString::from(format!("mode-{m:?}")))
                .flex_1()
                .min_w(px(170.))
                .p_3()
                .gap_1()
                .rounded(theme.radius)
                .border_1()
                .border_color(if on { theme.primary } else { theme.border })
                .when(on, |el| el.bg(theme.primary.opacity(0.08)))
                .cursor_pointer()
                .hover(|s| s.bg(theme.muted))
                .child(
                    h_flex()
                        .gap_2()
                        .when(on, |el| el.text_color(theme.primary))
                        .child(Icon::new(icon).small())
                        .child(div().font_semibold().child(m.label()))
                        .when(m == Mode::Hybrid, |el| el.child(div().text_xs().text_color(theme.muted_foreground).child(tr("Standard")))),
                )
                .child(div().text_xs().text_color(theme.muted_foreground).child(tr(text)))
                .on_click(cx.listener(move |this, _, _, cx| this.set_mode(m, cx)))
        };
        let tiles = h_flex()
            .flex_wrap()
            .gap_2()
            .child(tile(Mode::Local, IconName::Cpu, "Alles auf diesem Rechner: Whisper auf der GPU, KI über Ollama/LM Studio o. ä.", cx))
            .child(tile(Mode::Hybrid, IconName::Sparkles, "Transkription lokal, Zusammenfassungen und Fragen mit einem Cloud-Modell", cx))
            .child(tile(
                Mode::Cloud,
                IconName::Globe,
                "Transkription in der Cloud — für Rechner ohne starke GPU. Audio wird an den Anbieter gesendet.",
                cx,
            ));

        // What the last click couldn't do on its own.
        let mut hint: Option<AnyElement> = None;
        if self.chosen_mode == Some(Mode::Local) && mode != Mode::Local {
            let mut cloud: Vec<String> = Vec::new();
            for (what, id) in [(tr("Zusammenfassungen"), self.config.summary_provider.clone()), (tr("Fragen"), self.ask_conn_id())] {
                if let Some(c) = self.config.connection(&id).filter(|c| !c.is_local()) {
                    cloud.push(format!("{what} → {}", c.name));
                }
            }
            let locals: Vec<Connection> = self.config.connections.iter().filter(|c| c.is_local() && self.configured(c)).cloned().collect();
            let mut actions = h_flex().flex_wrap().gap_2();
            if locals.is_empty() {
                actions = actions.child(
                    Button::new("mode-add-local")
                        .small()
                        .outline()
                        .icon(IconName::Plus)
                        .label(tr("Füge eine lokale Verbindung hinzu (z. B. Ollama)"))
                        .on_click(cx.listener(|this, _, window, cx| this.open_form(None, "ollama", window, cx))),
                );
            } else {
                for c in locals {
                    let id = c.id.clone();
                    actions = actions.child(
                        Button::new(SharedString::from(format!("use-local-{}", c.id)))
                            .small()
                            .outline()
                            .label(trf("{} verwenden", &[&c.name]))
                            .on_click(cx.listener(move |this, _, _, cx| this.use_local_ai(id.clone(), cx))),
                    );
                }
            }
            hint = Some(
                v_flex()
                    .gap_2()
                    .p_3()
                    .rounded(theme.radius)
                    .bg(theme.warning.opacity(0.1))
                    .text_sm()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Icon::new(IconName::TriangleAlert).small().text_color(theme.warning).flex_none())
                            .child(div().flex_1().min_w_0().child(tr("Transkription ist jetzt lokal. Noch in der Cloud:"))),
                    )
                    .children(cloud.into_iter().map(|t| div().pl_6().text_xs().child(t)))
                    .child(actions)
                    .into_any_element(),
            );
        } else if self.chosen_mode == Some(Mode::Cloud) && mode != Mode::Cloud {
            hint = Some(
                h_flex()
                    .gap_2()
                    .p_3()
                    .rounded(theme.radius)
                    .bg(theme.warning.opacity(0.1))
                    .text_sm()
                    .child(Icon::new(IconName::TriangleAlert).small().text_color(theme.warning).flex_none())
                    .child(div().flex_1().min_w_0().child(tr(
                        "Keine Verbindung mit Cloud-Transkription eingerichtet. Richte OpenAI, Groq oder einen eigenen Whisper-Server ein.",
                    )))
                    .into_any_element(),
            );
        }

        let live = self.config.live_engine.clone();
        let fin = self.config.final_engine.clone();
        let live_picker = self.stt_picker(Slot::Live, "stt-live", cx);
        let final_picker = self.stt_picker(Slot::Final, "stt-final", cx);
        let live_hint = self.stt_hint(&live);
        let final_hint = self.stt_hint(&fin);
        let live_custom = self.custom_model_row(Slot::Live, cx);
        let final_custom = self.custom_model_row(Slot::Final, cx);
        let row = |label: &str, hint: Option<&str>, control: AnyElement| settings_row(label, hint, control, &theme);

        ui::card(cx)
            .child(div().font_semibold().child(tr("Verarbeitung")))
            .child(tiles)
            .children(hint)
            .child(row(tr("Live-Transkription"), Some(&live_hint), live_picker))
            .children(live_custom)
            .when(!live.is_local(), |el| {
                el.child(div().text_xs().text_color(theme.muted_foreground).child(tr("Live-Text ohne Zwischenstände (Cloud rechnet pro Anfrage ab)")))
            })
            .child(row(tr("Feinschliff nach der Aufnahme"), Some(&final_hint), final_picker))
            .children(final_custom)
            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(
                "Bei einem Fehler wird nichts automatisch umgestellt: Mostalgia zeigt den Fehler, die Aufnahme bleibt gespeichert und kann mit „Erneut transkribieren“ nachbearbeitet werden.",
            )))
    }

    fn render_connections(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mut tiles: Vec<AnyElement> = Vec::new();
        for c in self.config.connections.clone() {
            let configured = self.configured(&c);
            let key = self.keys.as_ref().and_then(|k| k.get(&c.id).cloned());
            let status = self.status.get(&c.id).cloned();
            let uses = self.uses(&c.id);
            let in_use = uses.iter().any(|u| *u);
            let (chip_el, note, error) = match (&status, &key) {
                (Some(ConnStatus::Testing), _) => (chip(tr("Prüfe …"), theme.muted_foreground, &theme), None, None),
                (Some(ConnStatus::Ok(note)), _) => (chip(tr("Verbunden"), theme.success, &theme), Some(note.clone()), None),
                (Some(ConnStatus::Failed(e)), _) => (chip(tr("Fehler"), theme.danger, &theme), None, Some(e.clone())),
                (None, Some(KeyState::Error(e))) if c.needs_key() => (chip(tr("Fehler"), theme.danger, &theme), None, Some(e.clone())),
                _ if self.keys.is_none() => (chip(tr("Prüfe …"), theme.muted_foreground, &theme), None, None),
                _ if !configured => (chip(tr("Nicht eingerichtet"), theme.muted_foreground, &theme), None, None),
                _ => (chip(tr("Nicht geprüft"), theme.muted_foreground, &theme), None, None),
            };
            let (privacy, privacy_color) = if c.is_local() {
                (mostalgia_core::connections::local_label(c.base()).to_string(), theme.success)
            } else if uses[2] || uses[3] {
                (trf("Cloud · Text und Audio gehen an {}", &[&c.name]), theme.info)
            } else {
                (trf("Cloud · Text geht an {}", &[&c.name]), theme.info)
            };
            let used_for: Vec<AnyElement> = ["Zusammenfassungen", "Fragen", "Live-Transkription", "Feinschliff"]
                .into_iter()
                .zip(uses)
                .filter(|(_, on)| *on)
                .map(|(label, _)| {
                    div()
                        .px_1p5()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .text_xs()
                        .child(tr(label))
                        .into_any_element()
                })
                .collect();
            let preset_name = tr(c.preset().name);
            let failed = error.is_some();
            let (test_id, edit_id) = (c.id.clone(), c.id.clone());
            let remove_id = c.id.clone();
            let testing = matches!(status, Some(ConnStatus::Testing));
            tiles.push(
                v_flex()
                    .flex_1()
                    .min_w(px(260.))
                    .p_3()
                    .gap_2()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(if failed { theme.danger.opacity(0.5) } else { theme.border })
                    .child(
                        h_flex()
                            .flex_wrap()
                            .justify_between()
                            .gap_2()
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .gap_2()
                                    .child(div().font_semibold().child(c.name.clone()))
                                    .when(preset_name != c.name, |el| el.child(div().text_xs().text_color(theme.muted_foreground).child(preset_name))),
                            )
                            .child(chip_el),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(if c.base().is_empty() { tr("Keine URL").to_string() } else { c.base().to_string() }),
                    )
                    .child(
                        h_flex()
                            .gap_1p5()
                            .text_xs()
                            .text_color(privacy_color)
                            .child(Icon::new(if c.is_local() { IconName::HardDrive } else { IconName::Globe }).xsmall().flex_none())
                            .child(div().flex_1().min_w_0().child(privacy)),
                    )
                    .when_some(note, |el, n| el.child(div().text_xs().text_color(theme.muted_foreground).child(n)))
                    .when_some(error, |el, e| el.child(div().w_full().text_xs().text_color(theme.danger).child(e)))
                    .when(!used_for.is_empty(), |el| {
                        el.child(
                            h_flex()
                                .flex_wrap()
                                .gap_1()
                                .child(div().text_xs().text_color(theme.muted_foreground).child(tr("Verwendet für:")))
                                .children(used_for),
                        )
                    })
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_1()
                            .child(
                                Button::new(SharedString::from(format!("conn-test-{}", c.id)))
                                    .xsmall()
                                    .outline()
                                    .icon(IconName::RefreshCw)
                                    .label(tr("Testen"))
                                    .loading(testing)
                                    .on_click(cx.listener(move |this, _, _, cx| this.test_connection(test_id.clone(), cx))),
                            )
                            .child(
                                Button::new(SharedString::from(format!("conn-edit-{}", c.id)))
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::Pencil)
                                    .label(tr("Bearbeiten"))
                                    .on_click(cx.listener(move |this, _, window, cx| this.open_form(Some(edit_id.clone()), "custom", window, cx))),
                            )
                            .child(
                                Button::new(SharedString::from(format!("conn-remove-{}", c.id)))
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::Trash)
                                    .label(tr("Entfernen"))
                                    .disabled(in_use)
                                    .when(in_use, |b| b.tooltip(tr("Wird noch verwendet — erst eine andere Verbindung wählen")))
                                    .on_click(cx.listener(move |this, _, _, cx| this.remove_connection(remove_id.clone(), cx))),
                            ),
                    )
                    .into_any_element(),
            );
        }
        let form = self.form.is_some().then(|| self.render_form(cx));
        ui::card(cx)
            .child(
                h_flex()
                    .flex_wrap()
                    .justify_between()
                    .gap_2()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w(px(200.))
                            .child(div().font_semibold().child(tr("Verbindungen")))
                            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(
                                "Claude, OpenAI, Ollama, LiteLLM oder jeder OpenAI-kompatible Endpunkt. Schlüssel liegen im GNOME-Schlüsselbund.",
                            ))),
                    )
                    .when(self.form.is_none(), |el| {
                        el.child(
                            Button::new("conn-add")
                                .small()
                                .primary()
                                .icon(IconName::Plus)
                                .label(tr("Verbindung hinzufügen"))
                                .on_click(cx.listener(|this, _, window, cx| this.open_form(None, "ollama", window, cx))),
                        )
                    }),
            )
            .children(form)
            .child(h_flex().flex_wrap().items_start().gap_3().children(tiles))
    }

    fn render_form(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(form) = &self.form else { return div().into_any_element() };
        let p = connections::preset(form.preset);
        let editing = form.editing.clone();
        let existing = editing.as_ref().and_then(|id| self.config.connection(id).cloned());
        let probe = existing.clone().unwrap_or_else(|| Connection::from_preset(p, &connections::new_id(&self.config.connections, p.id)));
        let from_env = probe.key_from_env();
        let (has_key, remove_key, error) = (form.has_key, form.remove_key, form.error.clone());
        let field = |label: &str, input: &Entity<InputState>, hint: Option<String>| {
            v_flex()
                .gap_1()
                .child(div().text_sm().child(label.to_string()))
                .child(Input::new(input).small())
                .when_some(hint, |el, h| el.child(div().text_xs().text_color(theme.muted_foreground).child(h)))
        };
        let key_hint = if from_env {
            Some(match p.env_var {
                Some(var) => format!("{} ({var})", tr("Aus der Umgebungsvariable")),
                None => tr("Aus der Umgebungsvariable").to_string(),
            })
        } else if remove_key {
            Some(tr("Wird beim Speichern entfernt.").to_string())
        } else if p.needs_key && !p.key_url.is_empty() {
            Some(trf("Schlüssel erstellen: {}", &[&p.key_url]))
        } else if !p.needs_key {
            Some(tr("Nur nötig, wenn der Server einen Schlüssel verlangt.").to_string())
        } else {
            None
        };
        let title = match &existing {
            Some(c) => trf("{} bearbeiten", &[&c.name]),
            None => tr("Neue Verbindung").to_string(),
        };
        let mut presets: Vec<AnyElement> = Vec::new();
        if editing.is_none() {
            for preset in PRESETS {
                let on = preset.id == p.id;
                let id = preset.id;
                presets.push(
                    Button::new(SharedString::from(format!("preset-{id}")))
                        .xsmall()
                        .label(tr(preset.name))
                        .when(on, |b| b.primary())
                        .when(!on, |b| b.outline())
                        .on_click(cx.listener(move |this, _, window, cx| this.open_form(None, id, window, cx)))
                        .into_any_element(),
                );
            }
        }
        let url_hint = if p.id == "custom" {
            Some(tr("Pflichtfeld, z. B. http://server:8000/v1").to_string())
        } else {
            Some(tr("Vorgabe des Anbieters; bei Bedarf anpassen.").to_string())
        };
        let form_url = self.form_url.read(cx).value().to_string();
        let local = connections::is_local_url(&form_url);
        v_flex()
            .p_3()
            .gap_3()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.primary.opacity(0.5))
            .bg(theme.primary.opacity(0.04))
            .child(div().font_semibold().child(title))
            .when(!presets.is_empty(), |el| {
                el.child(v_flex().gap_1().child(div().text_sm().child(tr("Anbieter"))).child(h_flex().flex_wrap().gap_1().children(presets)))
            })
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_3()
                    .items_start()
                    .child(div().flex_1().min_w(px(200.)).child(field(tr("Name"), &self.form_name, None)))
                    .child(div().flex_1().min_w(px(240.)).child(field(tr("Base-URL"), &self.form_url, url_hint))),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_3()
                    .items_end()
                    .child(div().flex_1().min_w(px(240.)).child(field(tr("API-Schlüssel"), &self.form_key, key_hint)))
                    .when(has_key && !from_env, |el| {
                        el.child(
                            Button::new("form-remove-key")
                                .small()
                                .ghost()
                                .label(if remove_key { tr("Schlüssel behalten") } else { tr("Schlüssel entfernen") })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(f) = &mut this.form {
                                        f.remove_key = !f.remove_key;
                                    }
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .when(p.api == Api::OpenAi, |el| {
                el.child(field(
                    tr("Eigene Modell-IDs (optional, mit Komma getrennt)"),
                    &self.form_models,
                    Some(tr("Nur nötig, wenn der Server keine Modellliste liefert").to_string()),
                ))
            })
            .child(
                h_flex()
                    .gap_1p5()
                    .text_xs()
                    .text_color(if local { theme.success } else { theme.muted_foreground })
                    .child(Icon::new(if local { IconName::HardDrive } else { IconName::Globe }).xsmall().flex_none())
                    .child(div().flex_1().min_w_0().child(if local {
                        connections::local_label(&form_url)
                    } else {
                        tr("Cloud · Text (und bei Cloud-Transkription Audio) geht an diesen Anbieter")
                    })),
            )
            .when_some(error, |el, e| el.child(div().text_sm().text_color(theme.danger).child(e)))
            .child(
                h_flex()
                    .gap_2()
                    .child(Button::new("form-save").small().primary().label(tr("Speichern")).on_click(cx.listener(|this, _, window, cx| this.save_form(window, cx))))
                    .child(Button::new("form-cancel").small().ghost().label(tr("Abbrechen")).on_click(cx.listener(|this, _, _, cx| {
                        this.form = None;
                        cx.notify();
                    }))),
            )
            .into_any_element()
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let lang = self.config.language.clone();
        let threshold = self.config.vad_threshold;
        let choice = |id: &'static str, label: &'static str, on: bool, cx: &mut Context<Self>, f: fn(&mut Config)| {
            Button::new(id)
                .small()
                .label(tr(label))
                .when(on, |b| b.primary())
                .when(!on, |b| b.ghost())
                .on_click(cx.listener(move |this, _, _, cx| {
                    f(&mut this.config);
                    this.save(cx);
                }))
        };
        let theme = cx.theme().clone();
        let row = |label: &str, hint: Option<&str>, control: AnyElement| settings_row(label, hint, control, &theme);
        let switch = |id: &'static str, on: bool, cx: &mut Context<Self>, f: fn(&mut Config, bool)| {
            Switch::new(id)
                .checked(on)
                .on_change(cx.listener(move |this, v: &bool, _, cx| {
                    f(&mut this.config, *v);
                    this.save(cx);
                }))
                .into_any_element()
        };
        let title = |t: &'static str| div().font_semibold().child(tr(t));
        let mut cards: Vec<Card> = Vec::new();

        // ── Aufnahme ──
        cards.push(Card {
            category: Category::Aufnahme,
            title: "Mikrofon",
            keywords: "mikrofon gerät eingang test pegel yeti microphone mic device input level",
            wide: false,
            element: self.render_devices(Kind::Mic, cx).into_any_element(),
        });
        cards.push(Card {
            category: Category::Aufnahme,
            title: "Desktop-Audio",
            keywords: "desktop systemaudio ausgabe monitor gerät test pegel system audio output speaker device level",
            wide: false,
            element: self.render_devices(Kind::System, cx).into_any_element(),
        });
        let recognition = ui::card(cx)
            .child(title("Sprache & Erkennung"))
            .child(row(
                tr("Sprache"),
                Some(tr("Empfohlen: „Automatisch erkennen“ – erkennt die Sprache der Besprechung selbst.")),
                h_flex()
                    .gap_1()
                    .child(choice("set-de", "Deutsch", lang == "de", cx, |c| c.language = "de".into()))
                    .child(choice("set-en", "English", lang == "en", cx, |c| c.language = "en".into()))
                    .child(choice("set-auto", "Automatisch erkennen", lang == "auto", cx, |c| c.language = "auto".into()))
                    .into_any_element(),
            ))
            .child(row(
                tr("Spracherkennung (VAD)"),
                Some(tr("Empfindlicher erkennt leise Stimmen, aber auch mehr Geräusche.")),
                h_flex()
                    .gap_1()
                    .child(choice("vad-low", "Zurückhaltend", threshold > 0.55, cx, |c| c.vad_threshold = 0.65))
                    .child(choice("vad-mid", "Normal", (0.45..=0.55).contains(&threshold), cx, |c| c.vad_threshold = 0.5))
                    .child(choice("vad-high", "Empfindlich", threshold < 0.45, cx, |c| c.vad_threshold = 0.35))
                    .into_any_element(),
            ))
            .child(row(
                tr("Vorläufigen Text anzeigen"),
                Some(tr("Grauer Text schon während des Sprechens, ersetzt beim Satzende.")),
                switch("set-interim", self.config.interim_text, cx, |c, v| c.interim_text = v),
            ))
            .child(row(
                tr("Rauschunterdrückung (Mikrofon)"),
                Some(tr("Filtert Lüfter und Brummen vor der Erkennung; die Aufnahme bleibt roh.")),
                switch("set-denoise", self.config.denoise, cx, |c, v| c.denoise = v),
            ));
        cards.push(Card {
            category: Category::Aufnahme,
            title: "Sprache & Erkennung",
            keywords: "sprache deutsch english automatisch vad spracherkennung empfindlich vorläufig interim rauschunterdrückung lüfter language german speech detection sensitivity provisional text noise suppression fan",
            wide: false,
            element: recognition.into_any_element(),
        });

        // Auto-start with calls.
        let mode = self.config.call_autostart.clone();
        let mode_button = |id: &'static str, label: &'static str, value: &'static str, cx: &mut Context<Self>| {
            Button::new(id)
                .small()
                .label(tr(label))
                .when(mode == value, |b| b.primary())
                .when(mode != value, |b| b.ghost())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.config.call_autostart = value.into();
                    this.save(cx);
                }))
        };
        let mut app_rows: Vec<AnyElement> = Vec::new();
        for (i, app) in self.config.call_apps_seen.clone().into_iter().enumerate() {
            let watched = !self.config.call_apps_ignored.contains(&app);
            let name = app.clone();
            app_rows.push(row(
                &app,
                None,
                Switch::new(SharedString::from(format!("call-app-{i}")))
                    .checked(watched)
                    .on_change(cx.listener(move |this, on: &bool, _, cx| {
                        this.config.call_apps_ignored.retain(|a| a != &name);
                        if !*on {
                            this.config.call_apps_ignored.push(name.clone());
                        }
                        this.save(cx);
                    }))
                    .into_any_element(),
            ).into_any_element());
        }
        let calls = ui::card(cx)
            .child(title("Anrufe erkennen"))
            .child(div().text_sm().text_color(theme.muted_foreground).child(tr(
                "Bemerkt, wenn Zoom, Teams, Meet im Browser, Discord & Co. das Mikrofon benutzen.",
            )))
            .child(row(
                tr("Bei einem Anruf"),
                None,
                h_flex()
                    .gap_1()
                    .child(mode_button("call-off", "Aus", "aus", cx))
                    .child(mode_button("call-ask", "Fragen", "fragen", cx))
                    .child(mode_button("call-auto", "Automatisch", "automatisch", cx))
                    .into_any_element(),
            ))
            .child(row(
                tr("Automatisch beenden, wenn der Anruf endet"),
                Some(tr("Nur für Aufnahmen, die durch einen Anruf gestartet wurden.")),
                switch("call-stop", self.config.call_stop_auto, cx, |c, v| c.call_stop_auto = v),
            ))
            .child(if app_rows.is_empty() {
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(tr("Erkannte Apps erscheinen hier nach dem ersten Anruf – einzeln abschaltbar."))
                    .into_any_element()
            } else {
                v_flex()
                    .gap_2()
                    .child(div().text_xs().font_semibold().text_color(theme.muted_foreground).child(tr("ERKANNTE APPS")))
                    .children(app_rows)
                    .into_any_element()
            });
        cards.push(Card {
            category: Category::Aufnahme,
            title: "Anrufe erkennen",
            keywords: "anruf anrufe call calls zoom teams meet discord slack auto start automatisch aufnahme starten erkennen detect automatic recording",
            wide: false,
            element: calls.into_any_element(),
        });

        // ── Sprecher ──
        let diarize_ok = mostalgia_core::diarize::available();
        let dt = self.config.diarize_threshold;
        let speakers = ui::card(cx)
            .child(title("Sprecher"))
            .child(row(
                tr("Sprecher bei Live-Text anzeigen"),
                Some(tr("„Ich“ (Mikrofon) und „Andere“ (Desktop) an jeder Zeile.")),
                switch("set-labels", self.config.speaker_labels, cx, |c, v| c.speaker_labels = v),
            ))
            .child(row(
                tr("Mehrere Sprecher unterscheiden"),
                Some(tr(if diarize_ok {
                    "Teilt „Andere“ in Sprecher 1, 2, … auf. Live ungefähr, im fertigen Transkript genau."
                } else {
                    "Modelle fehlen — scripts/download-model.sh ausführen."
                })),
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
                    tr("Trennschärfe"),
                    Some(tr("Werden zwei Personen zusammengelegt: „Mehr Sprecher“. Wird eine Person geteilt: „Weniger“.")),
                    h_flex()
                        .gap_1()
                        .child(choice("dia-less", "Weniger", dt > 0.55, cx, |c| c.diarize_threshold = 0.6))
                        .child(choice("dia-mid", "Normal", (0.45..=0.55).contains(&dt), cx, |c| c.diarize_threshold = 0.5))
                        .child(choice("dia-more", "Mehr Sprecher", dt < 0.45, cx, |c| c.diarize_threshold = 0.4))
                        .into_any_element(),
                ))
            })
            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(
                "Namen vergeben: im Transkript auf einen Sprecher-Chip klicken — alle Zeilen werden umbenannt.",
            )));
        cards.push(Card {
            category: Category::Sprecher,
            title: "Sprecher",
            keywords: "sprecher ich andere label diarisierung trennen mehrere personen umbenennen speaker speakers me others diarization separate people rename",
            wide: false,
            element: speakers.into_any_element(),
        });

        // ── KI & Transkription ──
        cards.push(Card {
            category: Category::Ki,
            title: "Verbindungen",
            keywords: "verbindung verbindungen anbieter provider connection connections endpoint endpunkt url base-url api key schlüssel keyring schlüsselbund claude anthropic openai deepseek ollama litellm lm studio lmstudio vllm openrouter groq together custom eigener lokal local cloud test testen",
            wide: true,
            element: self.render_connections(cx).into_any_element(),
        });
        cards.push(Card {
            category: Category::Ki,
            title: "Verarbeitung",
            keywords: "verarbeitung processing modus mode lokal local hybrid cloud whisper groq openai transkription transcription live feinschliff final stt kosten cost gpu audio",
            wide: true,
            element: self.render_processing(cx).into_any_element(),
        });

        cards.push(Card {
            category: Category::Ki,
            title: "Zusammenfassungen & Fragen",
            keywords: "ki ai zusammenfassung zusammenfassungen summary summaries fragen ask modell model denktiefe thinking effort reasoning automatisch automatic claude openai ollama deepseek lokal local cloud",
            wide: false,
            element: self.render_ai_use(cx).into_any_element(),
        });
        // ── Darstellung ──
        let ui_lang = self.config.ui_language.clone();
        let lang_button = |id: &'static str, label: &'static str, value: &'static str, cx: &mut Context<Self>| {
            Button::new(id)
                .small()
                .label(label)
                .when(ui_lang == value, |b| b.primary())
                .when(ui_lang != value, |b| b.ghost())
                .on_click(cx.listener(move |this, _, window, cx| this.set_ui_language(value, window, cx)))
        };
        let app_language = ui::card(cx)
            .child(title("Sprache der App"))
            .child(row(
                tr("App-Sprache"),
                Some(tr("Zusammenfassungen und Transkripte bleiben in der Sprache der Besprechung.")),
                h_flex()
                    .flex_wrap()
                    .gap_1()
                    .child(lang_button("ui-lang-system", tr("System"), "system", cx))
                    // Language names in their own language.
                    .child(lang_button("ui-lang-de", "Deutsch", "de", cx))
                    .child(lang_button("ui-lang-en", "English", "en", cx))
                    .into_any_element(),
            ));
        cards.push(Card {
            category: Category::Darstellung,
            title: "Sprache der App",
            keywords: "sprache app oberfläche deutsch english englisch language interface ui",
            wide: false,
            element: app_language.into_any_element(),
        });
        cards.push(Card {
            category: Category::Darstellung,
            title: "Mori",
            keywords: "mori companion assistant anime character notes avatar begleiterin figur notizen live punkte points",
            wide: false,
            element: self.render_mori(cx).into_any_element(),
        });
        cards.push(Card {
            category: Category::Darstellung,
            title: "Design",
            keywords: "design theme farbe hell dunkel blockwelt schrift schriftart animationen reduzieren color light dark font animations reduce motion sprache app language",
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
                    .child(Button::new("open-data").small().outline().icon(IconName::FolderOpen).label(tr("Öffnen")).on_click(move |_, _, cx| {
                        cx.open_with_system(&data_dir);
                    })),
            )
            .child(div().text_xs().text_color(theme.muted_foreground).child(tr("Datenbank, Aufnahmen (WAV) und Modelle liegen hier.")));
        cards.push(Card {
            category: Category::Speicher,
            title: "Speicherort",
            keywords: "speicher ordner daten datenbank aufnahmen wav modelle storage folder data database recordings models",
            wide: false,
            element: storage.into_any_element(),
        });
        let export = ui::card(cx)
            .child(title("Export"))
            .child(div().text_sm().child(tr("In jeder Besprechung über „Exportieren“: Zusammenfassung und Transkript als")))
            .child(
                v_flex()
                    .gap_1()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .children(mostalgia_core::export::Format::ALL.map(|f| h_flex().gap_2().child(Icon::new(IconName::FileText).xsmall()).child(tr(f.label())))),
            )
            .child(div().text_xs().text_color(theme.muted_foreground).child(tr("Im Terminal: mostalgia export <id> --format md|txt|srt|docx")));
        cards.push(Card {
            category: Category::Speicher,
            title: "Export",
            keywords: "export markdown text srt untertitel word docx subtitles",
            wide: false,
            element: export.into_any_element(),
        });

        // ── Tastenkürzel & System ──
        let shortcut = ui::card(cx)
            .child(title("Tastenkürzel"))
            .child(div().text_sm().text_color(theme.muted_foreground).child(tr("Startet oder stoppt eine Aufnahme von überall – auch wenn die App geschlossen ist.")))
            .child(match &self.shortcut {
                Some(b) => h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(Icon::new(IconName::Check).small().text_color(theme.success))
                    .child(div().text_sm().child(tr("Eingerichtet:")))
                    .child(div().px_2().py_0p5().rounded(theme.radius).border_1().border_color(theme.border).text_sm().font_family(theme.mono_font_family.clone()).child(pretty_binding(b)))
                    .into_any_element(),
                None => h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(div().text_sm().child(tr("Noch nicht eingerichtet.")))
                    .child(Button::new("install-shortcut").small().primary().icon(IconName::Keyboard).label(tr("Super + Shift + R einrichten")).on_click(cx.listener(|this, _, _, cx| {
                        match install_shortcut() {
                            Ok(b) => this.shortcut = Some(b),
                            Err(e) => this.shortcut_error = Some(format!("{e:#}")),
                        }
                        cx.notify();
                    })))
                    .into_any_element(),
            })
            .when_some(self.shortcut_error.clone(), |el, e| el.child(div().text_xs().text_color(theme.danger).child(e)))
            .child(div().text_xs().text_color(theme.muted_foreground).child(tr("Ändern oder entfernen: GNOME-Einstellungen → Tastatur → Eigene Tastenkürzel.")));
        cards.push(Card {
            category: Category::System,
            title: "Tastenkürzel",
            keywords: "tastenkürzel shortcut taste super toggle starten stoppen keyboard key start stop",
            wide: false,
            element: shortcut.into_any_element(),
        });
        let tray = ui::card(cx)
            .child(title("Tray-Symbol"))
            .child(match self.tray_available {
                Some(true) => h_flex().gap_2().child(Icon::new(IconName::Check).small().text_color(theme.success)).child(div().text_sm().child(tr("Aktiv – oben in der Leiste."))).into_any_element(),
                Some(false) => v_flex()
                    .gap_1()
                    .child(h_flex().gap_2().child(Icon::new(IconName::TriangleAlert).small().text_color(theme.warning)).child(div().text_sm().child(tr("GNOME zeigt gerade keine Tray-Symbole an."))))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(tr("Erweiterung einschalten und einmal ab- und wieder anmelden:")))
                    .child(div().text_xs().font_family(theme.mono_font_family.clone()).child("gnome-extensions enable ubuntu-appindicators@ubuntu.com"))
                    .into_any_element(),
                None => div().text_sm().text_color(theme.muted_foreground).child(tr("Wird geprüft …")).into_any_element(),
            });
        cards.push(Card {
            category: Category::System,
            title: "Tray-Symbol",
            keywords: "tray symbol leiste appindicator gnome icon panel",
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
            .child(div().text_sm().child(format!("Mostalgia {}", env!("CARGO_PKG_VERSION"))))
            .child(status_row(model.exists(), format!("Whisper: {}", model.display())))
            .child(status_row(self.config.vad_model_path().exists(), "Silero VAD".into()))
            .child(status_row(diarize_ok, tr("Sprechertrennung (sherpa-onnx)").into()))
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(IconName::Cpu).xsmall())
                    .child(self.gpu.clone().unwrap_or_else(|| tr("GPU wird bei der ersten Aufnahme erkannt").into())),
            );
        cards.push(Card {
            category: Category::System,
            title: "Info",
            keywords: "info version modell whisper gpu vad sherpa status model about",
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
                    c.title.to_lowercase().contains(&query)
                        || tr(c.title).to_lowercase().contains(&query)
                        || c.keywords.contains(&query)
                        || c.category.title().to_lowercase().contains(&query)
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
            .child(div().px_2().pb_2().text_lg().font_semibold().child(tr("Einstellungen")))
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
            v_flex().child(div().text_2xl().font_semibold().child(trf("Suche: „{}“", &[&self.query.trim()])))
        } else {
            v_flex()
                .child(div().text_2xl().font_semibold().child(self.category.title()))
                .child(div().text_sm().text_color(theme.muted_foreground).child(self.category.subtitle()))
        };

        h_flex()
            .size_full()
            .key_context("Settings")
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
                                .when(empty, |el| el.child(div().text_color(theme.muted_foreground).child(tr("Keine passende Einstellung gefunden.")))),
                        ),
                ),
            )
    }
}

/// A setting row: label (+ hint) and its control side by side; when the card
/// is too narrow the control wraps below the label instead of squeezing it.
/// The label never gets narrower than ~190 px and wraps by word.
fn settings_row(label: &str, hint: Option<&str>, control: AnyElement, theme: &gpui_kit::component::Theme) -> Div {
    h_flex()
        .w_full()
        .flex_wrap()
        .justify_between()
        .items_center()
        .gap_x_4()
        .gap_y_2()
        .child(
            v_flex()
                .flex_1()
                .min_w(px(190.))
                .child(div().text_sm().child(label.to_string()))
                .when_some(hint, |el, h| el.child(div().text_xs().text_color(theme.muted_foreground).child(h.to_string()))),
        )
        .child(div().flex_none().max_w_full().child(control))
}

/// Display name of a built-in theme in the UI language (user themes keep
/// their own name).
fn theme_name(id: &str, name: &str) -> String {
    let de = match id {
        "hell" => "Hell",
        "dunkel" => "Dunkel",
        "papier" => "Papier",
        "kontrast" => "Hoher Kontrast",
        "ozean" => "Ozean",
        "wald" => "Wald",
        "nachtschatten" => "Nachtschatten",
        "kaffee" => "Kaffee",
        "retro90" => "Retro 90er",
        "weltraum" => "Weltraum",
        "blockwelt" => "Blockwelt",
        _ => return name.to_string(),
    };
    tr(de).to_string()
}
