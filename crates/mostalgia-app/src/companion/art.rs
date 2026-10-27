//! Mori's artwork: every animation frame is generated as an SVG document
//! (transparent background, viewBox FRAME_W × FRAME_H), rendered by GPUI.
//!
//! Mori is drawn as a small rig: the body parts are fixed shapes, and each
//! pose only decides where the joints go for a phase `t` in `0..1` (head
//! tilt, eye state, where the pen tip and the notepad are; the arms follow by
//! two-bone IK). All motion is built from `sin`/`cos` of `2πt`, so every
//! pose loops seamlessly. No `<text>` and no filters: frames must render the
//! same in resvg and GPUI without fonts.

use std::f32::consts::{PI, TAU};
use std::fmt::{self, Write as _};

/// What Mori is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pose {
    /// Breathing, blinking, a small sway.
    Idle,
    /// Someone started speaking: looks up, head tilted.
    Listening,
    /// Speech going on: pen moving on the notepad, pages flipping.
    Writing,
    /// Waiting for the language model.
    Thinking,
    /// Showing an answer or a point.
    Talking,
    /// Recording paused.
    Sleeping,
    /// The provider failed (the error itself is shown next to her).
    Error,
}

impl Pose {
    pub const ALL: [Pose; 7] = [Pose::Idle, Pose::Listening, Pose::Writing, Pose::Thinking, Pose::Talking, Pose::Sleeping, Pose::Error];
}

/// Logical size of a frame (the SVG viewBox); scaled when drawn.
pub const FRAME_W: f32 = 200.0;
pub const FRAME_H: f32 = 320.0;
/// Animation speed every pose is drawn for.
pub const FPS: u32 = 12;

/// Frames in one loop of `pose`.
pub fn frame_count(pose: Pose) -> usize {
    match pose {
        Pose::Idle | Pose::Sleeping => 24,
        Pose::Listening | Pose::Writing | Pose::Thinking | Pose::Talking => 16,
        Pose::Error => 12,
    }
}

/// One frame as a complete SVG document.
pub fn frame_svg(pose: Pose, frame: usize) -> String {
    let count = frame_count(pose);
    let frame = frame % count;
    let rig = rig(pose, frame, frame as f32 / count as f32);
    draw(&rig)
}

// ---------------------------------------------------------------- palette

const OL: &str = "#2B1730"; // Night Plum: outlines
const SKIN: &str = "#FFE7D6";
const SKIN_SH: &str = "#F4C5AE";
const HAIR: &str = "#5B2A4E"; // Plum
const HAIR_HI: &str = "#9A5C88";
const HAIR_SH: &str = "#4C2244";
const CREAM: &str = "#FBF3E4";
const CREAM_DEEP: &str = "#F0DFC2";
const SAGE: &str = "#9DB8A0";
const SAGE_SH: &str = "#7F9E84";
const TERRA: &str = "#D9653B";
const EMBER: &str = "#B8472A";
const AMBER: &str = "#F2A541";
const AMBER_L: &str = "#F7C26B";
const MOUTH: &str = "#8A3444";
const TONGUE: &str = "#E9877A";
const BLUSH: &str = "#F2876F";
const BROW: &str = "#4A2242";
const SWEAT: &str = "#BFE0E4";

// ---------------------------------------------------------------- numbers

/// A coordinate printed with one decimal at most ("12", "12.5", "-3.2").
#[derive(Clone, Copy)]
struct N(f32);

impl fmt::Display for N {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let r = (self.0 * 10.0).round() / 10.0;
        let r = if r == 0.0 { 0.0 } else { r };
        if r.fract() == 0.0 { write!(f, "{r:.0}") } else { write!(f, "{r:.1}") }
    }
}

fn n(v: f32) -> N {
    N(v)
}

type P = (f32, f32);

fn lerp(a: P, b: P, k: f32) -> P {
    (a.0 + (b.0 - a.0) * k, a.1 + (b.1 - a.1) * k)
}

fn dir(deg: f32) -> P {
    let r = deg.to_radians();
    (r.cos(), r.sin())
}

fn wave(t: f32, cycles: f32, phase: f32) -> f32 {
    (TAU * (t * cycles + phase)).sin()
}

/// Two-bone IK: elbow and (reachable) hand for a shoulder `s` reaching to
/// `target`. `bend` (+1 / −1) picks the side the elbow points to.
fn ik(s: P, target: P, l1: f32, l2: f32, bend: f32) -> (P, P) {
    let (dx, dy) = (target.0 - s.0, target.1 - s.1);
    let dist = (dx * dx + dy * dy).sqrt().max(0.001);
    let d = dist.clamp((l1 - l2).abs() + 0.5, l1 + l2 - 0.3);
    let base = dy.atan2(dx);
    let a = ((l1 * l1 + d * d - l2 * l2) / (2.0 * l1 * d)).clamp(-1.0, 1.0).acos();
    let ang = base + bend * a;
    let elbow = (s.0 + l1 * ang.cos(), s.1 + l1 * ang.sin());
    let hand = (s.0 + dx / dist * d, s.1 + dy / dist * d);
    (elbow, hand)
}

// ---------------------------------------------------------------- the rig

#[derive(Clone, Copy, PartialEq)]
enum Eyes {
    Open,
    Wide,
    Closed,
    Happy,
}

#[derive(Clone, Copy)]
enum Mouth {
    Smile,
    Open(f32),
    O(f32),
    Hmm,
    Worried,
}

#[derive(Clone, Copy)]
enum Brow {
    Soft,
    Raised,
    Worried,
    Sleepy,
}

#[derive(Clone, Copy)]
enum Extra {
    None,
    Listen(f32),
    Bubble(f32),
    Zz(f32),
    Sweat(f32),
}

struct Rig {
    /// Whole figure: horizontal shake and sway (degrees around the feet).
    shake: f32,
    sway: f32,
    /// Upper body offset (breathing).
    bob: f32,
    head_rot: f32,
    head_dx: f32,
    head_dy: f32,
    eyes: Eyes,
    look: P,
    /// Upper lid, 0 = open, 1 = shut.
    lid: f32,
    mouth: Mouth,
    brow: Brow,
    blush: f32,
    /// Ahoge (the single upright lock) angle and hair-tip swing.
    ahoge: f32,
    swing: f32,
    /// Notepad top-left corner and rotation; where its hand holds it.
    pad: (f32, f32, f32),
    pad_grip: P,
    pad_bend: f32,
    pad_front: bool,
    /// Scribbled rows on the page (0..4) and an optional page flip (0..1).
    ink: f32,
    flip: Option<f32>,
    /// Pen tip and the pen's direction from tip to cap (degrees).
    pen_tip: P,
    pen_ang: f32,
    pen_bend: f32,
    extra: Extra,
}

const PAD_W: f32 = 30.0;
const PAD_H: f32 = 38.0;
const SHOULDER_L: P = (79.0, 130.0);
const SHOULDER_R: P = (121.0, 130.0);
const UPPER_ARM: f32 = 26.0;
const FOREARM: f32 = 25.0;
/// The head is drawn at a larger native scale, then scaled around the chin.
const HEAD_SCALE: f32 = 0.92;
const NECK: P = (100.0, 112.0);

fn base() -> Rig {
    Rig {
        shake: 0.0,
        sway: 0.0,
        bob: 0.0,
        head_rot: 0.0,
        head_dx: 0.0,
        head_dy: 0.0,
        eyes: Eyes::Open,
        look: (0.0, 0.0),
        lid: 0.0,
        mouth: Mouth::Smile,
        brow: Brow::Soft,
        blush: 0.45,
        ahoge: 0.0,
        swing: 0.0,
        pad: (102.0, 141.0, -7.0),
        pad_grip: (PAD_W - 1.5, 33.0),
        pad_bend: -1.0,
        pad_front: false,
        ink: 2.6,
        flip: None,
        pen_tip: (73.0, 186.0),
        pen_ang: -96.0,
        pen_bend: 1.0,
        extra: Extra::None,
    }
}

fn pad_point(r: &Rig, local: P) -> P {
    let (px, py, rot) = r.pad;
    let (c, s) = (rot.to_radians().cos(), rot.to_radians().sin());
    (px + local.0 * c - local.1 * s, py + local.0 * s + local.1 * c)
}

fn rig(pose: Pose, frame: usize, t: f32) -> Rig {
    let mut r = base();
    match pose {
        Pose::Idle => {
            r.sway = 0.8 * wave(t, 1.0, 0.0);
            r.bob = -0.9 * (0.5 - 0.5 * (TAU * 2.0 * t).cos());
            r.head_rot = 1.8 * wave(t, 1.0, 0.1);
            r.swing = -r.head_rot * 0.7;
            r.ahoge = 7.0 * wave(t, 1.0, 0.25);
            r.lid = match frame {
                14 => 0.55,
                15 => 1.0,
                16 => 0.35,
                _ => 0.0,
            };
            r.pen_tip.0 += 0.6 * wave(t, 1.0, 0.0);
        }
        Pose::Listening => {
            let nod = wave(t, 2.0, 0.0);
            r.head_rot = 7.0 + 1.2 * wave(t, 1.0, 0.0);
            r.head_dy = -1.0 + 0.9 * nod;
            r.bob = -0.5 * (0.5 - 0.5 * (TAU * t).cos());
            r.eyes = Eyes::Wide;
            r.look = (1.4, -1.4);
            r.brow = Brow::Raised;
            r.mouth = Mouth::O(1.6 + 0.2 * wave(t, 2.0, 0.0));
            r.ahoge = -6.0 + 9.0 * wave(t, 2.0, 0.1);
            r.swing = -1.0 * wave(t, 2.0, 0.0);
            // Pen poised over the page, ready.
            let tip = pad_point(&r, (7.0, 15.0 - 3.0 - 1.2 * wave(t, 2.0, 0.3)));
            r.pen_tip = tip;
            r.pen_ang = -118.0;
            r.extra = Extra::Listen(t);
        }
        Pose::Writing => {
            r.pad = (102.0, 149.0, 9.0);
            r.pad_grip = (PAD_W - 1.0, 34.0);
            r.head_rot = 4.0 + 1.0 * wave(t, 1.0, 0.0);
            r.head_dy = 2.5;
            r.head_dx = 1.0;
            r.look = (1.2, 2.0);
            r.lid = 0.38;
            r.brow = Brow::Soft;
            r.mouth = Mouth::Smile;
            r.ahoge = 10.0 + 3.0 * wave(t, 2.0, 0.0);
            r.bob = -0.5 * (0.5 - 0.5 * (TAU * t).cos());
            // Rows 0..3 are written in frames 0..12, then the page flips.
            let fw = 12.0;
            let f = frame as f32;
            let rows = [13.0, 19.0, 25.0];
            if f < fw {
                r.ink = 3.0 * f / fw;
                let row = (r.ink as usize).min(2);
                let k = r.ink - row as f32;
                let loop_x = 1.2 * (TAU * 2.0 * f / 4.0).sin();
                let loop_y = 1.4 * (TAU * 2.0 * f / 4.0).cos();
                r.pen_tip = pad_point(&r, (5.0 + k * 20.0 + loop_x, rows[row] - 1.0 + loop_y));
            } else {
                r.ink = 3.0;
                let k = (f - fw) / (16.0 - fw);
                r.flip = Some(k);
                // The pen lifts and swings back to the start of the page.
                let a = pad_point(&r, (25.0, 26.0));
                let b = pad_point(&r, (5.0, 12.0));
                let mut p = lerp(a, b, 0.5 - 0.5 * (PI * k).cos());
                p.1 -= 6.0 * (PI * k).sin();
                r.pen_tip = p;
            }
            r.pen_ang = -122.0;
        }
        Pose::Thinking => {
            r.head_rot = -6.0 + 1.2 * wave(t, 1.0, 0.0);
            r.head_dx = -0.5;
            r.look = (-1.6, -2.2);
            r.lid = 0.12;
            r.brow = Brow::Raised;
            r.mouth = Mouth::Hmm;
            r.ahoge = -4.0 + 5.0 * wave(t, 1.0, 0.0);
            r.swing = -r.head_rot * 0.15;
            r.bob = -0.6 * (0.5 - 0.5 * (TAU * t).cos());
            r.pad = (103.0, 150.0, 4.0);
            let tap = (wave(t, 2.0, 0.0)).max(0.0);
            r.pen_tip = (104.5, 112.5 - 1.6 * tap);
            r.pen_ang = 68.0;
            r.pen_bend = 1.0;
            r.extra = Extra::Bubble(t);
        }
        Pose::Talking => {
            let syl = (TAU * 2.0 * t).sin().abs();
            r.head_rot = 2.0 * wave(t, 1.0, 0.0);
            r.head_dy = -0.6 * syl;
            r.bob = -0.6 * (0.5 - 0.5 * (TAU * t).cos());
            r.mouth = if syl > 0.2 { Mouth::Open(3.4 * syl) } else { Mouth::Smile };
            r.eyes = if (6..=7).contains(&frame) { Eyes::Happy } else { Eyes::Open };
            r.brow = Brow::Raised;
            r.blush = 0.55;
            r.ahoge = 5.0 * wave(t, 2.0, 0.2);
            r.swing = -r.head_rot * 0.6;
            r.pad = (103.0, 133.0, -4.0);
            let g = wave(t, 1.0, 0.0);
            r.pen_tip = (60.0 + 2.5 * g, 124.0 - 4.0 * syl);
            r.pen_ang = 75.0 + 8.0 * g;
            r.pen_bend = -1.0;
        }
        Pose::Sleeping => {
            let s = wave(t, 1.0, 0.0);
            r.head_rot = 9.0 + 2.5 * s;
            r.head_dy = 3.0 + 1.5 * s;
            r.head_dx = 1.0;
            r.bob = -1.2 * (0.5 - 0.5 * (TAU * t).cos());
            r.eyes = Eyes::Closed;
            r.brow = Brow::Sleepy;
            r.mouth = Mouth::O(1.1 + 0.3 * s);
            r.blush = 0.55;
            r.ahoge = 28.0 + 4.0 * s;
            r.swing = -s * 0.8;
            r.pad = (93.0, 139.0, -12.0);
            r.pad_grip = (PAD_W - 2.0, 34.0);
            r.ink = 1.4;
            r.pen_tip = pad_point(&r, (3.0, 36.0));
            r.pen_tip.1 += 8.0;
            r.pen_ang = -80.0;
            r.pen_bend = 1.0;
            r.extra = Extra::Zz(t);
        }
        Pose::Error => {
            r.shake = 1.4 * wave(t, 3.0, 0.0);
            r.head_rot = -2.0 + 1.5 * wave(t, 3.0, 0.15);
            r.eyes = Eyes::Wide;
            r.look = (0.0, 0.6);
            r.brow = Brow::Worried;
            r.mouth = Mouth::Worried;
            r.blush = 0.25;
            r.ahoge = 14.0 * wave(t, 3.0, 0.3);
            r.swing = 1.2 * wave(t, 3.0, 0.1);
            r.pad = (120.0, 96.0, 9.0);
            r.pad_grip = (16.0, PAD_H - 1.0);
            r.pad_bend = -1.0;
            r.pad_front = true;
            r.pen_tip = (88.0, 118.0);
            r.pen_ang = 110.0;
            r.pen_bend = 1.0;
            r.extra = Extra::Sweat(t);
        }
    }
    r
}

// ---------------------------------------------------------------- drawing

fn draw(r: &Rig) -> String {
    let mut s = String::with_capacity(16 * 1024);
    let _ = write!(
        s,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}"><defs><linearGradient id="ir" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#3F1C3B"/><stop offset=".55" stop-color="#7A3550"/><stop offset="1" stop-color="#D9653B"/></linearGradient><linearGradient id="hg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#7A3A68"/><stop offset="1" stop-color="{HAIR}"/></linearGradient></defs>"##,
        w = FRAME_W,
        h = FRAME_H
    );
    // Ground shadow (not swayed).
    s.push_str(r##"<ellipse cx="100" cy="300" rx="30" ry="4" fill="#2B1730" opacity=".13"/>"##);
    let _ = write!(s, r#"<g transform="translate({} 0) rotate({} 100 298)" stroke-linejoin="round" stroke-linecap="round">"#, n(r.shake), n(r.sway));
    legs(&mut s);
    let _ = write!(s, r#"<g transform="translate(0 {})">"#, n(r.bob));
    let head_tf = format!(
        "translate({} {}) rotate({} {} {}) translate({} {}) scale({HEAD_SCALE}) translate(-100 -113)",
        n(r.head_dx),
        n(r.head_dy),
        n(r.head_rot),
        NECK.0,
        NECK.1,
        NECK.0,
        NECK.1
    );
    let _ = write!(s, r#"<g transform="{head_tf}">"#);
    back_hair(&mut s, r);
    s.push_str("</g>");
    torso(&mut s);
    // Notepad arm (her left, viewer's right).
    let grip = pad_point(r, r.pad_grip);
    let (pe, ph) = ik(SHOULDER_R, grip, UPPER_ARM, FOREARM, r.pad_bend);
    if !r.pad_front {
        arm(&mut s, SHOULDER_R, pe, ph);
        pad(&mut s, r);
        hand(&mut s, ph, false);
    }
    // Pen arm (her right, viewer's left).
    let pd = dir(r.pen_ang);
    let grip_pen = (r.pen_tip.0 + pd.0 * 8.0, r.pen_tip.1 + pd.1 * 8.0);
    let (ee, eh) = ik(SHOULDER_L, grip_pen, UPPER_ARM, FOREARM, r.pen_bend);
    // Keep the pen in the hand even when the target was out of reach.
    let tip = (eh.0 - pd.0 * 8.0, eh.1 - pd.1 * 8.0);
    if r.pad_front {
        arm(&mut s, SHOULDER_R, pe, ph);
    }
    arm(&mut s, SHOULDER_L, ee, eh);
    // Head.
    let _ = write!(s, r#"<g transform="{head_tf}">"#);
    head(&mut s, r);
    s.push_str("</g>");
    if r.pad_front {
        pad(&mut s, r);
        hand(&mut s, ph, false);
    }
    pen(&mut s, tip, pd);
    hand(&mut s, eh, true);
    s.push_str("</g>");
    extras(&mut s, r);
    s.push_str("</g></svg>");
    s
}

fn legs(s: &mut String) {
    for (hip, ankle) in [((93.0, 222.0), (92.0, 288.0)), ((107.0, 222.0), (108.0, 288.0))] {
        let _ = write!(
            s,
            r#"<path d="M{} {}L{} {}" stroke="{OL}" stroke-width="12.5" fill="none"/><path d="M{} {}L{} {}" stroke="{SKIN}" stroke-width="9" fill="none"/>"#,
            n(hip.0),
            n(hip.1),
            n(ankle.0),
            n(ankle.1),
            n(hip.0),
            n(hip.1),
            n(ankle.0),
            n(ankle.1)
        );
        // Knee-high shade line and socks with a frilled cuff.
        let (ax, ay) = ankle;
        let _ = write!(
            s,
            r#"<path d="M{} {}L{} {}" stroke="{OL}" stroke-width="12.5" fill="none"/><path d="M{} {}L{} {}" stroke="{CREAM}" stroke-width="9" fill="none"/><path d="M{} {}h9" stroke="{CREAM_DEEP}" stroke-width="2.2"/>"#,
            n(ax),
            n(ay - 10.0),
            n(ax),
            n(ay + 2.0),
            n(ax),
            n(ay - 10.0),
            n(ax),
            n(ay + 2.0),
            n(ax - 4.5),
            n(ay - 8.0)
        );
    }
    // Mary Jane shoes, toes slightly outward.
    for (cx, m) in [(91.0, -1.0), (109.0, 1.0)] {
        let x = |dx: f32| n(cx + m * dx);
        let _ = write!(
            s,
            r#"<path d="M{} 289C{} 287 {} 287 {} 289C{} 290 {} 293 {} 296C{} 299 {} 299.5 {} 299C{} 298.5 {} 296 {} 293Z" fill="{EMBER}" stroke="{OL}" stroke-width="1.8"/><path d="M{} 291.5L{} 290.5" stroke="{OL}" stroke-width="1.8"/><ellipse cx="{}" cy="294.6" rx="2.4" ry="1.1" fill="{AMBER_L}" opacity=".7"/>"#,
            x(-5.5),
            x(-4.0),
            x(3.0),
            x(5.0),
            x(9.0),
            x(12.0),
            x(12.0),
            x(12.0),
            x(2.0),
            x(-4.0),
            x(-7.0),
            x(-7.0),
            x(-6.0),
            x(-4.8),
            x(4.8),
            x(6.5)
        );
    }
}

fn torso(s: &mut String) {
    // Neck with the chin's shadow.
    let _ = write!(
        s,
        r#"<path d="M95 100L95 123L105 123L105 100Z" fill="{SKIN}" stroke="{OL}" stroke-width="1.8"/><path d="M95.9 106Q100 112 104.1 106L104.1 110Q100 114 95.9 110Z" fill="{SKIN_SH}"/>"#
    );
    // Sage dress, A-line, with a terracotta hem stripe.
    let _ = write!(
        s,
        r#"<path d="M89 121L111 121C113 140 114 158 115 170C124 188 133 207 139 226Q120 233.5 100 232.5Q80 233.5 61 226C67 207 76 188 85 170C86 158 87 140 89 121Z" fill="{SAGE}" stroke="{OL}" stroke-width="2"/><path d="M64.5 219Q100 226.5 135.5 219" stroke="{TERRA}" stroke-width="2.4" fill="none"/><path d="M86 171Q100 175 114 171" stroke="{SAGE_SH}" stroke-width="1.4" fill="none"/><path d="M86 186Q86 206 80 228M114 186Q114 206 120 228M100 180V232" stroke="{SAGE_SH}" stroke-width="1.4" fill="none"/>"#
    );
    // Cream cardigan, cropped at the waist, open at the front.
    for m in [-1.0f32, 1.0] {
        let x = |dx: f32| n(100.0 + m * dx);
        let _ = write!(
            s,
            r#"<path d="M{} 121C{} 121 {} 122 {} 125C{} 128 {} 135 {} 147L{} 175Q{} 179.5 {} 177.5C{} 162 {} 146 {} 130Z" fill="{CREAM}" stroke="{OL}" stroke-width="2"/><path d="M{} 171.5Q{} 175.5 {} 174" stroke="{CREAM_DEEP}" stroke-width="2.4" fill="none"/><path d="M{} 131C{} 146 {} 160 {} 173" stroke="{CREAM_DEEP}" stroke-width="1.8" fill="none"/>"#,
            x(1.0),
            x(8.0),
            x(15.0),
            x(19.0),
            x(24.0),
            x(26.0),
            x(26.0),
            x(26.5),
            x(17.0),
            x(8.0),
            x(7.0),
            x(5.0),
            x(3.0),
            x(25.5),
            x(17.0),
            x(9.5),
            x(5.5),
            x(6.8),
            x(8.2),
            x(9.6)
        );
    }
    // Buttons on her left front.
    for y in [142.0f32, 155.0, 168.0] {
        let _ = write!(s, r#"<circle cx="{}" cy="{}" r="1.7" fill="{AMBER}" stroke="{OL}" stroke-width="1"/>"#, n(92.6 - (y - 142.0) * 0.07), n(y));
    }
    // Terracotta ribbon at the collar.
    let _ = write!(
        s,
        r#"<path d="M99 125L96.5 135L99.5 133.5ZM101 125L103.5 135L100.5 133.5Z" fill="{EMBER}" stroke="{OL}" stroke-width="1.3"/><path d="M100 124.5L92 119.5Q88.5 124.5 92 129.5ZM100 124.5L108 119.5Q111.5 124.5 108 129.5Z" fill="{TERRA}" stroke="{OL}" stroke-width="1.4"/><circle cx="100" cy="124.5" r="2.6" fill="{EMBER}" stroke="{OL}" stroke-width="1.3"/>"#
    );
}

fn arm(s: &mut String, sh: P, el: P, ha: P) {
    let cuff = lerp(el, ha, 0.72);
    let _ = write!(
        s,
        r#"<path d="M{} {}L{} {}L{} {}" stroke="{OL}" stroke-width="13.6" fill="none"/><path d="M{} {}L{} {}L{} {}" stroke="{CREAM}" stroke-width="10" fill="none"/><path d="M{} {}L{} {}" stroke="{CREAM_DEEP}" stroke-width="10" stroke-linecap="butt" fill="none"/>"#,
        n(sh.0),
        n(sh.1),
        n(el.0),
        n(el.1),
        n(ha.0),
        n(ha.1),
        n(sh.0),
        n(sh.1),
        n(el.0),
        n(el.1),
        n(ha.0),
        n(ha.1),
        n(cuff.0),
        n(cuff.1),
        n(ha.0),
        n(ha.1)
    );
}

fn hand(s: &mut String, h: P, pen: bool) {
    let _ = write!(s, r#"<circle cx="{}" cy="{}" r="5.4" fill="{SKIN}" stroke="{OL}" stroke-width="1.6"/>"#, n(h.0), n(h.1));
    if pen {
        // Curled fingers.
        let _ = write!(s, r#"<path d="M{} {}q2.2 1.4 4.4 0" stroke="{SKIN_SH}" stroke-width="1.2" fill="none"/>"#, n(h.0 - 2.4), n(h.1 + 1.2));
    }
}

fn pen(s: &mut String, tip: P, d: P) {
    let a = (tip.0 + d.0 * 3.0, tip.1 + d.1 * 3.0);
    let b = (tip.0 + d.0 * 23.0, tip.1 + d.1 * 23.0);
    let _ = write!(
        s,
        r#"<path d="M{} {}L{} {}" stroke="{OL}" stroke-width="4.6" fill="none"/><path d="M{} {}L{} {}" stroke="{HAIR}" stroke-width="2.6" fill="none"/><path d="M{} {}L{} {}" stroke="{AMBER}" stroke-width="2.6" fill="none"/><circle cx="{}" cy="{}" r="1.3" fill="{AMBER}"/>"#,
        n(tip.0 + d.0 * 0.5),
        n(tip.1 + d.1 * 0.5),
        n(b.0),
        n(b.1),
        n(a.0),
        n(a.1),
        n(b.0),
        n(b.1),
        n(tip.0 + d.0 * 1.2),
        n(tip.1 + d.1 * 1.2),
        n(a.0),
        n(a.1),
        n(b.0),
        n(b.1)
    );
}

fn pad(s: &mut String, r: &Rig) {
    let (px, py, rot) = r.pad;
    let _ = write!(s, r#"<g transform="translate({} {}) rotate({})">"#, n(px), n(py), n(rot));
    let _ = write!(
        s,
        r#"<rect x="1.8" y="1.8" width="{PAD_W}" height="{PAD_H}" rx="3" fill="{EMBER}" stroke="{OL}" stroke-width="1.6"/><rect x="0" y="0" width="{PAD_W}" height="{PAD_H}" rx="3" fill="{AMBER}" stroke="{OL}" stroke-width="1.8"/><rect x="3" y="4.5" width="{}" height="{}" rx="1.2" fill="{CREAM}" stroke="{OL}" stroke-width="1"/>"#,
        PAD_W - 6.0,
        PAD_H - 7.5
    );
    let page = |s: &mut String, ink: f32| {
        for k in 0..5 {
            let y = 10.0 + k as f32 * 6.0;
            let _ = write!(s, r#"<path d="M5 {}H{}" stroke="{CREAM_DEEP}" stroke-width=".9"/>"#, n(y + 3.0), n(PAD_W - 5.0));
        }
        for k in 0..4 {
            let len = (ink - k as f32).clamp(0.0, 1.0) * if k == 3 { 12.0 } else { 20.0 };
            if len < 0.5 {
                continue;
            }
            let y = 12.0 + k as f32 * 6.0;
            let mut d = format!("M5 {}", n(y));
            let mut x = 5.0;
            while x < 5.0 + len {
                x = (x + 2.0).min(5.0 + len);
                let _ = write!(d, "L{} {}", n(x), n(y + 1.1 * (x * 1.7).sin()));
            }
            let c = if k == 0 { TERRA } else { HAIR };
            let _ = write!(s, r#"<path d="{d}" stroke="{c}" stroke-width="1.1" fill="none"/>"#);
        }
    };
    match r.flip {
        None => page(s, r.ink),
        Some(k) => {
            // New blank page underneath, the written one turning over the top.
            page(s, 0.0);
            let sy = (PI * k).cos();
            let _ = write!(s, r#"<g transform="translate(0 4.5) scale(1 {:.2}) translate(0 -4.5)">"#, sy);
            if sy > 0.0 {
                let _ = write!(s, r#"<rect x="3" y="4.5" width="{}" height="{}" rx="1.2" fill="{CREAM}" stroke="{OL}" stroke-width="1"/>"#, PAD_W - 6.0, PAD_H - 7.5);
                page(s, r.ink);
            } else {
                let _ = write!(s, r#"<rect x="3" y="4.5" width="{}" height="{}" rx="1.2" fill="{CREAM_DEEP}" stroke="{OL}" stroke-width="1"/>"#, PAD_W - 6.0, PAD_H - 7.5);
            }
            s.push_str("</g>");
        }
    }
    // Spiral binding.
    for k in 0..6 {
        let x = 4.5 + k as f32 * 4.2;
        let _ = write!(s, r#"<path d="M{} 6V-1.5" stroke="{OL}" stroke-width="2.6"/><path d="M{} 5.5V-1" stroke="{CREAM_DEEP}" stroke-width="1"/>"#, n(x), n(x));
    }
    s.push_str("</g>");
}

fn back_hair(s: &mut String, r: &Rig) {
    let w = r.swing;
    let _ = write!(
        s,
        r#"<path d="M100 33C72 33 61 52 62 78C62 92 62 101 {} 111C{} 114 72 113.5 77 108L123 108C128 113.5 {} 114 {} 111C138 101 138 92 138 78C139 52 128 33 100 33Z" fill="{HAIR_SH}" stroke="{OL}" stroke-width="2.2"/>"#,
        n(56.5 + w),
        n(64.0 + w),
        n(136.0 + w),
        n(143.5 + w)
    );
}

fn head(s: &mut String, r: &Rig) {
    // Face.
    let _ = write!(s, r#"<path d="M70 76C70 46 130 46 130 76C130 98 114 112.5 100 113C86 112.5 70 98 70 76Z" fill="{SKIN}" stroke="{OL}" stroke-width="2.2"/>"#);
    // Blush.
    if r.blush > 0.0 {
        let _ = write!(
            s,
            r#"<g fill="{BLUSH}" opacity="{:.2}"><ellipse cx="80" cy="100" rx="5.5" ry="2.7"/><ellipse cx="120" cy="100" rx="5.5" ry="2.7"/></g>"#,
            r.blush
        );
    }
    eye(s, r, 86.5, -1.0);
    eye(s, r, 113.5, 1.0);
    // Nose.
    let _ = write!(s, r#"<path d="M100.6 96.6l-.9 1.5" stroke="{SKIN_SH}" stroke-width="1.4"/>"#);
    mouth(s, r.mouth);
    // Ahoge behind the crown.
    let _ = write!(
        s,
        r#"<path transform="rotate({} 101 38)" d="M98.5 39C96 29 102 22 110 19.5C105 25 104 31 104.5 39Z" fill="{HAIR}" stroke="{OL}" stroke-width="2"/>"#,
        n(r.ahoge)
    );
    // Front hair: crown, fringe in soft points, side locks framing the face.
    let w = r.swing * 0.6;
    let _ = write!(
        s,
        r#"<path d="M63 84C61 52 78 36 100 36C122 36 139 52 137 84C137 94 136 103 {} 111C129 103 127 92 126 80Q124 70 122 61Q121 68 116 73Q113 66 110 59Q107 68 101 72.5Q98.5 66 95 58Q91.5 67 85 72.5Q83 66 80 60Q77 68 74 79C73 90 71 101 {} 111C64 103 63 94 63 84Z" fill="url(#hg)" stroke="{OL}" stroke-width="2.2"/>"#,
        n(132.0 + w),
        n(68.0 + w)
    );
    // Strand lines and a highlight band.
    let _ = write!(
        s,
        r#"<path d="M110 59Q108 50 102 41M95 58Q93 49 90 43M122 61Q121 53 116 45M80 60Q80 54 82 48" stroke="{HAIR_SH}" stroke-width="1.2" fill="none"/><path d="M76 52Q88 42 98 41M106 41Q117 42 124 50" stroke="{HAIR_HI}" stroke-width="3" fill="none"/>"#
    );
    // Brows, peeking through the fringe.
    let (li, lo, arch) = match r.brow {
        Brow::Soft => (75.5, 76.0, -2.0),
        Brow::Raised => (73.0, 74.0, -2.6),
        Brow::Worried => (71.0, 77.5, 0.6),
        Brow::Sleepy => (77.0, 77.0, -1.0),
    };
    for m in [-1.0f32, 1.0] {
        let inner = 100.0 + m * 9.0;
        let outer = 100.0 + m * 19.5;
        let _ = write!(
            s,
            r#"<path d="M{} {}Q{} {} {} {}" stroke="{BROW}" stroke-width="1.7" fill="none"/>"#,
            n(inner),
            n(li),
            n((inner + outer) / 2.0),
            n((li + lo) / 2.0 + arch),
            n(outer),
            n(lo)
        );
    }
    // Hair clip: a tiny amber cassette.
    let _ = write!(
        s,
        r#"<g transform="translate(123 58) rotate(20)"><rect x="-7.5" y="-5" width="15" height="10" rx="2.2" fill="{AMBER}" stroke="{OL}" stroke-width="1.5"/><rect x="-4.6" y="-2.3" width="9.2" height="4.4" rx="1.6" fill="{OL}"/><circle cx="-2.2" cy="-.1" r="1.2" fill="{AMBER_L}"/><circle cx="2.2" cy="-.1" r="1.2" fill="{AMBER_L}"/><path d="M-5 3.6h10" stroke="{TERRA}" stroke-width="1"/></g><path d="M134 50l1.2-3 1.2 3 3 1.2-3 1.2-1.2 3-1.2-3-3-1.2Z" fill="{AMBER_L}" stroke="{OL}" stroke-width=".8"/>"#
    );
    if let Extra::Sweat(t) = r.extra {
        let dy = 3.0 * t;
        let _ = write!(
            s,
            r#"<g transform="translate(137 {})"><path d="M0 -6C3 -1.5 4.4 1.5 2.6 4C1 6 -1 6 -2.6 4C-4.4 1.5 -3 -1.5 0 -6Z" fill="{SWEAT}" stroke="{OL}" stroke-width="1.3"/><ellipse cx="-.9" cy="1.6" rx=".9" ry="1.5" fill="white"/></g>"#,
            n(70.0 + dy)
        );
        // Little stress marks at the other temple.
        s.push_str(&format!(r#"<path d="M60 58l-6-4M58 66h-7M60 74l-6 3" stroke="{OL}" stroke-width="1.8"/>"#));
    }
}

fn eye(s: &mut String, r: &Rig, cx: f32, m: f32) {
    let cy = 89.0;
    let lash = |s: &mut String, yl: f32| {
        let _ = write!(
            s,
            r#"<path d="M{} {}Q{} {} {} {}L{} {}" stroke="{OL}" stroke-width="2.7" fill="none"/>"#,
            n(cx - m * 7.2),
            n(yl + 2.2),
            n(cx - m * 0.5),
            n(yl - 3.2),
            n(cx + m * 7.8),
            n(yl + 0.6),
            n(cx + m * 9.8),
            n(yl - 1.4)
        );
    };
    match r.eyes {
        Eyes::Closed => {
            let _ = write!(
                s,
                r#"<path d="M{} {}Q{} {} {} {}" stroke="{OL}" stroke-width="2.2" fill="none"/><path d="M{} {}l{} 1.6" stroke="{OL}" stroke-width="1.5"/>"#,
                n(cx - m * 7.0),
                n(cy + 1.5),
                n(cx),
                n(cy + 6.0),
                n(cx + m * 7.5),
                n(cy + 1.5),
                n(cx + m * 7.5),
                n(cy + 1.5),
                n(m * 1.8)
            );
        }
        Eyes::Happy => {
            let _ = write!(
                s,
                r#"<path d="M{} {}Q{} {} {} {}" stroke="{OL}" stroke-width="2.4" fill="none"/>"#,
                n(cx - m * 7.0),
                n(cy + 3.5),
                n(cx),
                n(cy - 5.0),
                n(cx + m * 7.5),
                n(cy + 3.5)
            );
        }
        Eyes::Open | Eyes::Wide => {
            let wide = r.eyes == Eyes::Wide;
            let (ry, irx, iry) = if wide { (10.0, 5.2, 7.0) } else { (9.2, 5.8, 7.8) };
            let ix = cx + r.look.0;
            let iy = cy + 1.0 + r.look.1;
            let id = if m < 0.0 { "el" } else { "er" };
            let _ = write!(
                s,
                r#"<clipPath id="{id}"><ellipse cx="{}" cy="{}" rx="7.4" ry="{ry}"/></clipPath><ellipse cx="{}" cy="{}" rx="7.4" ry="{ry}" fill="white"/><g clip-path="url(#{id})"><ellipse cx="{}" cy="{}" rx="{irx}" ry="{iry}" fill="url(#ir)" stroke="{OL}" stroke-width=".9"/><ellipse cx="{}" cy="{}" rx="2.6" ry="3.8" fill="{OL}"/><ellipse cx="{}" cy="{}" rx="3.6" ry="1.8" fill="{AMBER_L}" opacity=".55"/><circle cx="{}" cy="{}" r="2.4" fill="white"/><circle cx="{}" cy="{}" r="1.1" fill="white" opacity=".9"/>"#,
                n(cx),
                n(cy),
                n(cx),
                n(cy),
                n(ix),
                n(iy),
                n(ix),
                n(iy + 0.4),
                n(ix),
                n(iy + iry - 2.4),
                n(ix - 2.2),
                n(iy - 3.0),
                n(ix + 2.3),
                n(iy + 3.0)
            );
            let top = cy - ry + 0.6;
            let yl = top + r.lid * (2.0 * ry - 1.5);
            if r.lid > 0.0 {
                let _ = write!(s, r#"<rect x="{}" y="{}" width="18" height="{}" fill="{SKIN}"/>"#, n(cx - 9.0), n(cy - 13.0), n(yl - cy + 13.0 + 0.6));
            }
            s.push_str("</g>");
            if r.lid >= 0.95 {
                let _ = write!(
                    s,
                    r#"<path d="M{} {}Q{} {} {} {}" stroke="{OL}" stroke-width="2.2" fill="none"/>"#,
                    n(cx - m * 7.0),
                    n(cy + 1.5),
                    n(cx),
                    n(cy + 6.0),
                    n(cx + m * 7.5),
                    n(cy + 1.5)
                );
            } else {
                lash(s, yl);
                // Lower lash.
                let _ = write!(
                    s,
                    r##"<path d="M{} {}Q{} {} {} {}" stroke="#7A3D5E" stroke-width="1.2" fill="none"/>"##,
                    n(cx - m * 3.5),
                    n(cy + ry - 0.2),
                    n(cx + m * 1.0),
                    n(cy + ry + 0.8),
                    n(cx + m * 5.2),
                    n(cy + ry - 1.4)
                );
            }
        }
    }
}

fn mouth(s: &mut String, m: Mouth) {
    match m {
        Mouth::Smile => {
            let _ = write!(s, r#"<path d="M96 103Q100 106.5 104 103" stroke="{OL}" stroke-width="1.6" fill="none"/>"#);
        }
        Mouth::Open(h) => {
            let b = 103.0 + h + 1.5;
            let _ = write!(
                s,
                r#"<path d="M95.5 102.5Q100 103.6 104.5 102.5Q104 {} 100 {}Q96 {} 95.5 102.5Z" fill="{MOUTH}" stroke="{OL}" stroke-width="1.4"/><ellipse cx="100" cy="{}" rx="2.4" ry="{}" fill="{TONGUE}"/>"#,
                n(b),
                n(b + 0.4),
                n(b),
                n(b - 0.9),
                n((h * 0.35).clamp(0.4, 1.4))
            );
        }
        Mouth::O(r) => {
            let _ = write!(
                s,
                r#"<ellipse cx="100" cy="104.5" rx="{}" ry="{}" fill="{MOUTH}" stroke="{OL}" stroke-width="1.3"/>"#,
                n(r * 0.85),
                n(r)
            );
        }
        Mouth::Hmm => {
            let _ = write!(s, r#"<path d="M96.5 104.2Q99 103 103.5 104.6" stroke="{OL}" stroke-width="1.6" fill="none"/>"#);
        }
        Mouth::Worried => {
            let _ = write!(s, r#"<path d="M95.5 105.5q1.5-1.8 3 0t3 0 3 0" stroke="{OL}" stroke-width="1.5" fill="none"/>"#);
        }
    }
}

fn extras(s: &mut String, r: &Rig) {
    match r.extra {
        Extra::None | Extra::Sweat(_) => {}
        Extra::Listen(t) => {
            // Sound arcs reaching her ear, pulsing outward.
            for k in 0..2 {
                let u = (t * 2.0 + k as f32 * 0.5) % 1.0;
                let rad = 6.0 + u * 8.0;
                let op = (PI * u).sin() * 0.9;
                let _ = write!(
                    s,
                    r#"<path d="M{} {}A{} {} 0 0 1 {} {}" stroke="{TERRA}" stroke-width="2" fill="none" opacity="{:.2}"/>"#,
                    n(150.0 + rad * 0.5),
                    n(66.0 - rad * 0.85),
                    n(rad),
                    n(rad),
                    n(150.0 + rad * 0.5),
                    n(66.0 + rad * 0.85),
                    op
                );
            }
        }
        Extra::Bubble(t) => {
            let cloud = [(150.0, 24.0, 9.0), (161.0, 16.0, 10.5), (174.0, 19.0, 9.5), (180.0, 28.0, 7.0), (167.0, 30.0, 8.5), (154.0, 31.0, 7.0)];
            let lift = 1.0 * wave(t, 1.0, 0.0);
            let _ = write!(s, r#"<g transform="translate(0 {})">"#, n(lift));
            let _ = write!(s, r#"<circle cx="130" cy="47" r="2.6" fill="{CREAM}" stroke="{OL}" stroke-width="1.6"/><circle cx="139" cy="38" r="4" fill="{CREAM}" stroke="{OL}" stroke-width="1.6"/>"#);
            for (x, y, rr) in cloud {
                let _ = write!(s, r#"<circle cx="{x}" cy="{y}" r="{}" fill="{OL}"/>"#, n(rr + 1.8));
            }
            for (x, y, rr) in cloud {
                let _ = write!(s, r#"<circle cx="{x}" cy="{y}" r="{rr}" fill="{CREAM}"/>"#);
            }
            for k in 0..3 {
                let up = (TAU * (t * 2.0 - k as f32 / 6.0)).sin().max(0.0);
                let c = [AMBER, TERRA, EMBER][k];
                let _ = write!(s, r#"<circle cx="{}" cy="{}" r="2.5" fill="{c}"/>"#, 156 + k * 9, n(24.0 - 3.0 * up));
            }
            s.push_str("</g>");
        }
        Extra::Zz(t) => {
            for k in 0..3 {
                let u = (t + k as f32 / 3.0) % 1.0;
                let x = 132.0 + u * 26.0 + 3.0 * (TAU * u).sin();
                let y = 58.0 - u * 46.0;
                let z = 4.0 + u * 5.0;
                let op = (PI * u).sin().powf(0.6);
                let d = format!("M{} {}h{}l-{} {}h{}", n(x - z / 2.0), n(y - z / 2.0), n(z), n(z), n(z), n(z));
                let _ = write!(
                    s,
                    r#"<g opacity="{op:.2}"><path d="{d}" stroke="{OL}" stroke-width="{}" fill="none"/><path d="{d}" stroke="{AMBER_L}" stroke-width="{}" fill="none"/></g>"#,
                    n(2.0 + z * 0.35),
                    n(z * 0.28)
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_frame_is_a_small_svg_without_text() {
        for pose in Pose::ALL {
            for f in 0..frame_count(pose) {
                let svg = frame_svg(pose, f);
                assert!(svg.starts_with("<svg") && svg.ends_with("</svg>"));
                assert!(!svg.contains("<text"));
                assert!(svg.len() < 25_000, "{pose:?} {f}: {} bytes", svg.len());
            }
        }
    }
}
