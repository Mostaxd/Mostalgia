//! Alternative companions to Mori. Each module has the same artwork API as
//! `companion::art` (`Pose`, `FRAME_W`, `FRAME_H`, `FPS`, `frame_count`,
//! `frame_svg`), so any of them could later stand in for her, plus its own
//! way of drawing the meeting's notes (`notes_svg`).
//!
//! Not wired into the app yet: preview them with
//! `cargo run -p mostalgia-app --example cast_preview -- <out-dir>`.

/// The kinds of note a companion shows (decision, action item, question, fact).
pub use mostalgia_core::companion::PointKind;

pub mod hoshi;
pub mod tsumugi;
