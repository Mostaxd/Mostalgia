//! Hoshi's artwork: the stargazing archivist, a kid in an oversized
//! observatory coat with a brass telescope and a little star chart. Every
//! animation frame is generated as an SVG document (transparent background,
//! viewBox FRAME_W × FRAME_H), in the same way as Mori's (`companion::art`).
//!
//! She is drawn as a small rig: the body parts are fixed shapes, and each
//! pose only decides where the joints go for a phase `t` in `0..1` (head
//! tilt, eye state, where the telescope and the chart are; the arms follow
//! by two-bone IK). All motion is built from `sin`/`cos` of `2πt`, so every
//! pose loops seamlessly. No `<text>` and no filters: frames must render the
//! same in resvg and GPUI without fonts.
//!
//! Her way of showing the meeting's notes is a night sky (`notes_svg`):
//! every note is a star, shaped by its kind, and the notes of one stretch of
//! the meeting are joined into a constellation.

use std::f32::consts::{PI, TAU};
use std::fmt::{self, Write as _};

use super::PointKind;

/// What Hoshi is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pose {
    /// Breathing, blinking, the telescope held like a walking stick.
    Idle,
    /// Someone started speaking: peeks up through the telescope.
    Listening,
    /// Speech going on: plots stars on her chart, then sends them up.
    Writing,
    /// Waiting for the language model: gazes up at a twinkling star.
    Thinking,
    /// Showing an answer or a point: holds up the chart, points the way.
    Talking,
    /// Recording paused: dozing curled up in her coat, cap pulled down.
    Sleeping,
    /// The provider failed: the chart tipped over, stars spilling out.
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

const OL: &str = "#2B1730"; // Night Plum: outlines, shared with Mori
const SKIN: &str = "#FFE7D6";
const SKIN_SH: &str = "#F4C5AE";
const HAIR: &str = "#2E3870"; // Midnight
const HAIR_TOP: &str = "#45539A";
const HAIR_HI: &str = "#7486C9";
const HAIR_SH: &str = "#222A58";
const COAT: &str = "#3B4A8E"; // Night Blue
const COAT_SH: &str = "#2C3870";
const COAT_HI: &str = "#5568B0";
const CAP: &str = "#26306A";
const TIGHTS: &str = "#2A2F5C";
const BOOT: &str = "#5B2A4E"; // Plum
const CREAM: &str = "#FBF3E4";
const GOLD: &str = "#F2A541"; // Sunset Amber: brass, buttons
const GOLD_L: &str = "#F7C26B";
const STAR: &str = "#FFE29A"; // Star gold
const EMBER: &str = "#B8472A";
const TERRA: &str = "#D9653B";
const CHART: &str = "#1F2758";
const GLASS: &str = "#BFE0E4";
const MOUTH: &str = "#8A3444";
const TONGUE: &str = "#E9877A";
const BLUSH: &str = "#F2876F";
const BROW: &str = "#222A58";
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

fn add(a: P, d: P, k: f32) -> P {
    (a.0 + d.0 * k, a.1 + d.1 * k)
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

/// A small deterministic hash of two numbers, in `0..1` (stands in for a
/// random generator: the sky must look the same every time it's drawn).
fn hash(a: u32, b: u32) -> f32 {
    let mut x = a.wrapping_mul(0x9E37_79B1) ^ b.wrapping_mul(0x85EB_CA77) ^ 0x2545_F491;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    x = x.wrapping_mul(0x297A_2D39);
    x ^= x >> 15;
    (x & 0xFF_FFFF) as f32 / 16_777_216.0
}

/// A pointed star (`points` tips, radii `ro` / `ri`), as path data.
fn star_d(c: P, ro: f32, ri: f32, points: usize, rot: f32) -> String {
    let mut d = String::new();
    for k in 0..points * 2 {
        let r = if k % 2 == 0 { ro } else { ri };
        let p = add(c, dir(rot - 90.0 + k as f32 * 180.0 / points as f32), r);
        let _ = write!(d, "{}{} {}", if k == 0 { 'M' } else { 'L' }, n(p.0), n(p.1));
    }
    d.push('Z');
    d
}

/// A four-pointed sparkle with curved sides, as path data.
fn sparkle_d(c: P, r: f32) -> String {
    let (x, y, k) = (c.0, c.1, r * 0.16);
    format!(
        "M{} {}Q{} {} {} {}Q{} {} {} {}Q{} {} {} {}Q{} {} {} {}Z",
        n(x),
        n(y - r),
        n(x + k),
        n(y - k),
        n(x + r),
        n(y),
        n(x + k),
        n(y + k),
        n(x),
        n(y + r),
        n(x - k),
        n(y + k),
        n(x - r),
        n(y),
        n(x - k),
        n(y - k),
        n(x),
        n(y - r)
    )
}

// ---------------------------------------------------------------- the rig

#[derive(Clone, Copy, PartialEq)]
enum Eyes {
    Open,
    Wide,
    Closed,
    Happy,
    /// Squeezed shut, flustered ("> <").
    Squeeze,
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

/// What is drawn on the star chart.
#[derive(Clone, Copy)]
enum Ink {
    /// Her usual little constellation.
    Fixed,
    /// Writing: stars plotted one by one over frames 0..12, then lifted off
    /// the chart into the sky.
    Plot(usize),
    /// Error: the stars have slid off; the chart is empty.
    Empty,
}

#[derive(Clone, Copy)]
enum Extra {
    None,
    /// Glints at the telescope's far end.
    Glints(f32),
    /// A big star twinkling over her head, three motes circling it.
    Twinkle(f32),
    /// A sparkle at the tip of her pointer.
    Point(f32),
    /// A tiny crescent moon and drifting z's.
    Moon(f32),
    /// Stars spilling off the chart, a falling star, a sweat drop.
    Spill(f32),
}

/// The telescope: eyepiece position, direction from the eyepiece to the
/// lens (degrees), length (collapsed ≈ 15, fully drawn out ≈ 56) and where
/// along it her hand holds it (0 = eyepiece, 1 = lens).
#[derive(Clone, Copy)]
struct Scope {
    eye: P,
    ang: f32,
    len: f32,
    grip: f32,
}

impl Scope {
    fn hand(&self) -> P {
        add(self.eye, dir(self.ang), self.len * self.grip)
    }

    /// The telescope moved so her hand holds it at `hand`.
    fn held_at(self, hand: P) -> Scope {
        Scope { eye: add(hand, dir(self.ang), -self.len * self.grip), ..self }
    }
}

struct Rig {
    /// Whole figure: horizontal shake and sway (degrees around the feet).
    shake: f32,
    sway: f32,
    /// Upper body offset (breathing).
    bob: f32,
    /// Sitting on the ground (sleeping): the upper body sinks this far.
    seated: bool,
    head_rot: f32,
    head_dx: f32,
    head_dy: f32,
    eyes: Eyes,
    /// Her right eye (viewer's left) squeezed shut behind the eyepiece.
    wink: bool,
    look: P,
    /// Upper lid, 0 = open, 1 = shut.
    lid: f32,
    mouth: Mouth,
    brow: Brow,
    blush: f32,
    /// Pigtail swing (degrees) and the cap's slide down over the brow.
    swing: f32,
    cap: f32,
    /// Chart top-left corner and rotation; where its hand holds it.
    chart: (f32, f32, f32),
    chart_grip: P,
    chart_bend: f32,
    chart_front: bool,
    ink: Ink,
    /// The telescope in her right hand (viewer's left), or that hand alone.
    scope: Option<Scope>,
    hand_r: P,
    scope_bend: f32,
    extra: Extra,
}

const CHART_W: f32 = 26.0;
const CHART_H: f32 = 32.0;
/// The stars she plots on her chart (chart coordinates).
const CHART_STARS: [P; 4] = [(6.5, 24.0), (10.5, 12.0), (18.5, 18.5), (20.0, 7.5)];
const SHOULDER_L: P = (82.0, 165.0);
const SHOULDER_R: P = (118.0, 165.0);
const UPPER_ARM: f32 = 23.0;
const FOREARM: f32 = 22.0;
/// The head is drawn at its native scale (chin at y 113), then scaled
/// around the chin onto the neck. She's a kid: a big head on a small body.
const HEAD_SCALE: f32 = 1.04;
const NECK: P = (100.0, 150.0);
/// How far the upper body sinks when she sits down.
const SIT: f32 = 40.0;

fn base() -> Rig {
    Rig {
        shake: 0.0,
        sway: 0.0,
        bob: 0.0,
        seated: false,
        head_rot: 0.0,
        head_dx: 0.0,
        head_dy: 0.0,
        eyes: Eyes::Open,
        wink: false,
        look: (0.0, 0.0),
        lid: 0.0,
        mouth: Mouth::Smile,
        brow: Brow::Soft,
        blush: 0.5,
        swing: 0.0,
        cap: 0.0,
        chart: (105.0, 178.0, -7.0),
        chart_grip: (CHART_W + 1.0, 26.0),
        chart_bend: -1.0,
        chart_front: false,
        ink: Ink::Fixed,
        scope: Some(Scope { eye: (72.0, 228.0), ang: -102.0, len: 56.0, grip: 0.3 }),
        hand_r: (70.0, 205.0),
        scope_bend: 1.0,
        extra: Extra::None,
    }
}

fn chart_point(r: &Rig, local: P) -> P {
    let (px, py, rot) = r.chart;
    let (c, s) = (rot.to_radians().cos(), rot.to_radians().sin());
    (px + local.0 * c - local.1 * s, py + local.0 * s + local.1 * c)
}

/// A point of the head (native coordinates) in body coordinates.
fn head_point(r: &Rig, p: P) -> P {
    let q = (NECK.0 + (p.0 - 100.0) * HEAD_SCALE, NECK.1 + (p.1 - 113.0) * HEAD_SCALE);
    let (c, s) = (r.head_rot.to_radians().cos(), r.head_rot.to_radians().sin());
    let (dx, dy) = (q.0 - NECK.0, q.1 - NECK.1);
    (NECK.0 + dx * c - dy * s + r.head_dx, NECK.1 + dx * s + dy * c + r.head_dy)
}

/// Where the stylus (her collapsed telescope) is while plotting, in chart
/// coordinates, and how far it is lifted off the chart.
fn plot_tip(frame: usize) -> (P, f32) {
    let star = |k: usize| CHART_STARS[k % 4];
    if frame < 12 {
        let (k, sub) = (frame / 3, frame % 3);
        let prev = if k == 0 { lerp(star(3), star(0), 0.8) } else { star(k - 1) };
        match sub {
            0 => (lerp(prev, star(k), 0.65), 3.5),
            1 => (star(k), 0.0),
            _ => (star(k), 1.6),
        }
    } else {
        let k = (frame - 11) as f32 / 5.0;
        (lerp(star(3), star(0), 0.5 - 0.5 * (PI * k).cos()), 3.5 + 3.0 * (PI * k).sin())
    }
}

fn rig(pose: Pose, frame: usize, t: f32) -> Rig {
    let mut r = base();
    match pose {
        Pose::Idle => {
            r.sway = 0.8 * wave(t, 1.0, 0.0);
            r.bob = -0.9 * (0.5 - 0.5 * (TAU * 2.0 * t).cos());
            r.head_rot = 2.0 * wave(t, 1.0, 0.1);
            r.swing = -r.head_rot * 1.6 + 3.0 * wave(t, 2.0, 0.2);
            r.lid = match frame {
                14 => 0.55,
                15 => 1.0,
                16 => 0.35,
                _ => 0.0,
            };
            if let Some(sc) = r.scope.as_mut() {
                sc.ang = -102.0 + 2.0 * wave(t, 1.0, 0.0);
            }
        }
        Pose::Listening => {
            let nod = wave(t, 2.0, 0.0);
            r.head_rot = -6.0 + 1.0 * wave(t, 1.0, 0.0);
            r.head_dy = -1.0 + 0.6 * nod;
            r.bob = -0.5 * (0.5 - 0.5 * (TAU * t).cos());
            r.eyes = Eyes::Wide;
            r.wink = true;
            r.look = (-1.2, -1.8);
            r.brow = Brow::Raised;
            r.mouth = Mouth::O(1.5 + 0.2 * nod);
            r.swing = 4.0 * wave(t, 2.0, 0.1);
            r.chart = (106.0, 184.0, -14.0);
            let eye = head_point(&r, (86.0, 91.0));
            let ang = -146.0 + 1.5 * wave(t, 1.0, 0.25);
            r.scope = Some(Scope { eye: add(eye, dir(ang), -1.0), ang, len: 48.0, grip: 0.2 });
            r.scope_bend = 1.0;
            r.extra = Extra::Glints(t);
        }
        Pose::Writing => {
            r.chart = (98.0, 184.0, 10.0);
            r.chart_grip = (CHART_W + 1.5, CHART_H - 3.0);
            r.ink = Ink::Plot(frame);
            r.head_rot = 4.0 + 1.0 * wave(t, 1.0, 0.0);
            r.head_dy = 2.5;
            r.head_dx = 1.0;
            r.look = (1.0, 2.4);
            r.lid = 0.36;
            r.mouth = Mouth::Smile;
            r.swing = -5.0 + 2.0 * wave(t, 2.0, 0.0);
            r.bob = -0.5 * (0.5 - 0.5 * (TAU * t).cos());
            let (tip, lift) = plot_tip(frame);
            let tip = chart_point(&r, (tip.0, tip.1 - lift));
            r.scope = Some(Scope { eye: tip, ang: -124.0, len: 18.0, grip: 0.9 });
        }
        Pose::Thinking => {
            r.head_rot = 6.0 + 1.2 * wave(t, 1.0, 0.0);
            r.head_dx = 0.5;
            r.look = (1.8, -2.6);
            r.lid = 0.1;
            r.brow = Brow::Raised;
            r.mouth = Mouth::Hmm;
            r.swing = -r.head_rot * 0.8;
            r.bob = -0.6 * (0.5 - 0.5 * (TAU * t).cos());
            r.chart = (106.0, 182.0, -4.0);
            let tap = wave(t, 2.0, 0.0).max(0.0);
            let chin = head_point(&r, (103.0, 112.0));
            r.scope = Some(Scope { eye: (chin.0, chin.1 + 1.0 - 1.6 * tap), ang: 72.0, len: 15.0, grip: 0.62 });
            r.extra = Extra::Twinkle(t);
        }
        Pose::Talking => {
            let syl = (TAU * 2.0 * t).sin().abs();
            r.head_rot = 2.0 * wave(t, 1.0, 0.0);
            r.head_dy = -0.6 * syl;
            r.bob = -0.6 * (0.5 - 0.5 * (TAU * t).cos());
            r.mouth = if syl > 0.2 { Mouth::Open(3.2 * syl) } else { Mouth::Smile };
            r.eyes = if (6..=7).contains(&frame) { Eyes::Happy } else { Eyes::Open };
            r.brow = Brow::Raised;
            r.blush = 0.6;
            r.swing = -r.head_rot * 1.2 + 3.0 * wave(t, 2.0, 0.2);
            r.chart = (108.0, 166.0, 6.0);
            r.chart_grip = (2.0, 24.0);
            r.chart_bend = -1.0;
            let g = wave(t, 1.0, 0.0);
            let ang = -132.0 + 6.0 * g;
            let sc = Scope { eye: (0.0, 0.0), ang, len: 44.0, grip: 0.16 };
            r.scope = Some(sc.held_at((63.0 + 2.0 * g, 150.0 - 3.0 * syl)));
            r.scope_bend = 1.0;
            r.extra = Extra::Point(t);
        }
        Pose::Sleeping => {
            let s = wave(t, 1.0, 0.0);
            r.seated = true;
            r.head_rot = 10.0 + 2.5 * s;
            r.head_dy = 3.0 + 1.2 * s;
            r.head_dx = 1.0;
            r.bob = -1.2 * (0.5 - 0.5 * (TAU * t).cos());
            r.eyes = Eyes::Closed;
            r.brow = Brow::Sleepy;
            r.mouth = Mouth::O(1.0 + 0.3 * s);
            r.blush = 0.65;
            r.cap = 1.0;
            r.swing = 8.0 - 2.0 * s;
            r.chart = (86.0, 190.0, -10.0);
            r.chart_grip = (CHART_W - 2.0, 22.0);
            r.chart_front = true;
            r.scope = None;
            r.hand_r = chart_point(&r, (6.0, 25.0));
            r.extra = Extra::Moon(t);
        }
        Pose::Error => {
            r.shake = 1.4 * wave(t, 3.0, 0.0);
            r.head_rot = -2.0 + 1.5 * wave(t, 3.0, 0.15);
            r.eyes = Eyes::Squeeze;
            r.brow = Brow::Worried;
            r.mouth = Mouth::Worried;
            r.blush = 0.7;
            r.swing = 9.0 * wave(t, 3.0, 0.3);
            r.cap = 0.25;
            r.chart = (134.0, 166.0, 62.0);
            r.chart_grip = (2.0, 9.0);
            r.chart_front = true;
            r.ink = Ink::Empty;
            let sc = Scope { eye: (0.0, 0.0), ang: -62.0 + 6.0 * wave(t, 3.0, 0.0), len: 15.0, grip: 0.5 };
            r.scope = Some(sc.held_at((70.0, 146.0)));
            r.extra = Extra::Spill(t);
        }
    }
    r
}

// ---------------------------------------------------------------- drawing

fn draw(r: &Rig) -> String {
    let mut s = String::with_capacity(20 * 1024);
    let _ = write!(
        s,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}"><defs><linearGradient id="hir" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#1E2656"/><stop offset=".55" stop-color="#3D56A6"/><stop offset="1" stop-color="{GOLD_L}"/></linearGradient><linearGradient id="hhg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{HAIR_TOP}"/><stop offset="1" stop-color="{HAIR}"/></linearGradient></defs>"##,
        w = FRAME_W,
        h = FRAME_H
    );
    // Ground shadow (not swayed).
    let rx = if r.seated { 52 } else { 34 };
    let _ = write!(s, r##"<ellipse cx="100" cy="300" rx="{rx}" ry="4.5" fill="#2B1730" opacity=".13"/>"##);
    let _ = write!(s, r#"<g transform="translate({} 0) rotate({} 100 298)" stroke-linejoin="round" stroke-linecap="round">"#, n(r.shake), n(r.sway));
    if !r.seated {
        legs(&mut s);
    }
    let sit = if r.seated { SIT } else { 0.0 };
    let _ = write!(s, r#"<g transform="translate(0 {})">"#, n(r.bob + sit));
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
    back_hair(&mut s);
    s.push_str("</g>");
    torso(&mut s, r.seated);
    // Chart arm (her left, viewer's right).
    let grip = chart_point(r, r.chart_grip);
    let (ce, ch) = ik(SHOULDER_R, grip, UPPER_ARM, FOREARM, r.chart_bend);
    if !r.chart_front {
        sleeve(&mut s, SHOULDER_R, ce, ch);
        chart(&mut s, r);
        hand(&mut s, ce, ch);
    }
    // Telescope arm (her right, viewer's left).
    let target = r.scope.map_or(r.hand_r, |sc| sc.hand());
    let (te, th) = ik(SHOULDER_L, target, UPPER_ARM, FOREARM, r.scope_bend);
    // Keep the telescope in the hand even when the target was out of reach.
    let scope = r.scope.map(|sc| sc.held_at(th));
    if r.chart_front {
        sleeve(&mut s, SHOULDER_R, ce, ch);
    }
    sleeve(&mut s, SHOULDER_L, te, th);
    // Head.
    let _ = write!(s, r#"<g transform="{head_tf}">"#);
    head(&mut s, r);
    s.push_str("</g>");
    if r.chart_front {
        chart(&mut s, r);
        hand(&mut s, ce, ch);
    }
    if let Some(sc) = scope {
        telescope(&mut s, &sc);
    }
    hand(&mut s, te, th);
    extras(&mut s, r, scope);
    s.push_str("</g></g></svg>");
    s
}

fn legs(s: &mut String) {
    for (hip, ankle) in [((93.0, 250.0), (92.5, 287.0)), ((107.0, 250.0), (107.5, 287.0))] {
        let _ = write!(
            s,
            r#"<path d="M{} {}L{} {}" stroke="{OL}" stroke-width="12" fill="none"/><path d="M{} {}L{} {}" stroke="{TIGHTS}" stroke-width="8.6" fill="none"/>"#,
            n(hip.0),
            n(hip.1),
            n(ankle.0),
            n(ankle.1),
            n(hip.0),
            n(hip.1),
            n(ankle.0),
            n(ankle.1)
        );
    }
    // Plum ankle boots with a star-gold strap, toes slightly outward.
    for (cx, m) in [(92.0, -1.0), (108.0, 1.0)] {
        boot(s, cx, m, 0.0);
    }
}

fn boot(s: &mut String, cx: f32, m: f32, dy: f32) {
    let x = |dx: f32| n(cx + m * dx);
    let y = |v: f32| n(v + dy);
    let _ = write!(
        s,
        r#"<path d="M{} {}L{} {}L{} {}C{} {} {} {} {} {}C{} {} {} {} {} {}C{} {} {} {} {} {}Z" fill="{BOOT}" stroke="{OL}" stroke-width="1.8"/><path d="M{} {}L{} {}" stroke="{GOLD}" stroke-width="2"/><ellipse cx="{}" cy="{}" rx="2.4" ry="1" fill="{HAIR_HI}" opacity=".5"/>"#,
        x(-6.0),
        y(281.0),
        x(6.0),
        y(281.0),
        x(6.5),
        y(290.0),
        x(10.5),
        y(291.0),
        x(13.0),
        y(294.0),
        x(12.5),
        y(297.0),
        x(12.0),
        y(299.5),
        x(2.0),
        y(300.0),
        x(-6.5),
        y(299.5),
        x(-8.0),
        y(296.0),
        x(-7.0),
        y(287.0),
        x(-6.0),
        y(281.0),
        x(-6.0),
        y(286.0),
        x(6.2),
        y(286.0),
        x(7.0),
        y(294.0)
    );
}

fn torso(s: &mut String, seated: bool) {
    // Neck with the chin's shadow.
    let _ = write!(
        s,
        r#"<path d="M95.5 138L95.5 158L104.5 158L104.5 138Z" fill="{SKIN}" stroke="{OL}" stroke-width="1.8"/><path d="M96.4 143Q100 149 103.6 143L103.6 147Q100 151 96.4 147Z" fill="{SKIN_SH}"/>"#
    );
    if seated {
        // Sitting with her knees drawn up: the coat spreads into a round
        // heap, the boot toes peeking out under the hem.
        let _ = write!(
            s,
            r#"<path d="M91 152L109 152C114 154 119 157 121 163C129 190 148 220 151 248Q152 258 140 259Q100 263 60 259Q48 258 49 248C52 220 71 190 79 163C81 157 86 154 91 152Z" fill="{COAT}" stroke="{OL}" stroke-width="2"/><path d="M84 215Q72 236 64 256M116 215Q128 236 136 256" stroke="{COAT_SH}" stroke-width="1.6" fill="none"/><path d="M53 249Q100 258 147 249" stroke="{COAT_SH}" stroke-width="2.2" fill="none"/>"#
        );
        for (cx, m) in [(93.0, -1.0), (107.0, 1.0)] {
            let _ = write!(
                s,
                r#"<path d="M{} 254C{} 252 {} 252 {} 255C{} 259 {} 261 {} 261C{} 261 {} 259 {} 254Z" fill="{BOOT}" stroke="{OL}" stroke-width="1.8"/>"#,
                n(cx - m * 6.0),
                n(cx - m * 3.0),
                n(cx + m * 4.0),
                n(cx + m * 7.0),
                n(cx + m * 9.0),
                n(cx + m * 6.0),
                n(cx + m * 1.0),
                n(cx - m * 4.0),
                n(cx - m * 7.0),
                n(cx - m * 6.0)
            );
        }
        hem_stars(s, 249.0);
    } else {
        // The oversized coat, almost to her ankles, flaring out.
        let _ = write!(
            s,
            r#"<path d="M91 152L109 152C114 154 119 157 121 163C124 186 132 222 146 258Q123 267 100 266Q77 267 54 258C68 222 76 186 79 163C81 157 86 154 91 152Z" fill="{COAT}" stroke="{OL}" stroke-width="2"/><path d="M89 205Q85 232 74 259M111 205Q115 232 126 259" stroke="{COAT_SH}" stroke-width="1.6" fill="none"/><path d="M57 250Q100 261 143 250" stroke="{COAT_SH}" stroke-width="2.2" fill="none"/><path d="M100 186V265" stroke="{COAT_SH}" stroke-width="1.6"/>"#
        );
        hem_stars(s, 250.0);
        // Pocket with a gold stitch.
        let _ = write!(
            s,
            r#"<path d="M108 222L125 220L126 233Q117 236 109 234Z" fill="{COAT_SH}" stroke="{OL}" stroke-width="1.5"/><path d="M108 222L125 220" stroke="{GOLD}" stroke-width="1.4"/>"#
        );
        // Double row of brass buttons.
        for (x, y) in [(94.0, 196.0), (106.0, 196.0), (94.0, 211.0), (106.0, 211.0)] {
            let _ = write!(s, r#"<circle cx="{x}" cy="{y}" r="1.9" fill="{GOLD}" stroke="{OL}" stroke-width="1"/>"#);
        }
    }
    // Cream shirt in the V, wide lapels, a star-gold scarf with one end
    // hanging down.
    let _ = write!(s, r#"<path d="M93 152L107 152L100 170Z" fill="{CREAM}" stroke="{OL}" stroke-width="1.4"/>"#);
    for m in [-1.0f32, 1.0] {
        let x = |dx: f32| n(100.0 + m * dx);
        let _ = write!(
            s,
            r#"<path d="M{} 152C{} 153 {} 157 {} 162L{} 172L{} 175L{} 190L{} 170Z" fill="{COAT_HI}" stroke="{OL}" stroke-width="1.7"/>"#,
            x(7.0),
            x(12.0),
            x(16.0),
            x(18.0),
            x(10.5),
            x(13.5),
            x(0.0),
            x(0.5)
        );
    }
    let _ = write!(
        s,
        r#"<path d="M104 160L113.5 159.5L117 186L113.5 184L111.5 188L109 184.5L106.5 187.5Z" fill="{GOLD}" stroke="{OL}" stroke-width="1.7"/><path d="M109.5 167L111 182M113 166.5L114.5 181" stroke="{GOLD_L}" stroke-width="1.3"/><path d="M86.5 150.5Q100 161 113.5 150.5L115 157.5Q100 170 85 157.5Z" fill="{GOLD}" stroke="{OL}" stroke-width="1.8"/><path d="M87 154Q100 163.5 113 154" stroke="{GOLD_L}" stroke-width="1.4" fill="none"/><path d="M103 158Q108 155 112 159Q111 164 106 164Q102 162 103 158Z" fill="{GOLD}" stroke="{OL}" stroke-width="1.5"/>"#
    );
}

/// The little constellation embroidered along the hem.
fn hem_stars(s: &mut String, y: f32) {
    let pts = [(64.0, y + 3.5), (77.0, y + 1.0), (88.0, y + 6.0), (124.0, y + 4.5), (136.0, y + 1.0)];
    let _ = write!(
        s,
        r#"<path d="M{} {}L{} {}L{} {}M{} {}L{} {}" stroke="{GOLD_L}" stroke-width=".8" stroke-dasharray="1.6 1.6" fill="none" opacity=".8"/>"#,
        n(pts[0].0),
        n(pts[0].1),
        n(pts[1].0),
        n(pts[1].1),
        n(pts[2].0),
        n(pts[2].1),
        n(pts[3].0),
        n(pts[3].1),
        n(pts[4].0),
        n(pts[4].1)
    );
    for (k, p) in pts.into_iter().enumerate() {
        let r = if k % 2 == 0 { 2.6 } else { 2.0 };
        let _ = write!(s, r#"<path d="{}" fill="{GOLD_L}"/>"#, sparkle_d(p, r));
    }
}

/// The coat sleeve, too long for her (the cuff is drawn with the hand).
fn sleeve(s: &mut String, sh: P, el: P, ha: P) {
    let (dx, dy) = (ha.0 - el.0, ha.1 - el.1);
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let d = (dx / len, dy / len);
    let wrist = add(ha, d, -4.0);
    let _ = write!(
        s,
        r#"<path d="M{} {}L{} {}L{} {}" stroke="{OL}" stroke-width="15.6" fill="none"/><path d="M{} {}L{} {}L{} {}" stroke="{COAT}" stroke-width="12" fill="none"/><path d="M{} {}Q{} {} {} {}" stroke="{COAT_SH}" stroke-width="1.4" fill="none"/>"#,
        n(sh.0),
        n(sh.1),
        n(el.0),
        n(el.1),
        n(wrist.0),
        n(wrist.1),
        n(sh.0),
        n(sh.1),
        n(el.0),
        n(el.1),
        n(wrist.0),
        n(wrist.1),
        n(lerp(el, wrist, 0.15).0 - d.1 * 2.0),
        n(lerp(el, wrist, 0.15).1 + d.0 * 2.0),
        n(lerp(el, wrist, 0.5).0 - d.1 * 3.5),
        n(lerp(el, wrist, 0.5).1 + d.0 * 3.5),
        n(lerp(el, wrist, 0.8).0 - d.1 * 2.0),
        n(lerp(el, wrist, 0.8).1 + d.0 * 2.0)
    );
}

/// Her hand, mostly swallowed by the wide turned-back cuff of the sleeve:
/// only the fingers peek out (drawn over whatever the hand holds).
fn hand(s: &mut String, el: P, ha: P) {
    let (dx, dy) = (ha.0 - el.0, ha.1 - el.1);
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let d = (dx / len, dy / len);
    let paw = add(ha, d, 0.6);
    let _ = write!(s, r#"<circle cx="{}" cy="{}" r="4.4" fill="{SKIN}" stroke="{OL}" stroke-width="1.6"/>"#, n(paw.0), n(paw.1));
    let nrm = (-d.1, d.0);
    let b = add(ha, d, -9.5);
    let f = add(ha, d, -2.4);
    let q = [add(b, nrm, 6.2), add(f, nrm, 7.6), add(f, nrm, -7.6), add(b, nrm, -6.2)];
    let _ = write!(
        s,
        r#"<path d="M{} {}L{} {}Q{} {} {} {}L{} {}Z" fill="{COAT_HI}" stroke="{OL}" stroke-width="1.8"/><path d="M{} {}L{} {}" stroke="{CREAM}" stroke-width="2.2"/>"#,
        n(q[0].0),
        n(q[0].1),
        n(q[1].0),
        n(q[1].1),
        n(add(f, d, 1.6).0),
        n(add(f, d, 1.6).1),
        n(q[2].0),
        n(q[2].1),
        n(q[3].0),
        n(q[3].1),
        n(add(q[1], d, -1.3).0),
        n(add(q[1], d, -1.3).1),
        n(add(q[2], d, -1.3).0),
        n(add(q[2], d, -1.3).1)
    );
}

fn telescope(s: &mut String, sc: &Scope) {
    let d = dir(sc.ang);
    let l = sc.len;
    let seg = |s: &mut String, a: f32, b: f32, w: f32, fill: &str, hi: &str| {
        let (pa, pb) = (add(sc.eye, d, a), add(sc.eye, d, b));
        let off = (-d.1 * w * 0.22, d.0 * w * 0.22);
        let _ = write!(
            s,
            r#"<path d="M{} {}L{} {}" stroke="{OL}" stroke-width="{}" stroke-linecap="butt"/><path d="M{} {}L{} {}" stroke="{fill}" stroke-width="{w}" stroke-linecap="butt"/><path d="M{} {}L{} {}" stroke="{hi}" stroke-width="{}" stroke-linecap="butt"/>"#,
            n(add(pa, d, -0.9).0),
            n(add(pa, d, -0.9).1),
            n(add(pb, d, 0.9).0),
            n(add(pb, d, 0.9).1),
            n(w + 3.2),
            n(pa.0),
            n(pa.1),
            n(pb.0),
            n(pb.1),
            n(pa.0 - off.0),
            n(pa.1 - off.1),
            n(pb.0 - off.0),
            n(pb.1 - off.1),
            n(w * 0.25)
        );
    };
    let ring = |s: &mut String, a: f32, w: f32| {
        let (pa, pb) = (add(sc.eye, d, a - 1.0), add(sc.eye, d, a + 1.0));
        let _ = write!(
            s,
            r#"<path d="M{} {}L{} {}" stroke="{OL}" stroke-width="{}" stroke-linecap="butt"/><path d="M{} {}L{} {}" stroke="{GOLD_L}" stroke-width="{}" stroke-linecap="butt"/>"#,
            n(add(pa, d, -0.8).0),
            n(add(pa, d, -0.8).1),
            n(add(pb, d, 0.8).0),
            n(add(pb, d, 0.8).1),
            n(w + 4.4),
            n(pa.0),
            n(pa.1),
            n(pb.0),
            n(pb.1),
            n(w + 1.4)
        );
    };
    // Draw tubes from the eyepiece outward: eyepiece, middle, main (the
    // main tube is leather-wrapped, with brass rings).
    let main = l - (l * 0.42).max(14.0);
    seg(s, 0.0, (l * 0.3).max(2.5), 4.0, GOLD, GOLD_L);
    if l > 17.0 {
        seg(s, l * 0.27, main + 1.0, 5.6, GOLD, GOLD_L);
    }
    seg(s, main.max(1.5), l, 8.2, EMBER, TERRA);
    ring(s, main.max(1.5) + 1.0, 8.2);
    ring(s, l - 1.0, 8.2);
    // The lens.
    let c = add(sc.eye, d, l + 0.6);
    let _ = write!(
        s,
        r#"<ellipse cx="{}" cy="{}" rx="1.6" ry="5.2" transform="rotate({} {} {})" fill="{GLASS}" stroke="{OL}" stroke-width="1.2"/>"#,
        n(c.0),
        n(c.1),
        n(sc.ang),
        n(c.0),
        n(c.1)
    );
}

fn chart(s: &mut String, r: &Rig) {
    let (px, py, rot) = r.chart;
    let _ = write!(s, r#"<g transform="translate({} {}) rotate({})">"#, n(px), n(py), n(rot));
    let _ = write!(
        s,
        r#"<rect x="1.8" y="1.8" width="{CHART_W}" height="{CHART_H}" rx="3" fill="{EMBER}" stroke="{OL}" stroke-width="1.6"/><rect x="0" y="0" width="{CHART_W}" height="{CHART_H}" rx="3" fill="{GOLD}" stroke="{OL}" stroke-width="1.8"/><rect x="2.8" y="2.8" width="{}" height="{}" rx="1.6" fill="{CHART}" stroke="{OL}" stroke-width="1"/><path d="M5 {}H{}M{} 5V{}" stroke="{COAT_HI}" stroke-width=".6" opacity=".6"/><circle cx="{}" cy="{}" r="8.5" stroke="{COAT_HI}" stroke-width=".6" fill="none" opacity=".6"/>"#,
        CHART_W - 5.6,
        CHART_H - 5.6,
        n(CHART_H / 2.0),
        n(CHART_W - 5.0),
        n(CHART_W / 2.0),
        n(CHART_H - 5.0),
        n(CHART_W / 2.0),
        n(CHART_H / 2.0)
    );
    let line = |s: &mut String, a: P, b: P, op: f32| {
        let _ = write!(s, r#"<path d="M{} {}L{} {}" stroke="{CREAM}" stroke-width=".9" opacity="{op:.2}"/>"#, n(a.0), n(a.1), n(b.0), n(b.1));
    };
    let star = |s: &mut String, p: P, k: f32, op: f32| {
        let _ = write!(s, r#"<path d="{}" fill="{STAR}" opacity="{op:.2}"/>"#, sparkle_d(p, 3.2 * k));
    };
    match r.ink {
        Ink::Fixed => {
            for k in 1..4 {
                line(s, CHART_STARS[k - 1], CHART_STARS[k], 0.55);
            }
            for p in CHART_STARS {
                star(s, p, 1.0, 1.0);
            }
        }
        Ink::Empty => {}
        Ink::Plot(f) if f < 12 => {
            for k in 0..4 {
                let born = 3 * k + 1;
                if f < born {
                    continue;
                }
                if k > 0 {
                    line(s, CHART_STARS[k - 1], CHART_STARS[k], 0.55);
                }
                let pop = match f - born {
                    0 => 1.55,
                    1 => 1.15,
                    _ => 1.0,
                };
                star(s, CHART_STARS[k], pop, 1.0);
                if f == born {
                    // A tiny burst where the stylus touched.
                    let p = CHART_STARS[k];
                    let mut d = String::new();
                    for a in 0..4 {
                        let v = dir(45.0 + a as f32 * 90.0);
                        let _ = write!(d, "M{} {}L{} {}", n(p.0 + v.0 * 4.5), n(p.1 + v.1 * 4.5), n(p.0 + v.0 * 7.0), n(p.1 + v.1 * 7.0));
                    }
                    let _ = write!(s, r#"<path d="{d}" stroke="{STAR}" stroke-width="1.1"/>"#);
                }
            }
        }
        Ink::Plot(f) => {
            // The finished constellation lifts off the chart and fades.
            let k = (f - 11) as f32 / 4.0;
            let op = 1.0 - k;
            let up = |p: P| (p.0, p.1 - 15.0 * k);
            for i in 1..4 {
                line(s, up(CHART_STARS[i - 1]), up(CHART_STARS[i]), 0.55 * op * op);
            }
            for p in CHART_STARS {
                star(s, up(p), 1.0 + 0.3 * k, op);
            }
        }
    }
    s.push_str("</g>");
}

fn back_hair(s: &mut String) {
    let _ = write!(
        s,
        r#"<path d="M100 36C73 36 61 54 62 79C62 92 64 101 69 108L131 108C136 101 138 92 138 79C139 54 127 36 100 36Z" fill="{HAIR_SH}" stroke="{OL}" stroke-width="2.2"/>"#
    );
}

fn head(s: &mut String, r: &Rig) {
    // Low pigtails, tied with star bobbles, swinging.
    for m in [-1.0f32, 1.0] {
        let tie = (100.0 + m * 31.5, 104.0);
        let _ = write!(
            s,
            r#"<g transform="translate({} {}) scale({} 1) rotate({})"><path d="M-4 -1C-5 4 -5 10 -4 16Q-2 24 -5 32Q-1 30.5 1.5 26Q4 30 3 36Q7.5 33 9.5 27C13.5 17 12 5 4 -1Z" fill="url(#hhg)" stroke="{OL}" stroke-width="2"/><path d="M1 4Q4 14 3 24" stroke="{HAIR_SH}" stroke-width="1.2" fill="none"/></g>"#,
            n(tie.0),
            n(tie.1),
            m,
            n(-22.0 + m * r.swing)
        );
        let _ = write!(s, r#"<path d="{}" fill="{STAR}" stroke="{OL}" stroke-width="1.3"/>"#, star_d(tie, 5.0, 2.3, 5, m * 12.0));
    }
    // Face, a little rounder than Mori's.
    let _ =
        write!(s, r#"<path d="M68 77C68 45 132 45 132 77C132 99 116 112.5 100 113C84 112.5 68 99 68 77Z" fill="{SKIN}" stroke="{OL}" stroke-width="2.2"/>"#);
    if r.blush > 0.0 {
        let _ =
            write!(s, r#"<g fill="{BLUSH}" opacity="{:.2}"><ellipse cx="79" cy="102" rx="6" ry="3"/><ellipse cx="121" cy="102" rx="6" ry="3"/></g>"#, r.blush);
    }
    eye(s, r, 86.0, -1.0);
    eye(s, r, 114.0, 1.0);
    // Nose.
    let _ = write!(s, r#"<path d="M100.5 99l-.8 1.3" stroke="{SKIN_SH}" stroke-width="1.4"/>"#);
    mouth(s, r.mouth);
    // Front hair: crown, a soft rounded fringe, short side locks that lead
    // into the pigtails.
    let _ = write!(
        s,
        r#"<path d="M64 86C61 52 78 37 100 37C122 37 139 52 136 86C135 95 134 101 131 106C127 99 126 90 125 80Q123 72 120 66Q118 73 112.5 74.5Q111 68 106.5 63Q104 71 97.5 73Q96 67 92.5 63Q89 70 83 72.5Q81.5 68 80 64Q76.5 72 75 80C74 90 73 99 69 106C66 101 65 95 64 86Z" fill="url(#hhg)" stroke="{OL}" stroke-width="2.2"/>"#
    );
    let _ = write!(
        s,
        r#"<path d="M106.5 63Q104 52 99 43M92.5 63Q91 53 88 46M120 66Q119 56 114 48M80 64Q80 57 82 51" stroke="{HAIR_SH}" stroke-width="1.2" fill="none"/><path d="M75 55Q86 46 96 45M104 45Q115 46 125 54" stroke="{HAIR_HI}" stroke-width="3" fill="none"/>"#
    );
    // Brows, peeking through the fringe.
    let (li, lo, arch) = match r.brow {
        Brow::Soft => (76.5, 77.0, -2.0),
        Brow::Raised => (74.0, 75.0, -2.6),
        Brow::Worried => (72.5, 78.0, 0.6),
        Brow::Sleepy => (78.0, 78.0, -1.0),
    };
    for m in [-1.0f32, 1.0] {
        let inner = 100.0 + m * 8.5;
        let outer = 100.0 + m * 19.0;
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
    cap(s, r.cap);
    if let Extra::Spill(t) = r.extra {
        let dy = 3.0 * t;
        let _ = write!(
            s,
            r#"<g transform="translate(138 {})"><path d="M0 -6C3 -1.5 4.4 1.5 2.6 4C1 6 -1 6 -2.6 4C-4.4 1.5 -3 -1.5 0 -6Z" fill="{SWEAT}" stroke="{OL}" stroke-width="1.3"/><ellipse cx="-.9" cy="1.6" rx=".9" ry="1.5" fill="white"/></g>"#,
            n(72.0 + dy)
        );
    }
}

/// Her soft navy cap with a gold star pin; `down` (0..1) slides it over
/// her brow when she dozes off.
fn cap(s: &mut String, down: f32) {
    let _ = write!(
        s,
        r#"<g transform="translate({} {}) rotate({} 100 44)"><path d="M98 29Q99 23 104 22" stroke="{OL}" stroke-width="3.4" fill="none"/><path d="M98 29Q99 23 104 22" stroke="{CAP}" stroke-width="1.6" fill="none"/><path d="M64 51C56 41 72 28 99 27C127 26 143 37 136 48C133 54 120 53 100 54C82 55 68 57 64 51Z" fill="{CAP}" stroke="{OL}" stroke-width="2.2"/><path d="M72 36Q86 30 102 30" stroke="{COAT_HI}" stroke-width="2.6" fill="none"/><path d="M67 50Q100 57 133 47" stroke="{OL}" stroke-width="1.2" fill="none" opacity=".5"/><path d="{}" fill="{STAR}" stroke="{OL}" stroke-width="1.2"/></g>"#,
        n(2.0 * down),
        n(12.0 * down),
        n(-8.0 + 14.0 * down),
        star_d((122.0, 41.0), 5.0, 2.2, 5, 10.0)
    );
}

fn eye(s: &mut String, r: &Rig, cx: f32, m: f32) {
    let cy = 91.0;
    let closed = |s: &mut String| {
        let _ = write!(
            s,
            r#"<path d="M{} {}Q{} {} {} {}" stroke="{OL}" stroke-width="2.3" fill="none"/><path d="M{} {}l{} 1.6" stroke="{OL}" stroke-width="1.5"/>"#,
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
    };
    if r.wink && m < 0.0 {
        // Squeezed shut behind the eyepiece.
        let _ = write!(
            s,
            r#"<path d="M{} {}L{} {}L{} {}" stroke="{OL}" stroke-width="2.4" fill="none"/>"#,
            n(cx - 6.5),
            n(cy - 3.5),
            n(cx + 5.5),
            n(cy + 1.0),
            n(cx - 6.0),
            n(cy + 4.5)
        );
        return;
    }
    match r.eyes {
        Eyes::Closed => closed(s),
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
        Eyes::Squeeze => {
            let _ = write!(
                s,
                r#"<path d="M{} {}L{} {}L{} {}" stroke="{OL}" stroke-width="2.4" fill="none"/>"#,
                n(cx - m * 6.5),
                n(cy - 4.0),
                n(cx + m * 5.5),
                n(cy + 0.5),
                n(cx - m * 6.0),
                n(cy + 4.5)
            );
        }
        Eyes::Open | Eyes::Wide => {
            let wide = r.eyes == Eyes::Wide;
            let (ry, irx, iry) = if wide { (10.6, 5.6, 7.4) } else { (10.0, 6.2, 8.2) };
            let ix = cx + r.look.0;
            let iy = cy + 1.0 + r.look.1;
            let id = if m < 0.0 { "hel" } else { "her" };
            let _ = write!(
                s,
                r#"<clipPath id="{id}"><ellipse cx="{}" cy="{}" rx="7.8" ry="{ry}"/></clipPath><ellipse cx="{}" cy="{}" rx="7.8" ry="{ry}" fill="white"/><g clip-path="url(#{id})"><ellipse cx="{}" cy="{}" rx="{irx}" ry="{iry}" fill="url(#hir)" stroke="{OL}" stroke-width=".9"/><ellipse cx="{}" cy="{}" rx="2.7" ry="3.9" fill="{OL}"/><path d="{}" fill="white"/><circle cx="{}" cy="{}" r="1.1" fill="white" opacity=".9"/>"#,
                n(cx),
                n(cy),
                n(cx),
                n(cy),
                n(ix),
                n(iy),
                n(ix),
                n(iy + 0.4),
                sparkle_d((ix - 2.2, iy - 3.0), 3.3),
                n(ix + 2.4),
                n(iy + 3.4)
            );
            let top = cy - ry + 0.6;
            let yl = top + r.lid * (2.0 * ry - 1.5);
            if r.lid > 0.0 {
                let _ = write!(s, r#"<rect x="{}" y="{}" width="19" height="{}" fill="{SKIN}"/>"#, n(cx - 9.5), n(cy - 13.0), n(yl - cy + 13.0 + 0.6));
            }
            s.push_str("</g>");
            if r.lid >= 0.95 {
                closed(s);
            } else {
                let _ = write!(
                    s,
                    r#"<path d="M{} {}Q{} {} {} {}L{} {}" stroke="{OL}" stroke-width="2.7" fill="none"/>"#,
                    n(cx - m * 7.6),
                    n(yl + 2.4),
                    n(cx - m * 0.5),
                    n(yl - 3.2),
                    n(cx + m * 8.2),
                    n(yl + 0.8),
                    n(cx + m * 10.0),
                    n(yl - 1.2)
                );
                let _ = write!(
                    s,
                    r##"<path d="M{} {}Q{} {} {} {}" stroke="#3D4A8A" stroke-width="1.2" fill="none"/>"##,
                    n(cx - m * 3.5),
                    n(cy + ry - 0.2),
                    n(cx + m * 1.0),
                    n(cy + ry + 0.8),
                    n(cx + m * 5.4),
                    n(cy + ry - 1.4)
                );
            }
        }
    }
}

fn mouth(s: &mut String, m: Mouth) {
    match m {
        Mouth::Smile => {
            let _ = write!(s, r#"<path d="M96.5 105Q100 108.2 103.5 105" stroke="{OL}" stroke-width="1.6" fill="none"/>"#);
        }
        Mouth::Open(h) => {
            let b = 105.0 + h + 1.5;
            let _ = write!(
                s,
                r#"<path d="M96 104.5Q100 105.6 104 104.5Q103.6 {} 100 {}Q96.4 {} 96 104.5Z" fill="{MOUTH}" stroke="{OL}" stroke-width="1.4"/><ellipse cx="100" cy="{}" rx="2.2" ry="{}" fill="{TONGUE}"/>"#,
                n(b),
                n(b + 0.4),
                n(b),
                n(b - 0.9),
                n((h * 0.35).clamp(0.4, 1.3))
            );
        }
        Mouth::O(r) => {
            let _ = write!(s, r#"<ellipse cx="100" cy="106" rx="{}" ry="{}" fill="{MOUTH}" stroke="{OL}" stroke-width="1.3"/>"#, n(r * 0.85), n(r));
        }
        Mouth::Hmm => {
            let _ = write!(s, r#"<path d="M97 106Q99 104.8 103 106.2" stroke="{OL}" stroke-width="1.6" fill="none"/>"#);
        }
        Mouth::Worried => {
            let _ = write!(s, r#"<path d="M95.5 107q1.5-1.8 3 0t3 0 3 0" stroke="{OL}" stroke-width="1.5" fill="none"/>"#);
        }
    }
}

/// A gold sparkle with a dark rim, readable on light and dark backgrounds.
fn rim_sparkle(s: &mut String, c: P, r: f32, op: f32) {
    if r < 0.3 || op < 0.02 {
        return;
    }
    let _ = write!(s, r#"<path d="{}" fill="{STAR}" stroke="{OL}" stroke-width="1.2" opacity="{op:.2}"/>"#, sparkle_d(c, r));
}

fn extras(s: &mut String, r: &Rig, scope: Option<Scope>) {
    match r.extra {
        Extra::None => {}
        Extra::Glints(t) => {
            // Little stars catching her eye beyond the lens.
            if let Some(sc) = scope {
                let d = dir(sc.ang);
                let tip = add(sc.eye, d, sc.len + 6.0);
                for k in 0..3 {
                    let u = (t * 2.0 + k as f32 / 3.0) % 1.0;
                    let side = [-1.0, 1.0, 0.0][k];
                    let p = add(add(tip, d, 4.0 + 10.0 * u), (-d.1, d.0), side * (4.0 + 6.0 * u));
                    rim_sparkle(s, p, 2.0 + 2.8 * (PI * u).sin(), (PI * u).sin());
                }
            }
        }
        Extra::Twinkle(t) => {
            // Two small sparkles rising from her head to a big star.
            let lift = 1.2 * wave(t, 1.0, 0.0);
            let _ = write!(s, r#"<g transform="translate(0 {})">"#, n(lift));
            rim_sparkle(s, (136.0, 64.0), 2.6, 1.0);
            rim_sparkle(s, (145.0, 52.0), 3.6, 1.0);
            let c = (162.0, 34.0);
            let k = 1.0 + 0.16 * wave(t, 2.0, 0.0);
            let _ = write!(
                s,
                r#"<circle cx="{}" cy="{}" r="{}" fill="{STAR}" opacity=".25"/><g transform="rotate({} {} {})"><path d="{}" fill="{STAR}" stroke="{OL}" stroke-width="1.8"/></g>"#,
                c.0,
                c.1,
                n(13.0 * k),
                n(8.0 * wave(t, 1.0, 0.0)),
                c.0,
                c.1,
                star_d(c, 10.0 * k, 4.6 * k, 5, 0.0)
            );
            // Three motes circling the star, one after another.
            for i in 0..3 {
                let a = TAU * (t + i as f32 / 3.0);
                let p = (c.0 + 17.0 * a.cos(), c.1 + 7.0 * a.sin());
                let front = a.sin() > 0.0;
                let _ = write!(
                    s,
                    r#"<circle cx="{}" cy="{}" r="{}" fill="{}" stroke="{OL}" stroke-width="1"/>"#,
                    n(p.0),
                    n(p.1),
                    if front { 2.4 } else { 1.7 },
                    [GOLD, GOLD_L, EMBER][i]
                );
            }
            s.push_str("</g>");
        }
        Extra::Point(t) => {
            if let Some(sc) = scope {
                let tip = add(sc.eye, dir(sc.ang), sc.len + 5.0);
                let k = (TAU * 2.0 * t).sin().abs();
                rim_sparkle(s, tip, 2.4 + 2.2 * k, 1.0);
                rim_sparkle(s, (tip.0 - 9.0, tip.1 + 6.0), 1.8 * (1.0 - k) + 0.6, 0.8);
            }
        }
        Extra::Moon(t) => {
            // A tiny crescent moon rocking over her, z's drifting up.
            let rock = 6.0 * wave(t, 1.0, 0.0);
            let _ = write!(
                s,
                r#"<g transform="rotate({} 160 64) translate(0 {})"><path d="M166 52A13 13 0 1 0 168 76A10 10 0 1 1 166 52Z" fill="{STAR}" stroke="{OL}" stroke-width="1.8"/><circle cx="153" cy="66" r="1.2" fill="{OL}"/><path d="M155 71q2 1.5 4 .5" stroke="{OL}" stroke-width="1.1" fill="none"/></g>"#,
                n(rock),
                n(1.5 * wave(t, 1.0, 0.25))
            );
            for k in 0..3 {
                let u = (t + k as f32 / 3.0) % 1.0;
                let x = 150.0 + u * 16.0 + 3.0 * (TAU * u).sin();
                let y = 140.0 - u * 46.0;
                let z = 4.0 + u * 4.0;
                let op = (PI * u).sin().powf(0.6);
                let d = format!("M{} {}h{}l-{} {}h{}", n(x - z / 2.0), n(y - z / 2.0), n(z), n(z), n(z), n(z));
                let _ = write!(
                    s,
                    r#"<g opacity="{op:.2}"><path d="{d}" stroke="{OL}" stroke-width="{}" fill="none"/><path d="{d}" stroke="{GOLD_L}" stroke-width="{}" fill="none"/></g>"#,
                    n(2.0 + z * 0.35),
                    n(z * 0.28)
                );
            }
        }
        Extra::Spill(t) => {
            // Stars sliding off the tipped chart's low corner and falling.
            let lip = chart_point(r, (CHART_W - 2.0, CHART_H - 2.0));
            for k in 0..4 {
                let u = (t + k as f32 / 4.0) % 1.0;
                let drift = [3.0, -4.0, 6.0, -1.0][k];
                let p = (lip.0 + drift * u + 4.0 * u, lip.1 + 6.0 + 70.0 * u * u);
                let op = (PI * u).sin().powf(0.5);
                let _ = write!(s, r#"<g transform="rotate({} {} {})">"#, n(360.0 * u * if k % 2 == 0 { 1.0 } else { -1.0 }), n(p.0), n(p.1));
                rim_sparkle(s, p, 4.4, op);
                s.push_str("</g>");
            }
            // A falling star streaking past.
            let u = t;
            let a = (24.0, 22.0);
            let b = (64.0, 72.0);
            let head = lerp(a, b, u);
            let tail = lerp(a, b, (u - 0.35).max(0.0));
            let op = (PI * u).sin();
            let _ = write!(
                s,
                r#"<path d="M{} {}L{} {}" stroke="{GOLD_L}" stroke-width="2.4" opacity="{:.2}"/>"#,
                n(tail.0),
                n(tail.1),
                n(head.0),
                n(head.1),
                op * 0.8
            );
            let _ = write!(s, r#"<path d="{}" fill="{STAR}" stroke="{OL}" stroke-width="1.3" opacity="{op:.2}"/>"#, star_d(head, 5.0, 2.3, 5, 200.0 * u));
            // Little stress marks at her temple.
            let m = head_point(r, (60.0, 66.0));
            let _ = write!(
                s,
                r#"<path d="M{} {}l-6-4M{} {}h-7M{} {}l-6 3" stroke="{OL}" stroke-width="1.8"/>"#,
                n(m.0),
                n(m.1),
                n(m.0 - 2.0),
                n(m.1 + 8.0),
                n(m.0),
                n(m.1 + 16.0)
            );
        }
    }
}

// ---------------------------------------------------------------- the sky

/// Size of the night-sky panel `notes_svg` draws (its viewBox).
pub const NOTES_W: f32 = 240.0;
pub const NOTES_H: f32 = 160.0;
/// Frames in one twinkle loop of the sky (2 s at `FPS`).
pub const NOTES_FRAMES: usize = 24;
/// Notes joined into one constellation: one stretch of the meeting.
const GROUP: usize = 5;
/// Where stars may go: the sky above the hills, inside a margin.
const SKY: (f32, f32, f32, f32) = (14.0, 14.0, 226.0, 126.0);

/// Star colour per kind, close to the card colours of the dark theme
/// (but every kind also has its own shape: a five-pointed star for a
/// decision, a four-pointed sparkle for an action item, a ringed planet for
/// an open question, a round steady light for a fact or a key point, the
/// key point in its own blue).
fn kind_hex(kind: PointKind) -> &'static str {
    match kind {
        PointKind::Decision => "#FFD27A",
        PointKind::Action => "#F59B7B",
        PointKind::Question => "#D2B0E8",
        PointKind::Fact => "#8FD3D4",
        PointKind::Point => "#A9C4FF",
    }
}

/// One star of the sky: its kind's shape around `c`, `r` its radius, with
/// a soft halo of strength `glow` (0..1).
fn sky_star(s: &mut String, kind: PointKind, c: P, r: f32, glow: f32) {
    let col = kind_hex(kind);
    let _ = write!(s, r#"<circle cx="{}" cy="{}" r="{}" fill="{col}" opacity="{:.2}"/>"#, n(c.0), n(c.1), n(r * 1.75), 0.05 + 0.1 * glow);
    match kind {
        PointKind::Decision => {
            let _ = write!(s, r#"<path d="{}" fill="{col}" stroke="{col}" stroke-width="{}"/>"#, star_d(c, r * 1.1, r * 0.48, 5, 0.0), n(r * 0.18));
            let _ = write!(s, r#"<circle cx="{}" cy="{}" r="{}" fill="white" opacity=".8"/>"#, n(c.0), n(c.1 + r * 0.05), n(r * 0.25));
        }
        PointKind::Action => {
            let _ = write!(s, r#"<path d="{}" fill="{col}"/>"#, sparkle_d(c, r * 1.3));
            let _ = write!(s, r#"<circle cx="{}" cy="{}" r="{}" fill="white" opacity=".8"/>"#, n(c.0), n(c.1), n(r * 0.22));
        }
        PointKind::Question => {
            let (rx, ry) = (r * 1.25, r * 0.42);
            let tilt = -18.0;
            let _ = write!(
                s,
                r#"<ellipse cx="{}" cy="{}" rx="{}" ry="{}" transform="rotate({tilt} {} {})" fill="none" stroke="{col}" stroke-width="{}"/><circle cx="{}" cy="{}" r="{}" fill="{col}"/><path d="M{} {}A{} {} 0 0 0 {} {}" transform="rotate({tilt} {} {})" fill="none" stroke="{col}" stroke-width="{}"/><circle cx="{}" cy="{}" r="{}" fill="white" opacity=".55"/>"#,
                n(c.0),
                n(c.1),
                n(rx),
                n(ry),
                n(c.0),
                n(c.1),
                n(r * 0.2),
                n(c.0),
                n(c.1),
                n(r * 0.62),
                n(c.0 - rx),
                n(c.1),
                n(rx),
                n(ry),
                n(c.0 + rx),
                n(c.1),
                n(c.0),
                n(c.1),
                n(r * 0.2),
                n(c.0 - r * 0.2),
                n(c.1 - r * 0.22),
                n(r * 0.17)
            );
        }
        PointKind::Fact | PointKind::Point => {
            let g = r * 1.45;
            let _ = write!(
                s,
                r#"<path d="M{} {}H{}M{} {}V{}" stroke="{col}" stroke-width="{}" opacity=".85"/><circle cx="{}" cy="{}" r="{}" fill="{col}"/><circle cx="{}" cy="{}" r="{}" fill="white" opacity=".75"/>"#,
                n(c.0 - g),
                n(c.1),
                n(c.0 + g),
                n(c.0),
                n(c.1 - g),
                n(c.1 + g),
                n(r * 0.16),
                n(c.0),
                n(c.1),
                n(r * 0.55),
                n(c.0),
                n(c.1),
                n(r * 0.24)
            );
        }
    }
}

/// Constellation shapes in a unit square, five stars each.
const SHAPES: [[P; GROUP]; 4] = [
    [(0.0, 0.3), (0.26, 0.9), (0.5, 0.42), (0.76, 0.95), (1.0, 0.18)],
    [(0.0, 0.15), (0.32, 0.38), (0.62, 0.12), (0.88, 0.5), (0.52, 0.95)],
    [(0.45, 0.0), (0.95, 0.38), (0.6, 0.95), (0.05, 0.62), (0.4, 0.45)],
    [(0.0, 0.85), (0.18, 0.3), (0.5, 0.02), (0.82, 0.3), (1.0, 0.85)],
];

/// How the sky is split into one region per constellation: columns, rows.
fn grid(groups: usize) -> (usize, usize) {
    match groups {
        0 | 1 => (1, 1),
        2 => (2, 1),
        3 => (3, 1),
        4 => (2, 2),
        5 | 6 => (3, 2),
        7 | 8 => (4, 2),
        9 => (3, 3),
        g => {
            let cols = ((g as f32 * 1.6).sqrt().ceil() as usize).max(1);
            (cols, g.div_ceil(cols))
        }
    }
}

/// Where every note's star goes, and its radius. Deterministic: the same
/// notes always make the same sky, and adding a note never moves the
/// stars of earlier, finished constellations within their region.
fn layout(count: usize) -> (Vec<P>, f32) {
    let groups = count.div_ceil(GROUP);
    let (cols, rows) = grid(groups);
    let (x0, y0, x1, y1) = SKY;
    let (rw, rh) = ((x1 - x0) / cols as f32, (y1 - y0) / rows as f32);
    let radius = 7.5 * (rw.min(rh) / 75.0).clamp(0.5, 1.0);
    let mut out = Vec::with_capacity(count);
    for g in 0..groups {
        let (col, row) = (g % cols, g / cols);
        // Center a short last row.
        let in_row = if row + 1 == rows { groups - row * cols } else { cols };
        let shift = (cols - in_row) as f32 * rw / 2.0;
        let (gx, gy) = (x0 + shift + col as f32 * rw, y0 + row as f32 * rh);
        let size = (count - g * GROUP).min(GROUP);
        let seed = g as u32;
        let shape = SHAPES[(hash(seed, 1) * 4.0) as usize % 4];
        let (fx, fy) = (hash(seed, 2) < 0.5, hash(seed, 3) < 0.35);
        let mut pts: Vec<P> = shape[..size]
            .iter()
            .enumerate()
            .map(|(i, &(u, v))| {
                let u = if fx { 1.0 - u } else { u };
                let v = if fy { 1.0 - v } else { v };
                let j = (i as u32) * 7 + 11;
                (u + 0.07 * (hash(seed, j) - 0.5), v + 0.07 * (hash(seed, j + 1) - 0.5))
            })
            .collect();
        // Fit the shape into the region (keeping its proportions sane) and
        // center it, so a lone star sits in the middle.
        let pad = radius * 1.4;
        let (bw, bh) = (rw - 2.0 * pad, rh - 2.0 * pad);
        let (mut lo, mut hi) = ((f32::MAX, f32::MAX), (f32::MIN, f32::MIN));
        for p in &pts {
            lo = (lo.0.min(p.0), lo.1.min(p.1));
            hi = (hi.0.max(p.0), hi.1.max(p.1));
        }
        let mid = ((lo.0 + hi.0) / 2.0, (lo.1 + hi.1) / 2.0);
        for p in &mut pts {
            *p = (gx + rw / 2.0 + (p.0 - mid.0) * bw, gy + rh / 2.0 + (p.1 - mid.1) * bh);
        }
        out.extend(pts);
    }
    (out, radius)
}

/// Hoshi's night sky: one star per note, in order, every `GROUP` notes
/// joined into a constellation by faint lines. The stars twinkle over a
/// loop of `NOTES_FRAMES` frames; the newest one pulses with a little burst
/// every loop. A standalone SVG document of `NOTES_W` × `NOTES_H`.
pub fn notes_svg(notes: &[PointKind], frame: usize) -> String {
    let t = (frame % NOTES_FRAMES) as f32 / NOTES_FRAMES as f32;
    let (w, h) = (NOTES_W, NOTES_H);
    let mut s = String::with_capacity(4096 + notes.len() * 700);
    let _ = write!(
        s,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}"><defs><linearGradient id="hsky" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#141A40"/><stop offset=".7" stop-color="#2A2F66"/><stop offset="1" stop-color="#4A3A72"/></linearGradient><clipPath id="hpanel"><rect width="{w}" height="{h}" rx="14"/></clipPath></defs><g clip-path="url(#hpanel)" stroke-linecap="round" stroke-linejoin="round"><rect width="{w}" height="{h}" fill="url(#hsky)"/>"##
    );
    // A faint band of the milky way and the dust of far stars.
    s.push_str(r##"<path d="M-10 118C60 70 150 60 250 8L250 40C160 84 70 100 -10 146Z" fill="#8FA2FF" opacity=".06"/>"##);
    for i in 0..36u32 {
        let p = (4.0 + hash(i, 101) * (w - 8.0), 4.0 + hash(i, 102) * 128.0);
        let tw = 0.5 + 0.5 * wave(t, 1.0, hash(i, 103));
        let r = 0.5 + 0.7 * hash(i, 104);
        let _ = write!(s, r#"<circle cx="{}" cy="{}" r="{}" fill="{CREAM}" opacity="{:.2}"/>"#, n(p.0), n(p.1), n(r), 0.2 + 0.45 * tw);
    }
    if notes.is_empty() {
        // Nothing noted yet: only the moon is up.
        s.push_str(r##"<path d="M128 46A22 22 0 1 0 132 88A17 17 0 1 1 128 46Z" fill="#FFE29A" opacity=".9"/><circle cx="120" cy="66" r="34" fill="#FFE29A" opacity=".06"/>"##);
    }
    let (pts, radius) = layout(notes.len());
    // Constellation lines, stopping short of the stars.
    for g in 0..notes.len().div_ceil(GROUP) {
        let end = ((g + 1) * GROUP).min(notes.len());
        for i in g * GROUP + 1..end {
            let (a, b) = (pts[i - 1], pts[i]);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let len = (dx * dx + dy * dy).sqrt();
            let gap = radius * 1.6;
            if len <= 2.0 * gap {
                continue;
            }
            let d = (dx / len, dy / len);
            let (p, q) = (add(a, d, gap), add(b, d, -gap));
            let _ = write!(s, r##"<path d="M{} {}L{} {}" stroke="#DCE3FF" stroke-width="1" opacity=".38"/>"##, n(p.0), n(p.1), n(q.0), n(q.1));
        }
    }
    let newest = notes.len().saturating_sub(1);
    for (i, (&kind, &c)) in notes.iter().zip(&pts).enumerate() {
        let ph = hash(i as u32, 7);
        let tw = wave(t, if ph < 0.5 { 1.0 } else { 2.0 }, ph);
        let mut r = radius * (1.0 + 0.07 * tw);
        if i == newest {
            // Born again every loop: a quick pop and a ring of light.
            let f = frame % NOTES_FRAMES;
            r *= match f {
                0 => 0.55,
                1 => 1.25,
                2 => 1.12,
                _ => 1.0 + 0.06 * tw,
            };
            let u = (f as f32 / 10.0).min(1.0);
            if u < 1.0 {
                let op = (1.0 - u) * 0.8;
                let col = kind_hex(kind);
                let _ = write!(
                    s,
                    r#"<circle cx="{}" cy="{}" r="{}" fill="none" stroke="{col}" stroke-width="{}" opacity="{op:.2}"/>"#,
                    n(c.0),
                    n(c.1),
                    n(radius * (1.3 + 2.0 * u)),
                    n(1.6 * (1.0 - u) + 0.4)
                );
                let mut d = String::new();
                for k in 0..8 {
                    let v = dir(22.5 + k as f32 * 45.0);
                    let (a, b) = (radius * (1.6 + 2.2 * u), radius * (2.2 + 2.6 * u));
                    let _ = write!(d, "M{} {}L{} {}", n(c.0 + v.0 * a), n(c.1 + v.1 * a), n(c.0 + v.0 * b), n(c.1 + v.1 * b));
                }
                let _ = write!(s, r#"<path d="{d}" stroke="{col}" stroke-width="1.2" opacity="{op:.2}"/>"#);
            }
        }
        sky_star(&mut s, kind, c, r, 0.5 + 0.5 * tw);
    }
    // Hills against the sky, and her observatory's lit window.
    s.push_str(r##"<path d="M-5 142Q30 130 70 138T150 135Q190 130 245 137V165H-5Z" fill="#12163A"/><path d="M192 135V131A7 7 0 0 1 206 131V135Z" fill="#12163A"/><circle cx="196" cy="133" r="1.1" fill="#F7C26B"/>"##);
    s.push_str("</g></svg>");
    s
}

/// Frames of `note_pop_svg`, played once (not looped).
pub const POP_FRAMES: usize = 12;
/// Size of the `note_pop_svg` square (its viewBox).
pub const POP_SIZE: f32 = 48.0;

/// A single new star leaving her telescope: it rises from the bottom of a
/// `POP_SIZE` square, swells with a burst of rays and settles.
pub fn note_pop_svg(kind: PointKind, frame: usize) -> String {
    let f = frame.min(POP_FRAMES - 1);
    let u = f as f32 / (POP_FRAMES - 1) as f32;
    let ease = 1.0 - (1.0 - u) * (1.0 - u);
    let c = (24.0, 40.0 - 18.0 * ease);
    let grow = match f {
        0 => 0.25,
        1 => 0.55,
        2 => 0.9,
        3 => 1.25,
        4 => 1.12,
        _ => 1.0,
    };
    let mut s = String::with_capacity(2048);
    let _ = write!(
        s,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {w}" width="{w}" height="{w}"><g stroke-linecap="round" stroke-linejoin="round">"#,
        w = POP_SIZE
    );
    let col = kind_hex(kind);
    // A trail of sparks behind it.
    for k in 0..3 {
        let p = (c.0 + [-3.0, 2.5, -1.0][k], c.1 + 7.0 + 5.0 * k as f32);
        let op = (1.0 - u) * (1.0 - k as f32 * 0.3);
        let _ = write!(s, r#"<path d="{}" fill="{col}" opacity="{op:.2}"/>"#, sparkle_d(p, 2.2 - 0.5 * k as f32));
    }
    if (3..10).contains(&f) {
        let k = (f - 3) as f32 / 7.0;
        let mut d = String::new();
        for i in 0..8 {
            let v = dir(i as f32 * 45.0);
            let (a, b) = (12.0 + 5.0 * k, 15.0 + 6.0 * k);
            let _ = write!(d, "M{} {}L{} {}", n(c.0 + v.0 * a), n(c.1 + v.1 * a), n(c.0 + v.0 * b), n(c.1 + v.1 * b));
        }
        let _ = write!(s, r#"<path d="{d}" stroke="{col}" stroke-width="1.6" opacity="{:.2}"/>"#, 1.0 - k);
    }
    sky_star(&mut s, kind, c, 7.0 * grow, 1.0);
    s.push_str("</g></svg>");
    s
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
                assert!(!svg.contains("<text") && !svg.contains("filter"));
                assert!(svg.len() < 30_000, "{pose:?} {f}: {} bytes", svg.len());
            }
        }
    }

    #[test]
    fn poses_loop() {
        for pose in Pose::ALL {
            let count = frame_count(pose);
            assert_eq!(frame_svg(pose, 0), frame_svg(pose, count));
            assert_ne!(frame_svg(pose, 0), frame_svg(pose, count / 2), "{pose:?} doesn't move");
        }
    }

    fn mixed(count: usize) -> Vec<PointKind> {
        let kinds = [PointKind::Decision, PointKind::Action, PointKind::Question, PointKind::Fact];
        (0..count).map(|i| kinds[(i * 7 + i / 3) % 4]).collect()
    }

    #[test]
    fn the_sky_draws_any_number_of_notes() {
        for count in [0, 1, 2, 5, 6, 20, 47, 120] {
            let notes = mixed(count);
            for f in 0..NOTES_FRAMES {
                let svg = notes_svg(&notes, f);
                assert!(svg.starts_with("<svg") && svg.ends_with("</svg>"));
                assert!(!svg.contains("<text") && !svg.contains("NaN"));
            }
            assert_eq!(notes_svg(&notes, 0), notes_svg(&notes, NOTES_FRAMES));
        }
    }

    #[test]
    fn stars_stay_in_the_sky_and_apart() {
        for count in [1, 5, 20, 40] {
            let (pts, r) = layout(count);
            assert_eq!(pts.len(), count);
            for (i, a) in pts.iter().enumerate() {
                assert!(a.0 > 0.0 && a.0 < NOTES_W && a.1 > 0.0 && a.1 < SKY.3 + r, "{count}: star {i} at {a:?}");
                for b in &pts[..i] {
                    let d = ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
                    assert!(d > 2.2 * r, "{count}: stars {a:?} and {b:?} overlap");
                }
            }
        }
        let (one, _) = layout(1);
        assert!((one[0].0 - NOTES_W / 2.0).abs() < 1.0);
    }

    #[test]
    fn a_star_pops_for_every_kind() {
        for kind in [PointKind::Decision, PointKind::Action, PointKind::Question, PointKind::Fact] {
            for f in 0..POP_FRAMES {
                assert!(note_pop_svg(kind, f).starts_with("<svg"));
            }
        }
    }
}
