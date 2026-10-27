use gpui_kit::component::{Root, TitleBar};
use gpui_kit::*;
use mostalgia_app::{AppAssets, app, load_fonts, ui};

actions!(mostalgia, [Quit]);

fn main() {
    gpui_kit::application().with_assets(AppAssets).run(|cx: &mut App| {
        gpui_kit::init(cx);
        let config = mostalgia_core::config::Config::load();
        mostalgia_core::i18n::set_lang(mostalgia_core::i18n::resolve(&config.ui_language));
        load_fonts(cx);

        cx.bind_keys([
            KeyBinding::new("ctrl-q", Quit, None),
            KeyBinding::new("ctrl-k", app::OpenAsk, None),
            KeyBinding::new("delete", app::DeleteSelected, Some("Library")),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1240.), px(820.)), cx)),
            window_min_size: Some(size(px(860.), px(560.))),
            app_id: Some("mostalgia".into()),
            ..TitleBar::window_options()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            ui::apply_theme(window, cx);
            window.set_window_title("mostalgia");
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
