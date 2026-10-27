//! Renders the app's screens offscreen to PNGs (light and dark), using the
//! real library database. Run with:
//!
//!     cargo run -p mostalgia-app --example screenshots --features screenshots -- <out-dir> [meeting-id]

use std::sync::Arc;

use gpui_kit::component::Root;
use gpui_kit::*;
use mostalgia_app::{app::MostalgiaApp, load_fonts};
use mostalgia_core::Source;

fn main() {
    let mut args = std::env::args().skip(1);
    let out = std::path::PathBuf::from(args.next().unwrap_or_else(|| "screenshots".into()));
    let meeting: Option<i64> = args.next().and_then(|s| s.parse().ok());
    std::fs::create_dir_all(&out).unwrap();
    // No background index backfill: this process exits right after the shots.
    // SAFETY: set before any other thread starts.
    unsafe { std::env::set_var("MOSTALGIA_NO_BACKFILL", "1") };
    // No call watching / notifications from offscreen windows.
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
    // UI_LANG=en|de picks the app language; WIDTH=<px> the window width.
    mostalgia_core::i18n::set_lang(mostalgia_core::i18n::resolve(&std::env::var("UI_LANG").unwrap_or_else(|_| "de".into())));
    let width: f32 = std::env::var("WIDTH").ok().and_then(|w| w.parse().ok()).unwrap_or(1240.);
    // THEMES=hell,comic limits which themes are rendered.
    let only: Option<Vec<String>> = std::env::var("THEMES").ok().map(|t| t.split(',').map(str::to_string).collect());

    // Every theme on the live-recording screen; the full tour in light and dark.
    let mut runs: Vec<(String, bool)> = vec![("mostalgia".into(), true), ("mostalgia-nacht".into(), true)];
    runs.extend(mostalgia_app::themes::all().into_iter().map(|t| (t.id, false)).filter(|(id, _)| id != "mostalgia" && id != "mostalgia-nacht"));
    for (theme, full) in runs {
        if only.as_ref().is_some_and(|o| !o.contains(&theme)) {
            continue;
        }
        let spec = mostalgia_app::themes::resolve(&theme, false);
        cx.update(|cx| mostalgia_app::themes::apply(&spec, cx));
        let (handle, view) = cx
            .update(|cx| {
                let mut app = None;
                let h = gpui_kit::open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds {
                            origin: Default::default(),
                            size: size(px(width), px(if std::env::var_os("TALL").is_some() { 2200. } else { 820. })),
                        })),
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
        // Opening a window syncs the theme to its (light) headless appearance.
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

        if full {
            shot("1-new-recording", &mut cx);
            // The setup screen with cloud transcription chosen.
            cx.update_window(handle, |_, _, cx| {
                let rec = view.read(cx).recording_view();
                rec.update(cx, |r, cx| {
                    let mut config = mostalgia_core::config::Config::load();
                    let cloud = mostalgia_core::config::SttChoice { connection: "openai".into(), model: "gpt-4o-transcribe".into() };
                    config.live_engine = cloud.clone();
                    config.final_engine = cloud;
                    r.demo_config(config, cx);
                });
            })
            .unwrap();
            shot("1b-new-recording-cloud", &mut cx);
            cx.update_window(handle, |_, _, cx| {
                let rec = view.read(cx).recording_view();
                rec.update(cx, |r, cx| r.demo_config(mostalgia_core::config::Config::load(), cx));
            })
            .unwrap();
        }

        cx.update_window(handle, |_, _, cx| {
            let rec = view.read(cx).recording_view();
            rec.update(cx, |r, cx| {
                r.demo(
                    187,
                    &[
                        (Source::System, 3.2, "Guten Morgen zusammen, schön, dass ihr alle da seid."),
                        (Source::Mic, 8.9, "Morgen! Sollen wir direkt mit dem Budget für das nächste Quartal anfangen?"),
                        (Source::System, 15.4, "Ja, gerne. Wir liegen aktuell etwa zehn Prozent unter Plan, vor allem wegen der Messe im Mai."),
                        (Source::Mic, 27.0, "Okay. Dann schlage ich vor, dass wir die Marketingausgaben bis Ende Juni einfrieren."),
                        (Source::System, 36.8, "Einverstanden. Anna, kannst du bis Freitag eine neue Prognose schicken?"),
                        (Source::System, 44.1, "Klar, mache ich. Ich brauche nur noch die Zahlen aus dem Vertrieb."),
                        (Source::Mic, 52.5, "Die schicke ich dir heute Nachmittag."),
                    ],
                    cx,
                )
            });
        })
        .unwrap();
        // Let the line fade-ins finish.
        std::thread::sleep(std::time::Duration::from_millis(600));
        shot("2-recording", &mut cx);
        if full {
            cx.update_window(handle, |_, _, cx| {
                let rec = view.read(cx).recording_view();
                rec.update(cx, |r, cx| r.demo_engine_error("OpenAI · gpt-4o-transcribe", "OpenAI: nicht erreichbar unter https://api.openai.com/v1 — läuft der Server?", 3, cx));
            })
            .unwrap();
            shot("2b-recording-engine-error", &mut cx);
        }
        // The empty live transcript, where themes draw their scene.
        cx.update_window(handle, |_, _, cx| {
            let rec = view.read(cx).recording_view();
            rec.update(cx, |r, cx| r.demo(4, &[], cx));
        })
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(700));
        shot("7-empty", &mut cx);
        // Each settings category in Hell, Dunkel and one playful theme.
        if full || theme == "comic" {
            cx.update_window(handle, |_, window, cx| {
                view.update(cx, |app, cx| app.open_settings(window, cx));
            })
            .unwrap();
            for cat in mostalgia_app::settings::SettingsView::CATEGORY_IDS {
                cx.update_window(handle, |_, _, cx| {
                    if let Some(s) = view.read(cx).settings_view() {
                        s.update(cx, |s, cx| s.show_category(cat, cx));
                    }
                })
                .unwrap();
                std::thread::sleep(std::time::Duration::from_millis(400));
                shot(&format!("8-settings-{cat}"), &mut cx);
            }
        }
        if !full {
            continue;
        }
        // The delete confirmation for the first meeting.
        cx.update_window(handle, |_, _, cx| {
            view.update(cx, |app, cx| app.request_delete(vec![1], cx));
        })
        .unwrap();
        shot("10-delete", &mut cx);
        cx.update_window(handle, |_, _, cx| {
            view.update(cx, |app, cx| app.request_delete(Vec::new(), cx));
        })
        .unwrap();

        if let Some(id) = meeting {
            cx.update_window(handle, |_, window, cx| {
                view.update(cx, |app, cx| app.open_meeting(id, window, cx));
            })
            .unwrap();
            for (tab, name) in [(0, "3-summary"), (1, "4-transcript"), (2, "5-audio")] {
                cx.update_window(handle, |_, _, cx| {
                    if let Some(m) = view.read(cx).meeting_view() {
                        m.update(cx, |m, cx| m.set_tab(tab, cx));
                    }
                })
                .unwrap();
                // The waveform loads in the background.
                // Let the tab indicator settle; the waveform loads in the background.
                cx.run_until_parked();
                cx.update_window(handle, |_, window, _| window.refresh()).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(if tab == 2 { 800 } else { 400 }));
                shot(name, &mut cx);
            }
            cx.update_window(handle, |_, _, cx| {
                if let Some(m) = view.read(cx).meeting_view() {
                    m.update(cx, |m, cx| m.open_retranscribe(cx));
                }
            })
            .unwrap();
            shot("11-retranscribe", &mut cx);
        }

        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |app, cx| app.open_settings(window, cx));
        })
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        shot("6-settings", &mut cx);

        // "Fragen" with a finished answer (no API call).
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |app, cx| app.open_ask(window, cx));
            if let Some(a) = view.read(cx).ask_view() {
                a.update(cx, |a, cx| {
                    a.demo(
                        "Was wurde über die Marketingausgaben entschieden?",
                        "Die Marketingausgaben werden bis Ende Juni eingefroren [[m:1@13]]. Grund: Das Budget liegt etwa zehn Prozent unter Plan, vor allem wegen der Messe [[m:1@6]].\n- Vorschlag kam von Sprecher 1, Sprecher 2 stimmte zu [[m:1@19]].\n- Eine neue Prognose wird an alle verschickt; die Frist ist im Transkript unklar [[m:1@19]].",
                        cx,
                    )
                });
            }
        })
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        shot("9-ask", &mut cx);
    }
}
