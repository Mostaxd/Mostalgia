//! The artwork of whichever companion is chosen: Mori (`art`), Tsumugi or
//! Hoshi (`cast`). All three share the pose set and frame size, so the
//! companion only asks this module for frames.

use mostalgia_core::companion::PointKind;
pub use mostalgia_core::companion::Character;

use super::art::{self, Pose};
use super::cast::{hoshi, tsumugi};

fn tsumugi_pose(pose: Pose) -> tsumugi::Pose {
    match pose {
        Pose::Idle => tsumugi::Pose::Idle,
        Pose::Listening => tsumugi::Pose::Listening,
        Pose::Writing => tsumugi::Pose::Writing,
        Pose::Thinking => tsumugi::Pose::Thinking,
        Pose::Talking => tsumugi::Pose::Talking,
        Pose::Sleeping => tsumugi::Pose::Sleeping,
        Pose::Error => tsumugi::Pose::Error,
    }
}

fn hoshi_pose(pose: Pose) -> hoshi::Pose {
    match pose {
        Pose::Idle => hoshi::Pose::Idle,
        Pose::Listening => hoshi::Pose::Listening,
        Pose::Writing => hoshi::Pose::Writing,
        Pose::Thinking => hoshi::Pose::Thinking,
        Pose::Talking => hoshi::Pose::Talking,
        Pose::Sleeping => hoshi::Pose::Sleeping,
        Pose::Error => hoshi::Pose::Error,
    }
}

/// Frames in one loop of `pose`.
pub fn frame_count(c: Character, pose: Pose) -> usize {
    match c {
        Character::Mori => art::frame_count(pose),
        Character::Tsumugi => tsumugi::frame_count(tsumugi_pose(pose)),
        Character::Hoshi => hoshi::frame_count(hoshi_pose(pose)),
    }
}

/// One frame as a complete SVG document (`FRAME_W` × `FRAME_H`).
pub fn frame_svg(c: Character, pose: Pose, frame: usize) -> String {
    match c {
        Character::Mori => art::frame_svg(pose, frame),
        Character::Tsumugi => tsumugi::frame_svg(tsumugi_pose(pose), frame),
        Character::Hoshi => hoshi::frame_svg(hoshi_pose(pose), frame),
    }
}

/// Animation speed her frames are drawn for.
pub fn fps(c: Character) -> u32 {
    match c {
        Character::Mori => art::FPS,
        Character::Tsumugi => tsumugi::FPS,
        Character::Hoshi => hoshi::FPS,
    }
}

/// Frame size (the SVG viewBox); the same for all of them.
pub fn frame_size(c: Character) -> (f32, f32) {
    match c {
        Character::Mori => (art::FRAME_W, art::FRAME_H),
        Character::Tsumugi => (tsumugi::FRAME_W, tsumugi::FRAME_H),
        Character::Hoshi => (hoshi::FRAME_W, hoshi::FRAME_H),
    }
}

/// Frames of her "a note arrived" pop (played once), if she has one
/// (Mori's notes go onto her notepad).
pub fn pop_frames(c: Character) -> Option<usize> {
    match c {
        Character::Mori => None,
        Character::Tsumugi => Some(tsumugi::POP_FRAMES),
        Character::Hoshi => Some(hoshi::POP_FRAMES),
    }
}

/// One frame of the pop for a note of `kind`: a square SVG document
/// (Tsumugi's woven swatch, Hoshi's new star).
pub fn pop_svg(c: Character, kind: PointKind, frame: usize) -> Option<(String, f32)> {
    match c {
        Character::Mori => None,
        Character::Tsumugi => Some((tsumugi::note_pop_svg(kind, frame), tsumugi::POP_W)),
        Character::Hoshi => Some((hoshi::note_pop_svg(kind, frame), hoshi::POP_SIZE)),
    }
}

/// An SVG document wrapped in an outer one of `px_h` pixels height, so it
/// is rasterized sharp at the size it's drawn instead of scaled.
pub fn scaled(svg: String, (vw, vh): (f32, f32), px_h: u32) -> String {
    let inner = match svg.trim_start().strip_prefix("<?xml") {
        Some(rest) => rest.split_once("?>").map_or("", |(_, r)| r).to_string(),
        None => svg,
    };
    let h = px_h.max(1);
    let w = ((h as f32) * vw / vh).round().max(1.) as u32;
    format!(r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="{w}" height="{h}" viewBox="0 0 {vw} {vh}">{inner}</svg>"#)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_character_draws_every_pose() {
        for c in Character::ALL {
            assert_eq!(frame_size(c), (art::FRAME_W, art::FRAME_H));
            for pose in Pose::ALL {
                let n = frame_count(c, pose);
                assert!(n > 0);
                let svg = frame_svg(c, pose, n - 1);
                assert!(svg.contains("<svg") && svg.trim_end().ends_with("</svg>"), "{c:?} {pose:?}");
            }
            if let Some(frames) = pop_frames(c) {
                let (svg, size) = pop_svg(c, PointKind::Decision, frames - 1).unwrap();
                assert!(svg.contains("<svg") && size > 0.);
            }
        }
        assert!(pop_svg(Character::Mori, PointKind::Fact, 0).is_none());
        let s = scaled(frame_svg(Character::Hoshi, Pose::Idle, 0), (200., 320.), 64);
        assert!(s.starts_with(r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="40" height="64""#));
    }
}
