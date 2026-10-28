//! Renders the app's screens offscreen to PNGs (light and dark), using the
//! library database in $MOSTALGIA_DATA_DIR (point it at a demo library).
//! Run with:
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
    mostalgia_core::i18n::set_lang(mostalgia_core::i18n::resolve(&std::env::var("UI_LANG").unwrap_or_else(|_| "en".into())));
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

        // TEXTIMPORT=<ids>: only "meeting from text" — the dialog, empty and
        // filled with a Teams-style transcript, then the given imported
        // meetings (summary, transcript, notes).
        if let Ok(ids) = std::env::var("TEXTIMPORT") {
            cx.update_window(handle, |_, window, cx| view.update(cx, |app, cx| app.open_text_import(window, cx))).unwrap();
            shot("20-text-import-empty", &mut cx);
            cx.update_window(handle, |_, window, cx| {
                if let Some(t) = view.read(cx).text_import_view() {
                    t.update(cx, |t, cx| t.demo("", "2026-10-27 10:00", Some("Q3 launch sync.txt"), TEAMS_SAMPLE, window, cx));
                }
            })
            .unwrap();
            shot("20b-text-import-filled", &mut cx);
            for id in ids.split(',').filter_map(|s| s.trim().parse::<i64>().ok()) {
                cx.update_window(handle, |_, window, cx| view.update(cx, |app, cx| app.open_meeting(id, window, cx))).unwrap();
                for (tab, name) in [(0, "summary"), (1, "transcript"), (2, "notes")] {
                    cx.update_window(handle, |_, _, cx| {
                        if let Some(m) = view.read(cx).meeting_view() {
                            m.update(cx, |m, cx| m.set_tab(tab, cx));
                        }
                    })
                    .unwrap();
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    shot(&format!("21-imported-{id}-{name}"), &mut cx);
                }
            }
            continue;
        }

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
                        (Source::System, 3.2, "Morning, everyone, good to see you all."),
                        (Source::Mic, 8.9, "Morning! Shall we start with next quarter's budget?"),
                        (Source::System, 15.4, "Sure. We're about ten percent under plan right now, mostly because of the trade show in May."),
                        (Source::Mic, 27.0, "Okay. Then I'd suggest we freeze marketing spend until the end of June."),
                        (Source::System, 36.8, "Agreed. Anna, could you send a new forecast by Friday?"),
                        (Source::System, 44.1, "Sure, will do. I just need the numbers from sales."),
                        (Source::Mic, 52.5, "I'll send them over this afternoon."),
                    ],
                    cx,
                )
            });
        })
        .unwrap();
        // Let the line fade-ins finish.
        std::thread::sleep(std::time::Duration::from_millis(600));
        shot("2-recording", &mut cx);
        // The decision log, filled from the demo meetings.
        if std::env::var_os("DECISIONS").is_some() {
            cx.update_window(handle, |_, window, cx| {
                view.update(cx, |app, cx| app.open_decisions(false, window, cx));
                if let Some(d) = view.read(cx).decisions_view() {
                    d.update(cx, |d, cx| d.demo_build_now(cx));
                }
            })
            .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(300));
            shot("2h-decisions", &mut cx);
        }
        // "Import recording…": the dialog for a file.
        if let Some(file) = std::env::var_os("IMPORTDLG") {
            cx.update_window(handle, |_, window, cx| view.update(cx, |app, cx| app.demo_import(Some(file.into()), window, cx))).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(300));
            shot("2g-import", &mut cx);
            cx.update_window(handle, |_, window, cx| {
                view.update(cx, |app, cx| app.demo_import(None, window, cx));
            })
            .ok();
        }
        // After Stop: the final pass under way, in the library and here.
        if std::env::var_os("FINALIZING").is_some() {
            cx.update_window(handle, |_, _, cx| {
                let rec = view.read(cx).recording_view();
                rec.update(cx, |r, cx| r.demo_finalizing(0.71, cx));
                view.update(cx, |_, cx| cx.notify());
            })
            .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(300));
            shot("2f-finalizing", &mut cx);
            mostalgia_app::processing::clear(mostalgia_app::processing::Key::Recording);
        }
        // Mori on the recording screen: writing, with live points, with an
        // answer in her bubble (no network). Needs `companion: true` in the config.
        if full && cx.update(|cx| view.read(cx).companion_view().read(cx).is_enabled()) {
            use mostalgia_app::companion::art::Pose;
            use mostalgia_core::companion::PointKind;
            let mori = |cx: &mut HeadlessAppContext, f: &dyn Fn(&mut mostalgia_app::companion::Companion, &mut Context<mostalgia_app::companion::Companion>)| {
                cx.update_window(handle, |_, _, cx| {
                    let c = view.read(cx).companion_view();
                    c.update(cx, |c, cx| f(c, cx));
                })
                .unwrap();
            };
            mori(&mut cx, &|c, cx| c.demo_pose(Some(Pose::Writing), cx));
            shot("12-mori-writing", &mut cx);
            mori(&mut cx, &|c, cx| {
                c.demo_pose(Some(Pose::Talking), cx);
                c.demo_points(
                    &[
                        (PointKind::Fact, "Budget is about 10% under plan, mostly because of the trade show in May.", 15.4),
                        (PointKind::Decision, "Marketing spend is frozen until the end of June.", 27.0),
                        (PointKind::Action, "Anna: send a new forecast by Friday (needs the sales numbers).", 36.8),
                    ],
                    &[1],
                    cx,
                );
            });
            std::thread::sleep(std::time::Duration::from_millis(500));
            shot("13-mori-points", &mut cx);
            // Many points: the full list below her (newest at the top, next
            // to her), then above her (newest at the bottom), then scrolled
            // away from the newest with the "N new" pill.
            mori(&mut cx, &|c, cx| {
                c.demo_points(
                    &[
                        (PointKind::Question, "Who reviews the migration?", 41.0),
                        (PointKind::Fact, "Churn went from 4.1% to 3.2% after the onboarding change in March, mostly in the small-business segment.", 44.0),
                        (PointKind::Action, "Ben: book the room for the design review next Tuesday afternoon.", 47.5),
                        (PointKind::Decision, "The beta stays invite-only until the load tests pass.", 50.0),
                        (PointKind::Fact, "Budget: 48,000 EUR left for Q3.", 52.0),
                        (PointKind::Question, "Do we need legal to sign off on the new data retention policy before the launch, or is the existing agreement enough?", 54.0),
                        (PointKind::Action, "Chris updates the onboarding emails.", 55.0),
                        (PointKind::Fact, "Support tickets are down 18% week over week.", 56.5),
                        (PointKind::Decision, "We drop the Windows 7 installer.", 57.5),
                    ],
                    &[],
                    cx,
                );
                c.demo_expand(true, cx);
            });
            std::thread::sleep(std::time::Duration::from_millis(500));
            shot("13b-mori-points-below", &mut cx);
            mori(&mut cx, &|c, cx| c.demo_pos(Some((1., 1.)), cx));
            std::thread::sleep(std::time::Duration::from_millis(300));
            shot("13c-mori-points-above", &mut cx);
            mori(&mut cx, &|c, cx| c.demo_scroll_away(300., cx));
            std::thread::sleep(std::time::Duration::from_millis(100));
            mori(&mut cx, &|c, cx| c.demo_points(&[(PointKind::Action, "Dana: write up the incident report by Wednesday.", 58.0)], &[], cx));
            std::thread::sleep(std::time::Duration::from_millis(500));
            shot("13d-mori-points-scrolled", &mut cx);
            mori(&mut cx, &|c, cx| {
                c.demo_expand(false, cx);
                c.demo_pos(Some((0., 1.)), cx);
            });
            std::thread::sleep(std::time::Duration::from_millis(300));
            shot("13e-mori-points-left-above", &mut cx);
            mori(&mut cx, &|c, cx| c.demo_pos(None, cx));
            // As it happens: the newest card in full, older ones settled
            // into quiet one-liners.
            mori(&mut cx, &|c, cx| {
                c.demo_reset(cx);
                c.demo_pose(Some(Pose::Talking), cx);
                c.demo_points(
                    &[
                        (PointKind::Fact, "Q3 budget is about 10% under plan, mostly because of the May trade show.", 15.4),
                        (PointKind::Decision, "Marketing spend is frozen until the end of June.", 27.0),
                        (PointKind::Question, "Does the frozen budget also cover the agency retainer?", 33.0),
                        (PointKind::Action, "Anna sends the revised Q3 forecast to finance by Friday (needs the sales numbers).", 36.8),
                    ],
                    &[1],
                    cx,
                );
                c.demo_age_points(1, 20., cx);
            });
            std::thread::sleep(std::time::Duration::from_millis(900));
            shot("13f-mori-points-calm", &mut cx);
            // Each companion writing along, with a note card and (Tsumugi,
            // Hoshi) her own "a note arrived" pop.
            for c in mostalgia_app::companion::character::Character::ALL {
                mori(&mut cx, &|m, cx| {
                    m.demo_reset(cx);
                    m.demo_character(c, cx);
                    m.demo_pose(Some(Pose::Writing), cx);
                    m.demo_points(
                        &[
                            (PointKind::Decision, "Marketing spend is frozen until the end of June.", 27.0),
                            (PointKind::Action, "Anna sends the revised Q3 forecast to finance by Friday.", 36.8),
                        ],
                        &[],
                        cx,
                    );
                });
                std::thread::sleep(std::time::Duration::from_millis(500));
                mori(&mut cx, &|m, cx| m.demo_pop(Some(PointKind::Action), 7, cx));
                shot(&format!("19-companion-{}", c.id()), &mut cx);
            }
            let configured = mostalgia_core::config::Config::load().companion_character();
            mori(&mut cx, &|m, cx| {
                m.demo_reset(cx);
                m.demo_character(configured, cx);
            });
            mori(&mut cx, &|c, cx| {
                c.demo_pose(Some(Pose::Talking), cx);
                c.demo_chat(
                    true,
                    "What did we decide so far?",
                    "Two decisions so far:\n- Marketing spend is frozen until the end of June [[t:00:00:27]].\n- Anna sends a new forecast by Friday [[t:00:00:36]]; you send her the sales numbers this afternoon [00:00:44–00:00:52].\n\nThe budget gap comes mostly from the trade show in May [00:00:15, 00:00:27].",
                    cx,
                );
            });
            std::thread::sleep(std::time::Duration::from_millis(300));
            shot("14-mori-chat", &mut cx);
            mori(&mut cx, &|c, cx| {
                c.demo_chat(
                    false,
                    "Where did we talk about the trade show?",
                    "In the budget review: the budget is about ten percent under plan, mostly because of the trade show [[m:1@6]]. Marketing spend was frozen as a result [[m:1@14]], and Anna sends a new forecast [[m:1@19]].",
                    cx,
                );
            });
            std::thread::sleep(std::time::Duration::from_millis(300));
            shot("14b-mori-chat-library", &mut cx);
            // The Notes tab while recording (record controls with the time
            // in the header), her menu, then hidden: the collapsed bar with
            // cards popping out of it, its menu.
            mori(&mut cx, &|c, cx| c.demo_notes_tab(true, cx));
            std::thread::sleep(std::time::Duration::from_millis(300));
            shot("16-mori-notes-live", &mut cx);
            mori(&mut cx, &|c, cx| {
                c.demo_close(cx);
                c.demo_expand(false, cx);
                c.demo_menu(Some((70., 90.)), cx);
            });
            std::thread::sleep(std::time::Duration::from_millis(300));
            shot("17-mori-menu", &mut cx);
            mori(&mut cx, &|c, cx| {
                c.demo_menu(None, cx);
                c.demo_hidden(true, cx);
                c.demo_points(
                    &[
                        (PointKind::Decision, "Release moves to Friday.", 60.0),
                        (PointKind::Action, "Ben: book the room for the design review.", 63.0),
                    ],
                    &[],
                    cx,
                );
            });
            std::thread::sleep(std::time::Duration::from_millis(500));
            shot("18-mori-bar-cards", &mut cx);
            mori(&mut cx, &|c, cx| {
                c.demo_pos(Some((1., 1.)), cx);
                c.demo_points(&[(PointKind::Question, "Who signs off on the budget?", 66.0)], &[], cx);
            });
            std::thread::sleep(std::time::Duration::from_millis(500));
            shot("18b-mori-bar-cards-above", &mut cx);
            mori(&mut cx, &|c, cx| c.demo_menu(Some((54., -100.)), cx));
            std::thread::sleep(std::time::Duration::from_millis(300));
            shot("18c-mori-bar-menu", &mut cx);
            mori(&mut cx, &|c, cx| {
                c.demo_menu(None, cx);
                c.demo_notes_tab(true, cx);
            });
            std::thread::sleep(std::time::Duration::from_millis(300));
            shot("18d-mori-bar-notes", &mut cx);
            mori(&mut cx, &|c, cx| c.demo_reset(cx));
        }
        if full {
            cx.update_window(handle, |_, _, cx| {
                let rec = view.read(cx).recording_view();
                rec.update(cx, |r, cx| r.demo_engine_error(
                    "OpenAI · gpt-4o-transcribe",
                    &mostalgia_core::i18n::trf("{}: nicht erreichbar unter {} — läuft der Server?", &[&"OpenAI", &"https://api.openai.com/v1"]),
                    3,
                    cx,
                ));
            })
            .unwrap();
            shot("2b-recording-engine-error", &mut cx);
        }
        // MICTOGGLE=1: the mic switched off and on mid-recording (markers in
        // the transcript, crossed-out mic), on the screen and in Mori's bar.
        if std::env::var_os("MICTOGGLE").is_some() {
            cx.update_window(handle, |_, _, cx| {
                let rec = view.read(cx).recording_view();
                rec.update(cx, |r, cx| {
                    r.demo(
                        187,
                        &[
                            (Source::System, 3.2, "Morning, everyone, good to see you all."),
                            (Source::Mic, 8.9, "Morning! Shall we start with next quarter's budget?"),
                            (Source::System, 15.4, "Sure. We're about ten percent under plan right now, mostly because of the trade show in May."),
                            (Source::Mic, 27.0, "Okay. Then I'd suggest we freeze marketing spend until the end of June."),
                            (Source::System, 36.8, "Agreed. Anna, could you send a new forecast by Friday?"),
                            (Source::System, 44.1, "Sure, will do. I just need the numbers from sales."),
                        ],
                        cx,
                    );
                    r.demo_switches(&[(Source::Mic, false, 12.4), (Source::Mic, true, 26.1), (Source::Mic, false, 50.2)], cx);
                });
            })
            .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(600));
            shot("2c-recording-mic-off", &mut cx);
            if cx.update(|cx| view.read(cx).companion_view().read(cx).is_enabled()) {
                cx.update_window(handle, |_, _, cx| {
                    let c = view.read(cx).companion_view();
                    c.update(cx, |c, cx| c.demo_hidden(true, cx));
                })
                .unwrap();
                std::thread::sleep(std::time::Duration::from_millis(300));
                shot("2d-mori-bar-mic-off", &mut cx);
                // The bar's indicator: sparkles mid-twinkle, resting while
                // paused, the spinner and the old red dot.
                std::thread::sleep(std::time::Duration::from_millis(900));
                shot("2d2-mori-bar-sparkles-later", &mut cx);
                cx.update_window(handle, |_, _, cx| {
                    let rec = view.read(cx).recording_view();
                    rec.update(cx, |r, cx| r.set_paused(true, cx));
                })
                .unwrap();
                shot("2d3-mori-bar-paused", &mut cx);
                cx.update_window(handle, |_, _, cx| {
                    let rec = view.read(cx).recording_view();
                    rec.update(cx, |r, cx| r.set_paused(false, cx));
                })
                .unwrap();
                for (name, indicator) in [("2d4-mori-bar-spinner", mostalgia_app::companion::BarIndicator::Spinner), ("2d5-mori-bar-sparkles", mostalgia_app::companion::BarIndicator::Sparkles)] {
                    cx.update_window(handle, |_, _, cx| {
                        let c = view.read(cx).companion_view();
                        c.update(cx, |c, cx| c.demo_bar_indicator(indicator, cx));
                    })
                    .unwrap();
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    shot(name, &mut cx);
                }
                cx.update_window(handle, |_, _, cx| {
                    let c = view.read(cx).companion_view();
                    c.update(cx, |c, cx| {
                        c.demo_hidden(false, cx);
                        c.demo_chat(true, "Did I miss anything while my mic was off?", "Anna will send a new forecast by Friday [[t:36]].", cx);
                    });
                })
                .unwrap();
                std::thread::sleep(std::time::Duration::from_millis(300));
                shot("2e-mori-bubble-mic-off", &mut cx);
                cx.update_window(handle, |_, _, cx| {
                    let c = view.read(cx).companion_view();
                    c.update(cx, |c, cx| c.demo_reset(cx));
                })
                .unwrap();
            }
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
            for (tab, name) in [(0, "3-summary"), (1, "4-transcript"), (2, "5b-notes"), (3, "5-audio")] {
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
                std::thread::sleep(std::time::Duration::from_millis(if tab == 3 { 800 } else { 400 }));
                shot(name, &mut cx);
            }
            // "Copy recap": the toast after copying.
            if std::env::var_os("RECAP").is_some() {
                cx.update_window(handle, |_, window, cx| {
                    if let Some(m) = view.read(cx).meeting_view() {
                        m.update(cx, |m, cx| {
                            m.set_tab(0, cx);
                            m.demo_copy_recap(window, cx);
                        });
                    }
                })
                .unwrap();
                std::thread::sleep(std::time::Duration::from_millis(400));
                shot("3b-recap-copied", &mut cx);
                if let Some(text) = cx.update(|cx| cx.read_from_clipboard()).and_then(|c| c.text()) {
                    std::fs::write(out.join("recap.txt"), text).unwrap();
                }
            }
            // "Regenerate notes": the panel, its progress, the result.
            for (stage, name) in [(0, "5c-notes-regenerate"), (1, "5d-notes-regenerating"), (2, "5e-notes-regenerated")] {
                cx.update_window(handle, |_, window, cx| {
                    if let Some(m) = view.read(cx).meeting_view() {
                        m.update(cx, |m, cx| m.demo_regenerate(stage, window, cx));
                    }
                })
                .unwrap();
                shot(name, &mut cx);
            }
            // Mori without a recording: the record button's source choice, a
            // failed start, the open meeting's saved notes, the bar's choice.
            if cx.update(|cx| view.read(cx).companion_view().read(cx).is_enabled()) {
                let mori = |cx: &mut HeadlessAppContext, f: &dyn Fn(&mut mostalgia_app::companion::Companion, &mut Context<mostalgia_app::companion::Companion>)| {
                    cx.update_window(handle, |_, _, cx| {
                        let c = view.read(cx).companion_view();
                        c.update(cx, |c, cx| f(c, cx));
                    })
                    .unwrap();
                };
                cx.update_window(handle, |_, _, cx| {
                    let rec = view.read(cx).recording_view();
                    rec.update(cx, |r, cx| r.demo_end(cx));
                })
                .unwrap();
                mori(&mut cx, &|c, cx| {
                    c.demo_reset(cx);
                    c.demo_pos(None, cx);
                    c.demo_record_choice(false, cx);
                });
                std::thread::sleep(std::time::Duration::from_millis(300));
                shot("15-mori-record-choice", &mut cx);
                mori(&mut cx, &|c, cx| c.demo_start_failed("No speech model downloaded yet – choose one in Settings → Recording.", cx));
                std::thread::sleep(std::time::Duration::from_millis(300));
                shot("15b-mori-start-failed", &mut cx);
                mori(&mut cx, &|c, cx| c.demo_notes_tab(true, cx));
                std::thread::sleep(std::time::Duration::from_millis(300));
                shot("16b-mori-notes-saved", &mut cx);
                mori(&mut cx, &|c, cx| {
                    c.demo_close(cx);
                    c.demo_hidden(true, cx);
                    c.demo_record_choice(true, cx);
                });
                std::thread::sleep(std::time::Duration::from_millis(300));
                shot("18e-mori-bar-choice", &mut cx);
                mori(&mut cx, &|c, cx| c.demo_reset(cx));
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
                        "What did we decide about marketing spend?",
                        "Marketing spend is frozen until the end of June [[m:1@14]]. The reason: the budget is about ten percent under plan, mostly because of the trade show [[m:1@6]].\n- Speaker 1 proposed it and Speaker 2 agreed [[m:1@19]].\n- Anna sends a new forecast to everyone by Friday [[m:1@19]].\n- The launch sync planned around it: the public beta moves to May 12 [[m:2@95]].",
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

/// A Teams-style transcript for the "meeting from text" dialog.
const TEAMS_SAMPLE: &str = "Anna Schmidt   0:03
Morning, everyone. Let's start with the Q3 launch plan, then pricing.

Ben Müller   0:21
Sure. Engineering is on track for the beta on September 12th, but the payment integration still needs about two weeks.

Anna Schmidt   0:48
Two weeks pushes us past the 12th. Can we ship the beta without payments and switch them on later?

Clara Jensen   1:10
From the marketing side that works, as long as we announce paid plans in the launch post.

Ben Müller   1:32
Then I'll put payments behind a feature flag.
";
