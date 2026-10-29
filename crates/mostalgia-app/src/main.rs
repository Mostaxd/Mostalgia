use gpui_kit::component::{Root, TitleBar};
use gpui_kit::*;
use mostalgia_app::{AppAssets, app, load_fonts, ui};

actions!(mostalgia, [Quit]);

fn main() {
    // Mori on the desktop: her own small always-on-top window (started by
    // the app; `--demo` runs her alone with a fake recording).
    if std::env::args().any(|a| a == "--mori") {
        mostalgia_app::companion::desktop::run(std::env::args().any(|a| a == "--demo"));
        return;
    }
    // The floating live captions (started by the app during a recording).
    if std::env::args().any(|a| a == "--captions") {
        mostalgia_app::companion::captions::run(std::env::args().any(|a| a == "--demo"));
        return;
    }
    gpui_kit::application().with_assets(AppAssets).run(|cx: &mut App| {
        gpui_kit::init(cx);
        let config = mostalgia_core::config::Config::load();
        mostalgia_core::i18n::set_lang(mostalgia_core::i18n::resolve(&config.ui_language));
        load_fonts(cx);

        cx.bind_keys([
            KeyBinding::new("ctrl-q", Quit, None),
            KeyBinding::new("ctrl-k", app::OpenAsk, None),
            KeyBinding::new("ctrl-=", app::ZoomIn, None),
            KeyBinding::new("ctrl-+", app::ZoomIn, None),
            KeyBinding::new("ctrl-shift-=", app::ZoomIn, None),
            KeyBinding::new("ctrl--", app::ZoomOut, None),
            KeyBinding::new("ctrl-0", app::ZoomReset, None),
            KeyBinding::new("delete", app::DeleteSelected, Some("Library")),
        ]);
        // macOS: the same with the Command key, and quitting with the
        // window (as elsewhere) rather than living on without one.
        #[cfg(target_os = "macos")]
        {
            cx.bind_keys([
                KeyBinding::new("cmd-q", Quit, None),
                KeyBinding::new("cmd-k", app::OpenAsk, None),
                KeyBinding::new("cmd-=", app::ZoomIn, None),
                KeyBinding::new("cmd-+", app::ZoomIn, None),
                KeyBinding::new("cmd-shift-=", app::ZoomIn, None),
                KeyBinding::new("cmd--", app::ZoomOut, None),
                KeyBinding::new("cmd-0", app::ZoomReset, None),
                KeyBinding::new("cmd-backspace", app::DeleteSelected, Some("Library")),
            ]);
            cx.set_quit_mode(QuitMode::LastWindowClosed);
        }
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_action(|_: &app::ZoomIn, cx| app::MostalgiaApp::zoom_unfocused(Some(true), cx));
        cx.on_action(|_: &app::ZoomOut, cx| app::MostalgiaApp::zoom_unfocused(Some(false), cx));
        cx.on_action(|_: &app::ZoomReset, cx| app::MostalgiaApp::zoom_unfocused(None, cx));

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1240.), px(820.)), cx)),
            window_min_size: Some(size(px(860.), px(560.))),
            app_id: Some("io.github.mostaxd.Mostalgia".into()),
            ..TitleBar::window_options()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            ui::apply_theme(window, cx);
            window.set_window_title("Mostalgia");
            let view = cx.new(|cx| app::MostalgiaApp::new(window, cx));
            // `mostalgia-app --start` (what `mostalgia toggle` launches) records right away.
            if std::env::args().any(|a| a == "--start") {
                view.update(cx, |app, cx| app.start_recording(cx));
            }
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("failed to open the window");
        cx.activate(true);
    });
}
