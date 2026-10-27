//! mostalgia desktop app: library, live recording, meeting detail, settings.

pub mod app;
pub mod ask_view;
pub mod autostart;
pub mod control;
pub mod meeting;
pub mod recording;
pub mod settings;
pub mod themes;
pub mod ui;

use std::borrow::Cow;

use gpui_kit::*;

const FONTS: [&[u8]; 11] = [
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../assets/fonts/Inter-Bold.ttf"),
    // Selectable UI fonts.
    include_bytes!("../assets/fonts/AtkinsonHyperlegible-Regular.ttf"),
    include_bytes!("../assets/fonts/AtkinsonHyperlegible-Bold.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans.ttf"),
    // Timer and timestamps.
    include_bytes!("../assets/fonts/JetBrainsMono.ttf"),
    // Arabic script, as a fallback for every UI font.
    include_bytes!("../assets/fonts/NotoSansArabic.ttf"),
    // Pixel and terminal themes.
    include_bytes!("../assets/fonts/PixelifySans.ttf"),
    include_bytes!("../assets/fonts/VT323-Regular.ttf"),
];

/// Registers the bundled fonts (all SIL OFL).
pub fn load_fonts(cx: &mut App) {
    cx.text_system()
        .add_fonts(FONTS.iter().map(|f| Cow::Borrowed(*f)).collect())
        .expect("loading the bundled Inter font");
}

gpui_kit::assets::icon_assets!(pub ExtraIcons, [
    ArrowDown, AudioLines, Bookmark, Calendar, Check, Clock, Cpu, FileText, FolderOpen, Mic, Monitor, Pause,
    Keyboard, MicOff, Palette, Pencil, Play, Plus, Radio, RefreshCw, Search, Settings, Sparkles, Square, Trash, TriangleAlert,
    Users, X,
]);

/// The default GPUI Kit assets plus the extra Lucide icons the app uses.
pub struct AppAssets;

/// The app icon (64 px PNG), for the title bar and About card.
pub const ICON_PNG: &[u8] = include_bytes!("../../../assets/brand/icons/hicolor/64x64/apps/io.github.mostaxd.Mostalgia.png");
/// Asset path of [`ICON_PNG`] (served by [`AppAssets`]).
pub const ICON_PATH: &str = "brand/icon.png";

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == ICON_PATH {
            return Ok(Some(Cow::Borrowed(ICON_PNG)));
        }
        // The default set reports icons it doesn't bundle as an error.
        match gpui_kit::assets::Assets.load(path) {
            Ok(Some(data)) => Ok(Some(data)),
            default => match ExtraIcons.load(path)? {
                Some(data) => Ok(Some(data)),
                None => default,
            },
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut all = gpui_kit::assets::Assets.list(path).unwrap_or_default();
        all.extend(ExtraIcons.list(path)?);
        all.sort();
        all.dedup();
        Ok(all)
    }
}
