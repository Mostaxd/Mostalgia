//! Renders the documents drive and the live answer helper offscreen
//! (light and dark), using the library in $MOSTALGIA_DATA_DIR. Run with:
//!
//!     cargo run -p mostalgia-app --example docs_screenshots --features screenshots -- <out-dir> [doc-id loc]

use std::sync::Arc;

use gpui_kit::component::Root;
use gpui_kit::*;
use mostalgia_app::{app::MostalgiaApp, load_fonts};
use mostalgia_core::Source;

fn main() {
    let mut args = std::env::args().skip(1);
    let out = std::path::PathBuf::from(args.next().unwrap_or_else(|| "screenshots".into()));
    let doc: Option<i64> = args.next().and_then(|s| s.parse().ok());
    let loc: Option<u32> = args.next().and_then(|s| s.parse().ok());
    std::fs::create_dir_all(&out).unwrap();
    // SAFETY: set before any other thread starts.
    unsafe { std::env::set_var("MOSTALGIA_NO_BACKFILL", "1") };
    // SAFETY: set before any other thread starts.
    unsafe { std::env::set_var("MOSTALGIA_NO_CALLWATCH", "1") };
    let mut cx = HeadlessAppContext::with_platform(
        gpui_platform::current_platform(true).text_system(),
        Arc::new(mostalgia_app::AppAssets),
        gpui_platform::current_headless_renderer,
    );
    cx.update(|cx| {
        gpui_kit::init(cx);
        load_fonts(cx);
    });
    mostalgia_core::i18n::set_lang(mostalgia_core::i18n::resolve(&std::env::var("UI_LANG").unwrap_or_else(|_| "en".into())));
    for theme in ["mostalgia", "mostalgia-nacht"] {
        let spec = mostalgia_app::themes::resolve(theme, false);
        cx.update(|cx| mostalgia_app::themes::apply(&spec, cx));
        let (handle, view) = cx.update(|cx| {
            let mut app = None;
            let h = gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds { origin: Default::default(), size: size(px(1240.), px(std::env::var("HEIGHT").ok().and_then(|h| h.parse().ok()).unwrap_or(820.))) })),
                    focus: false,
                    show: false,
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    let view = cx.new(|cx| MostalgiaApp::new(window, cx));
                    app = Some(view.clone());
                    cx.new(|cx| Root::new(view, window, cx))
                },
            )
            .unwrap();
            (h.0, app.unwrap())
        });
        let handle: AnyWindowHandle = handle;
        cx.update(|cx| mostalgia_app::themes::apply(&spec, cx));
        let shot = |name: &str, cx: &mut HeadlessAppContext| {
            cx.run_until_parked();
            cx.update_window(handle, |_, window, _| window.refresh()).unwrap();
            cx.run_until_parked();
            let img = cx.capture_screenshot(handle).expect("offscreen rendering");
            let path = out.join(format!("{name}-{theme}.png"));
            img.save(&path).unwrap();
            println!("{}", path.display());
        };

        // The documents drive, with a document open at a cited page.
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |app, cx| match doc {
                Some(id) => app.open_document(id, loc, window, cx),
                None => {
                    app.open_documents(window, cx);
                }
            })
        })
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        shot("20-documents", &mut cx);

        // An answer in "Ask" citing a meeting and a document.
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |app, cx| app.open_ask(window, cx));
            if let Some(ask) = view.read(cx).ask_view() {
                let d = doc.unwrap_or(1);
                ask.update(cx, |a, cx| {
                    a.demo(
                        "How often does the VPN password change, and who do I ask for access?",
                        &format!("The VPN password expires every 90 days [[d:{d}@{}]]. New access is requested through the IT portal [[d:{d}@{}]]; in the last sync Anna offered to set it up for the new hires [[m:1@36]].", loc.unwrap_or(1), loc.unwrap_or(1)),
                        cx,
                    )
                });
            }
        })
        .unwrap();
        shot("21-ask-documents", &mut cx);

        // Settings → Companion with the live answers card.
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |app, cx| app.open_settings(window, cx));
            if let Some(s) = view.read(cx).settings_view() {
                s.update(cx, |s, cx| s.show_category("begleiterin", cx));
            }
        })
        .unwrap();
        shot("23-settings-companion", &mut cx);

        // A recording with a question from the others and her suggestion.
        cx.update_window(handle, |_, _, cx| {
            view.update(cx, |app, cx| app.show_recording(cx));
            let rec = view.read(cx).recording_view();
            rec.update(cx, |r, cx| {
                r.demo(
                    64,
                    &[
                        (Source::System, 3.2, "Morning! Quick round before we start with the onboarding."),
                        (Source::Mic, 9.0, "Sure, the new hires start on Monday."),
                        (Source::System, 18.5, "Great. Mosta, how often do they have to change the VPN password?"),
                    ],
                    cx,
                )
            });
        })
        .unwrap();
        if cx.update(|cx| view.read(cx).companion_view().read(cx).is_enabled()) {
            cx.update_window(handle, |_, _, cx| {
                let c = view.read(cx).companion_view();
                let d = doc.unwrap_or(1);
                c.update(cx, |c, cx| {
                    c.demo_pose(Some(mostalgia_app::companion::art::Pose::Talking), cx);
                    c.demo_answer(
                        "Mosta, how often do they have to change the VPN password?",
                        18.5,
                        &format!("Every 90 days – the password expires automatically [[d:{d}@{}]]. New access goes through the IT portal [[d:{d}@{}]].", loc.unwrap_or(1), loc.unwrap_or(1)),
                        cx,
                    );
                });
            })
            .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(600));
            shot("22-live-answer", &mut cx);
        }

    }
}
