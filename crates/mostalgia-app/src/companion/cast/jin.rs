//! Jin's artwork: the ronin scribe, a stoic young swordsman in a worn haori
//! with a straw kasa on his back. His katana ends in a calligraphy brush:
//! he slices what is said out of the air, the ink trail of each stroke
//! turns into a paper strip, and the strips end up on his sword rack. Every
//! animation frame is generated as an SVG document (transparent background,
//! viewBox FRAME_W × FRAME_H), in the same way as Mori's (`companion::art`).
//!
//! He is drawn as a small rig: the body parts are fixed shapes, and each
//! pose only decides where the joints go for a phase `t` in `0..1` (head
//! tilt, eye state, where the scabbard and the brush-katana are; the arms
//! follow by two-bone IK). All motion is built from `sin`/`cos` of `2πt`
//! (the sword strokes from keyframes that close on themselves), so every
//! pose loops seamlessly. No `<text>` and no filters: frames must render the
//! same in resvg and GPUI without fonts.
//!
//! His way of showing the meeting's notes is a sword rack (`notes_svg`):
//! every note is a paper strip hung from the rack's bars, its colour and its
//! brush mark telling the kind (an ensō for a decision, a slash for an
//! action, a hook for a question, three bars for a fact), the newest one
//! still fluttering on its string.

use std::f32::consts::{PI, TAU};
use std::fmt::{self, Write as _};

use super::PointKind;

/// What Jin is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pose {
    /// Standing calmly, one hand resting on the sheathed hilt, the other on
    /// the scabbard; breathing, blinking, the haori's hem stirring.
    Idle,
    /// Someone started speaking: turns his head, opens one eye, and his
    /// thumb nudges the guard so a sliver of blade shows.
    Listening,
    /// Speech going on: the brush-katana is out and slashes; each stroke
    /// leaves an ink arc in the air that turns into a paper strip.
    Writing,
    /// Waiting for the language model: eyes closed in meditation, the blade
    /// held upright, ink gathering on the brush, a leaf drifting by.
    Thinking,
    /// Showing an answer or a point: holds up a paper strip, hand on hilt.
    Talking,
    /// Recording paused: sitting against his sheathed sword, the kasa
    /// tipped over his face.
    Sleeping,
    /// The provider failed: the brush splatters ink everywhere, an ink blot
    /// on his cheek, a sweat drop.
    Error,
}

impl Pose {
    pub const ALL: [Pose; 7] = [
        Pose::Idle,
        Pose::Listening,
        Pose::Writing,
        Pose::Thinking,
        Pose::Talking,
        Pose::Sleeping,
        Pose::Error,
    ];
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
const HAIR: &str = "#2C2A40"; // Sumi ink
const HAIR_TOP: &str = "#45425F";
const HAIR_HI: &str = "#6E6A94";
const HAIR_SH: &str = "#1D1B2C";
const HAORI: &str = "#3A4166"; // Indigo
const HAORI_SH: &str = "#2B3150";
const HAORI_HI: &str = "#55608E";
const KIMONO: &str = "#D9D0BD"; // Unbleached linen
const KIMONO_SH: &str = "#BDB29A";
const HAKAMA: &str = "#6C6A86";
const HAKAMA_SH: &str = "#55536E";
const HAKAMA_HI: &str = "#8988A6";
const CREAM: &str = "#FBF3E4";
const STRAW: &str = "#E2BE79";
const STRAW_SH: &str = "#C49A55";
const STRAW_HI: &str = "#F2D9A2";
const EMBER: &str = "#B8472A";
const TERRA: &str = "#D9653B";
const GOLD: &str = "#F2A541";
const GOLD_L: &str = "#F7C26B";
const SAYA: &str = "#4A2340";
const SAYA_HI: &str = "#7E4F72";
const TSUKA: &str = "#2E2A42";
const STEEL: &str = "#E6EDF3";
const STEEL_SH: &str = "#A9B5C7";
const INK: &str = "#211B2D";
const INK_HI: &str = "#6A6188";
const TABI: &str = "#FBF7EE";
const MOUTH: &str = "#8A3444";
const TONGUE: &str = "#E9877A";
const BLUSH: &str = "#F2876F";
const BROW: &str = "#1D1B2C";
const SWEAT: &str = "#BFE0E4";

// ---------------------------------------------------------------- numbers

/// A coordinate printed with one decimal at most ("12", "12.5", "-3.2").
#[derive(Clone, Copy)]
struct N(f32);

impl fmt::Display for N {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let r = (self.0 * 10.0).round() / 10.0;
        let r = if r == 0.0 { 0.0 } else { r };
        if r.fract() == 0.0 {
            write!(f, "{r:.0}")
        } else {
            write!(f, "{r:.1}")
        }
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

fn smooth(u: f32) -> f32 {
    let u = u.clamp(0.0, 1.0);
    u * u * (3.0 - 2.0 * u)
}

/// Two-bone IK: elbow and (reachable) hand for a shoulder `s` reaching to
/// `target`. `bend` (+1 / −1) picks the side the elbow points to.
fn ik(s: P, target: P, l1: f32, l2: f32, bend: f32) -> (P, P) {
    let (dx, dy) = (target.0 - s.0, target.1 - s.1);
    let dist = (dx * dx + dy * dy).sqrt().max(0.001);
    let d = dist.clamp((l1 - l2).abs() + 0.5, l1 + l2 - 0.3);
    let base = dy.atan2(dx);
    let a = ((l1 * l1 + d * d - l2 * l2) / (2.0 * l1 * d))
        .clamp(-1.0, 1.0)
        .acos();
    let ang = base + bend * a;
    let elbow = (s.0 + l1 * ang.cos(), s.1 + l1 * ang.sin());
    let hand = (s.0 + dx / dist * d, s.1 + dy / dist * d);
    (elbow, hand)
}

/// A small deterministic hash of two numbers, in `0..1` (stands in for a
/// random generator: the rack must look the same every time it's drawn).
fn hash(a: u32, b: u32) -> f32 {
    let mut x = a.wrapping_mul(0x9E37_79B1) ^ b.wrapping_mul(0x85EB_CA77) ^ 0x2545_F491;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    x = x.wrapping_mul(0x297A_2D39);
    x ^= x >> 15;
    (x & 0xFF_FFFF) as f32 / 16_777_216.0
}

/// Path data, built point by point.
struct D(String);

impl D {
    fn new() -> D {
        D(String::with_capacity(128))
    }
    fn m(mut self, p: P) -> D {
        let _ = write!(self.0, "M{} {}", n(p.0), n(p.1));
        self
    }
    fn l(mut self, p: P) -> D {
        let _ = write!(self.0, "L{} {}", n(p.0), n(p.1));
        self
    }
    fn q(mut self, c: P, p: P) -> D {
        let _ = write!(self.0, "Q{} {} {} {}", n(c.0), n(c.1), n(p.0), n(p.1));
        self
    }
    fn c(mut self, c1: P, c2: P, p: P) -> D {
        let _ = write!(
            self.0,
            "C{} {} {} {} {} {}",
            n(c1.0),
            n(c1.1),
            n(c2.0),
            n(c2.1),
            n(p.0),
            n(p.1)
        );
        self
    }
    fn z(mut self) -> String {
        self.0.push('Z');
        self.0
    }
    fn s(self) -> String {
        self.0
    }
}

/// A tapered brush stroke along `pts`: `width(u)` gives its full width at
/// `u` in `0..1` along the stroke. Drawn as one filled shape.
fn brush(s: &mut String, pts: &[P], width: impl Fn(f32) -> f32, fill: &str, op: f32) {
    if pts.len() < 2 || op < 0.02 {
        return;
    }
    let last = pts.len() - 1;
    let mut left = Vec::with_capacity(pts.len());
    let mut right = Vec::with_capacity(pts.len());
    for (i, &p) in pts.iter().enumerate() {
        let (a, b) = (pts[i.saturating_sub(1)], pts[(i + 1).min(last)]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = (dx * dx + dy * dy).sqrt().max(0.001);
        let nrm = (-dy / len, dx / len);
        let w = width(i as f32 / last as f32) / 2.0;
        left.push(add(p, nrm, w));
        right.push(add(p, nrm, -w));
    }
    let mut d = D::new().m(left[0]);
    for &p in &left[1..] {
        d = d.l(p);
    }
    for &p in right.iter().rev() {
        d = d.l(p);
    }
    let _ = write!(
        s,
        r#"<path d="{}" fill="{fill}" opacity="{op:.2}"/>"#,
        d.z()
    );
}

/// Points on a circle arc (degrees), for brush strokes.
fn arc_pts(c: P, rx: f32, ry: f32, from: f32, to: f32, steps: usize) -> Vec<P> {
    (0..=steps)
        .map(|i| {
            let a = (from + (to - from) * i as f32 / steps as f32).to_radians();
            (c.0 + rx * a.cos(), c.1 + ry * a.sin())
        })
        .collect()
}

/// An ink drop (a teardrop pointing up) at `c`.
fn drop_d(c: P, r: f32) -> String {
    D::new()
        .m((c.0, c.1 - r * 1.9))
        .c(
            (c.0 + r * 0.5, c.1 - r * 0.9),
            (c.0 + r, c.1 - r * 0.4),
            (c.0 + r, c.1 + r * 0.15),
        )
        .c(
            (c.0 + r, c.1 + r * 0.75),
            (c.0 + r * 0.5, c.1 + r),
            (c.0, c.1 + r),
        )
        .c(
            (c.0 - r * 0.5, c.1 + r),
            (c.0 - r, c.1 + r * 0.75),
            (c.0 - r, c.1 + r * 0.15),
        )
        .c(
            (c.0 - r, c.1 - r * 0.4),
            (c.0 - r * 0.5, c.1 - r * 0.9),
            (c.0, c.1 - r * 1.9),
        )
        .z()
}

/// An ink splat: a blob with a few round lobes, at `c`.
fn splat(s: &mut String, c: P, r: f32, seed: u32, op: f32) {
    if op < 0.02 || r < 0.2 {
        return;
    }
    let _ = write!(
        s,
        r#"<g fill="{INK}" opacity="{op:.2}"><circle cx="{}" cy="{}" r="{}"/>"#,
        n(c.0),
        n(c.1),
        n(r)
    );
    for k in 0..5u32 {
        let a = 360.0 * (k as f32 / 5.0 + 0.15 * hash(seed, k));
        let v = dir(a);
        let rr = r * (0.35 + 0.3 * hash(seed, k + 9));
        let p = add(c, v, r * 0.95);
        let _ = write!(
            s,
            r#"<circle cx="{}" cy="{}" r="{}"/>"#,
            n(p.0),
            n(p.1),
            n(rr)
        );
        if k % 2 == 0 {
            let q = add(c, v, r * (1.7 + 0.4 * hash(seed, k + 3)));
            let _ = write!(
                s,
                r#"<circle cx="{}" cy="{}" r="{}"/>"#,
                n(q.0),
                n(q.1),
                n(rr * 0.5)
            );
        }
    }
    s.push_str("</g>");
}

// ---------------------------------------------------------------- the rig

#[derive(Clone, Copy, PartialEq)]
enum Eyes {
    Open,
    /// Calm, closed (meditating, asleep).
    Closed,
    /// Squeezed shut, flustered ("> <").
    Squeeze,
}

#[derive(Clone, Copy)]
enum Mouth {
    /// His usual: a small straight line.
    Flat,
    /// The corner of a smile.
    Smirk,
    Open(f32),
    O(f32),
    Worried,
}

#[derive(Clone, Copy)]
enum Brow {
    Calm,
    /// Focused: inner ends lowered.
    Stern,
    Raised,
    Worried,
}

/// Where the brush-katana is.
#[derive(Clone, Copy)]
enum Blade {
    /// In the scabbard; `peek` is how far the guard has been thumbed out.
    Sheathed { peek: f32 },
    /// Drawn: the grip's middle and the blade's direction (degrees).
    Drawn { hand: P, ang: f32 },
}

#[derive(Clone, Copy)]
enum Extra {
    None,
    /// Ripples of sound reaching his ear.
    Listen(f32),
    /// The slashes' ink trails and the strips they turn into (frame time).
    Strokes(f32),
    /// An ink drop gathering on the brush, a leaf drifting, an ensō being
    /// drawn in the air.
    Meditate(f32),
    /// A held paper strip's little gleam.
    Present(f32),
    /// Brush-drawn z's rising from under the hat.
    Zzz(f32),
    /// Ink splattering off the brush, a blot on the floor, a sweat drop.
    Splatter(f32),
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
    /// His right eye (viewer's left) closed while the other one is open.
    wink: bool,
    look: P,
    /// Upper lid, 0 = open, 1 = shut. He keeps it a little low: cool.
    lid: f32,
    mouth: Mouth,
    brow: Brow,
    blush: f32,
    /// An ink blot on his cheek (error).
    blot: bool,
    /// Ponytail swing (degrees) and the haori hem's stir (−1..1).
    swing: f32,
    hem: f32,
    /// The kasa: on his back, or tipped down over his face.
    hat_on: bool,
    /// The scabbard: its mouth and the direction to its tip (degrees).
    saya: (P, f32),
    blade: Blade,
    /// Where his hands go when they are not holding the sword: right hand
    /// (viewer's left) and left hand (viewer's right), and elbow sides.
    hand_r: P,
    hand_l: P,
    bend_r: f32,
    bend_l: f32,
    /// A paper strip held up in his left hand: tilt (degrees) and kind.
    strip: Option<(f32, PointKind)>,
    /// The brush tip frayed and dripping.
    frayed: bool,
    extra: Extra,
}

const SHOULDER_R: P = (81.0, 163.0);
const SHOULDER_L: P = (119.0, 163.0);
const UPPER_ARM: f32 = 23.0;
const FOREARM: f32 = 23.0;
/// The head is drawn at its native scale (chin at y 113), then scaled
/// around the chin onto the neck. Chibi: a big head on a small body.
const HEAD_SCALE: f32 = 1.04;
const NECK: P = (100.0, 150.0);
/// How far the upper body sinks when he sits down.
const SIT: f32 = 40.0;
/// Sword measures: scabbard, grip, steel and the brush past the ferrule.
const SAYA_LEN: f32 = 64.0;
const TSUKA_LEN: f32 = 21.0;
const BLADE_LEN: f32 = 42.0;
const BRUSH_LEN: f32 = 14.0;
/// The scabbard's mouth and angle where it sits in his obi.
const SAYA_HIP: (P, f32) = ((111.0, 204.0), 30.0);

fn base() -> Rig {
    let (k, a) = SAYA_HIP;
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
        lid: 0.24,
        mouth: Mouth::Flat,
        brow: Brow::Calm,
        blush: 0.35,
        blot: false,
        swing: 0.0,
        hem: 0.0,
        hat_on: false,
        saya: SAYA_HIP,
        blade: Blade::Sheathed { peek: 0.0 },
        hand_r: add(k, dir(a), -TSUKA_LEN - 1.0),
        hand_l: add(k, dir(a), 9.0),
        bend_r: 1.0,
        bend_l: 1.0,
        strip: None,
        frayed: false,
        extra: Extra::None,
    }
}

/// A point of the head (native coordinates) in body coordinates.
fn head_point(r: &Rig, p: P) -> P {
    let q = (
        NECK.0 + (p.0 - 100.0) * HEAD_SCALE,
        NECK.1 + (p.1 - 113.0) * HEAD_SCALE,
    );
    let (c, s) = (r.head_rot.to_radians().cos(), r.head_rot.to_radians().sin());
    let (dx, dy) = (q.0 - NECK.0, q.1 - NECK.1);
    (
        NECK.0 + dx * c - dy * s + r.head_dx,
        NECK.1 + dx * s + dy * c + r.head_dy,
    )
}

/// The writing strokes as keyframes (frame time, grip, blade angle): a
/// wind-up, a diagonal cut, a low sweep and back to guard. Angles only
/// grow, so the blade keeps turning the same way round; the last key is
/// the first one, one turn later, so the loop closes.
const STROKE_KEYS: [(f32, P, f32); 7] = [
    (0.0, (64.0, 190.0), -100.0),
    (2.5, (72.0, 138.0), -128.0),
    (5.0, (106.0, 196.0), 25.0),
    (7.0, (108.0, 198.0), 34.0),
    (10.0, (72.0, 178.0), 208.0),
    (12.0, (70.0, 180.0), 214.0),
    (16.0, (64.0, 190.0), 260.0),
];
/// The two cuts that leave ink: (start, end) in frame time.
const CUTS: [(f32, f32); 2] = [(2.5, 5.0), (7.0, 10.0)];

fn stroke_at(tf: f32) -> (P, f32) {
    let tf = tf.rem_euclid(16.0);
    for w in STROKE_KEYS.windows(2) {
        let ((ta, pa, aa), (tb, pb, ab)) = (w[0], w[1]);
        if tf <= tb {
            let u = smooth((tf - ta) / (tb - ta));
            return (lerp(pa, pb, u), aa + (ab - aa) * u);
        }
    }
    (STROKE_KEYS[0].1, STROKE_KEYS[0].2)
}

/// The geometry of a drawn sword held at `hand`, pointing to `ang`.
struct Sword {
    kashira: P,
    tsuba: P,
    /// Base and end of the steel, the curve's control point.
    base: P,
    ctrl: P,
    end: P,
    /// Ferrule start, brush direction, brush tip.
    ferrule: P,
    bdir: P,
    tip: P,
}

fn sword(hand: P, ang: f32) -> Sword {
    let d = dir(ang);
    let nrm = (-d.1, d.0);
    let kashira = add(hand, d, -TSUKA_LEN * 0.5);
    let tsuba = add(hand, d, TSUKA_LEN * 0.5);
    let base = add(tsuba, d, 1.5);
    let ctrl = add(base, d, BLADE_LEN * 0.5);
    // The katana's curve (sori): toward the back, growing to the tip.
    let end = add(add(base, d, BLADE_LEN), nrm, -3.0);
    let (tx, ty) = (end.0 - ctrl.0, end.1 - ctrl.1);
    let tl = (tx * tx + ty * ty).sqrt().max(0.001);
    let bdir = (tx / tl, ty / tl);
    let ferrule = end;
    let tip = add(ferrule, bdir, 3.5 + BRUSH_LEN);
    Sword {
        kashira,
        tsuba,
        base,
        ctrl,
        end,
        ferrule,
        bdir,
        tip,
    }
}

fn rig(pose: Pose, frame: usize, t: f32) -> Rig {
    let mut r = base();
    match pose {
        Pose::Idle => {
            r.sway = 0.6 * wave(t, 1.0, 0.0);
            r.bob = -0.9 * (0.5 - 0.5 * (TAU * 2.0 * t).cos());
            r.head_rot = 1.6 * wave(t, 1.0, 0.1);
            r.swing = -r.head_rot * 1.5 + 4.0 * wave(t, 2.0, 0.2);
            r.hem = wave(t, 1.0, 0.3);
            r.lid = match frame {
                14 => 0.6,
                15 => 1.0,
                16 => 0.45,
                _ => 0.24,
            };
            r.hand_r = add(r.hand_r, (0.0, 1.0), 0.6 * wave(t, 2.0, 0.0));
        }
        Pose::Listening => {
            let nod = wave(t, 2.0, 0.0);
            r.head_rot = -7.0 + 1.0 * wave(t, 1.0, 0.0);
            r.head_dx = -1.0;
            r.head_dy = -0.5 + 0.5 * nod;
            r.bob = -0.5 * (0.5 - 0.5 * (TAU * t).cos());
            r.wink = true;
            r.lid = 0.12;
            r.look = (-2.4, -0.6);
            r.brow = Brow::Raised;
            r.mouth = Mouth::Flat;
            r.swing = 5.0 + 3.0 * wave(t, 2.0, 0.1);
            r.hem = 0.5 * wave(t, 1.0, 0.0);
            // His thumb pushes the guard out a little, twice.
            let peek = 5.5 * (0.5 - 0.5 * (TAU * 2.0 * t).cos());
            r.blade = Blade::Sheathed { peek };
            let (k, a) = r.saya;
            r.hand_l = add(k, dir(a), 8.0);
            r.hand_r = (70.0, 216.0 + 0.8 * nod);
            r.bend_r = 1.0;
            r.extra = Extra::Listen(t);
        }
        Pose::Writing => {
            let tf = frame as f32;
            let (hand, ang) = stroke_at(tf);
            r.blade = Blade::Drawn { hand, ang };
            r.bend_r = 1.0;
            let cut = CUTS.iter().any(|&(a, b)| tf > a && tf <= b);
            r.head_rot = if cut { 3.0 } else { -1.0 } + 1.0 * wave(t, 1.0, 0.0);
            r.head_dy = 1.0;
            r.lid = 0.3;
            r.brow = Brow::Stern;
            r.mouth = if cut { Mouth::O(1.3) } else { Mouth::Flat };
            let s = sword(hand, ang);
            let to = (s.tip.0 - 100.0, s.tip.1 - 100.0);
            let tl = (to.0 * to.0 + to.1 * to.1).sqrt().max(1.0);
            r.look = (2.2 * to.0 / tl, 2.0 * to.1 / tl);
            r.swing = 6.0 * wave(t, 2.0, 0.1);
            r.hem = 1.2 * wave(t, 2.0, 0.25);
            r.bob = 0.8 * (0.5 - 0.5 * (TAU * 2.0 * t).cos());
            r.extra = Extra::Strokes(tf);
        }
        Pose::Thinking => {
            r.head_rot = 2.0 + 1.0 * wave(t, 1.0, 0.0);
            r.eyes = Eyes::Closed;
            r.brow = Brow::Calm;
            r.mouth = Mouth::Flat;
            r.bob = -0.8 * (0.5 - 0.5 * (TAU * t).cos());
            r.swing = 3.0 * wave(t, 1.0, 0.2);
            r.hem = 0.6 * wave(t, 1.0, 0.0);
            r.blade = Blade::Drawn {
                hand: (55.0, 199.0 + 0.6 * wave(t, 1.0, 0.0)),
                ang: -97.0,
            };
            r.bend_r = 1.0;
            r.extra = Extra::Meditate(t);
        }
        Pose::Talking => {
            let syl = (TAU * 2.0 * t).sin().abs();
            r.head_rot = 2.0 * wave(t, 1.0, 0.0) + 2.0;
            r.head_dy = -0.6 * syl;
            r.bob = -0.6 * (0.5 - 0.5 * (TAU * t).cos());
            r.mouth = if syl > 0.25 {
                Mouth::Open(2.6 * syl)
            } else {
                Mouth::Smirk
            };
            r.brow = Brow::Raised;
            r.lid = 0.18;
            r.look = (1.6, -0.6);
            r.blush = 0.45;
            r.swing = -r.head_rot * 1.2 + 3.0 * wave(t, 2.0, 0.2);
            r.hem = 0.6 * wave(t, 1.0, 0.0);
            let g = wave(t, 1.0, 0.0);
            r.hand_l = (149.0 + 1.5 * g, 153.0 - 2.0 * syl);
            r.bend_l = -1.0;
            r.strip = Some((12.0 + 4.0 * g, PointKind::Decision));
            r.extra = Extra::Present(t);
        }
        Pose::Sleeping => {
            let s = wave(t, 1.0, 0.0);
            r.seated = true;
            r.head_rot = 9.0 + 2.0 * s;
            r.head_dy = 2.0 + 1.2 * s;
            r.head_dx = 1.0;
            r.bob = -1.2 * (0.5 - 0.5 * (TAU * t).cos());
            r.eyes = Eyes::Closed;
            r.mouth = Mouth::O(1.0 + 0.3 * s);
            r.blush = 0.55;
            r.hat_on = true;
            r.swing = 8.0 - 2.0 * s;
            // The sword stands upright against his shoulder, hugged.
            r.saya = ((121.0, 192.0), 80.0);
            r.blade = Blade::Sheathed { peek: 0.0 };
            let (k, a) = r.saya;
            r.hand_l = add(k, dir(a), 12.0);
            r.hand_r = add(k, dir(a), 22.0);
            r.bend_r = -1.0;
            r.bend_l = 1.0;
            r.extra = Extra::Zzz(t);
        }
        Pose::Error => {
            let j = wave(t, 3.0, 0.0);
            r.shake = 1.4 * j;
            r.head_rot = -6.0 + 1.5 * wave(t, 3.0, 0.15);
            r.head_dy = 1.0;
            r.eyes = Eyes::Squeeze;
            r.brow = Brow::Worried;
            r.mouth = Mouth::Worried;
            r.blush = 0.6;
            r.blot = true;
            r.swing = 9.0 * wave(t, 3.0, 0.3);
            r.hem = 1.5 * wave(t, 3.0, 0.1);
            r.blade = Blade::Drawn {
                hand: (70.0 + 1.2 * j, 204.0),
                ang: 116.0 + 5.0 * wave(t, 3.0, 0.2),
            };
            r.bend_r = 1.0;
            r.frayed = true;
            r.extra = Extra::Splatter(t);
        }
    }
    r
}

// ---------------------------------------------------------------- drawing

fn draw(r: &Rig) -> String {
    let mut s = String::with_capacity(24 * 1024);
    let _ = write!(
        s,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}"><defs><linearGradient id="jir" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#16172B"/><stop offset=".55" stop-color="#40507E"/><stop offset="1" stop-color="#9FB6D6"/></linearGradient><linearGradient id="jhg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{HAIR_TOP}"/><stop offset="1" stop-color="{HAIR}"/></linearGradient><linearGradient id="jhw" gradientUnits="userSpaceOnUse" x1="0" y1="150" x2="0" y2="252"><stop offset="0" stop-color="{HAORI}"/><stop offset=".5" stop-color="{HAORI}"/><stop offset="1" stop-color="#232842"/></linearGradient></defs>"##,
        w = FRAME_W,
        h = FRAME_H
    );
    // Ground shadow (not swayed).
    let rx = if r.seated { 54 } else { 36 };
    let _ = write!(
        s,
        r##"<ellipse cx="100" cy="300" rx="{rx}" ry="4.5" fill="#2B1730" opacity=".13"/>"##
    );
    if let Extra::Splatter(t) = r.extra {
        // The ink pooling under the brush (on the ground, not shaken).
        let k = 1.0 + 0.08 * wave(t, 1.0, 0.0);
        let _ = write!(
            s,
            r#"<ellipse cx="38" cy="296" rx="{}" ry="4" fill="{INK}" opacity=".85"/>"#,
            n(15.0 * k)
        );
        splat(&mut s, (25.0, 293.0), 2.0, 5, 0.85);
        splat(&mut s, (55.0, 297.0), 1.6, 8, 0.85);
    }
    let _ = write!(
        s,
        r#"<g transform="translate({} 0) rotate({} 100 298)" stroke-linejoin="round" stroke-linecap="round">"#,
        n(r.shake),
        n(r.sway)
    );
    if !r.seated {
        feet(&mut s);
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
    if !r.hat_on {
        kasa_back(&mut s);
    }
    let _ = write!(s, r#"<g transform="{head_tf}">"#);
    back_hair(&mut s, r);
    s.push_str("</g>");
    torso(&mut s, r);
    // The scabbard sits in his obi (or stands against him, asleep).
    if !r.seated {
        sheathed(&mut s, r);
    }
    // Left arm (viewer's right): on the scabbard or holding up a strip.
    let (le, lh) = ik(SHOULDER_L, r.hand_l, UPPER_ARM, FOREARM, r.bend_l);
    // Right arm (viewer's left): on the hilt, or wielding the drawn sword.
    let target = match r.blade {
        Blade::Drawn { hand, .. } => hand,
        Blade::Sheathed { .. } => r.hand_r,
    };
    let (re, rh) = ik(SHOULDER_R, target, UPPER_ARM, FOREARM, r.bend_r);
    sleeve(&mut s, SHOULDER_L, le, lh);
    if r.seated {
        // Hugging the sword: it stands in front of his left arm, the
        // right arm reaching across to it.
        sheathed(&mut s, r);
        hand(&mut s, le, lh);
    }
    if let Extra::Strokes(tf) = r.extra {
        // The ink hangs in the air behind his head.
        strokes(&mut s, tf, false);
    }
    sleeve(&mut s, SHOULDER_R, re, rh);
    // Head.
    let _ = write!(s, r#"<g transform="{head_tf}">"#);
    head(&mut s, r);
    s.push_str("</g>");
    if let Some((tilt, kind)) = r.strip {
        held_strip(&mut s, lh, tilt, kind);
    }
    if !r.seated {
        hand(&mut s, le, lh);
    }
    let held = match r.blade {
        Blade::Drawn { ang, .. } => {
            let sw = sword(rh, ang);
            drawn_sword(&mut s, &sw, r.frayed);
            Some(sw)
        }
        Blade::Sheathed { .. } => None,
    };
    if let Extra::Strokes(tf) = r.extra {
        strokes(&mut s, tf, true);
    }
    hand(&mut s, re, rh);
    extras(&mut s, r, held.as_ref());
    s.push_str("</g></g></svg>");
    s
}

/// The scabbard with the sword in it (its guard thumbed out by `peek`).
fn sheathed(s: &mut String, r: &Rig) {
    saya(s, r.saya);
    if let Blade::Sheathed { peek } = r.blade {
        let (k, a) = r.saya;
        let d = dir(a);
        let tsuba = add(k, d, -1.0 - peek);
        if peek > 0.2 {
            // A sliver of steel and the gold collar between guard and mouth.
            seg(s, add(k, d, -0.5), tsuba, 4.2, STEEL, Some(STEEL_SH));
            seg(s, add(tsuba, d, 0.4), add(tsuba, d, 1.8), 4.6, GOLD, None);
        }
        hilt(s, add(tsuba, d, -TSUKA_LEN), tsuba);
    }
}

/// A straight piece of the sword: an outlined band from `a` to `b`, with an
/// optional highlight line along one side.
fn seg(s: &mut String, a: P, b: P, w: f32, fill: &str, hi: Option<&str>) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let d = (dx / len, dy / len);
    let (oa, ob) = (add(a, d, -0.9), add(b, d, 0.9));
    let _ = write!(
        s,
        r#"<path d="M{} {}L{} {}" stroke="{OL}" stroke-width="{}" stroke-linecap="butt"/><path d="M{} {}L{} {}" stroke="{fill}" stroke-width="{w}" stroke-linecap="butt"/>"#,
        n(oa.0),
        n(oa.1),
        n(ob.0),
        n(ob.1),
        n(w + 3.0),
        n(a.0),
        n(a.1),
        n(b.0),
        n(b.1)
    );
    if let Some(hi) = hi {
        let off = (-d.1 * w * 0.25, d.0 * w * 0.25);
        let _ = write!(
            s,
            r#"<path d="M{} {}L{} {}" stroke="{hi}" stroke-width="{}" stroke-linecap="butt"/>"#,
            n(a.0 - off.0),
            n(a.1 - off.1),
            n(b.0 - off.0),
            n(b.1 - off.1),
            n(w * 0.3)
        );
    }
}

/// The grip from the pommel `a` to the guard `b`: dark wrap with a cream
/// diamond lacing, a gold pommel cap and a round gold guard.
fn hilt(s: &mut String, a: P, b: P) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let d = (dx / len, dy / len);
    let nrm = (-d.1, d.0);
    seg(s, a, b, 5.2, TSUKA, None);
    // Lacing: a zigzag of the cream cord.
    let mut z = D::new();
    let steps = (len / 3.2) as usize;
    for i in 0..=steps {
        let p = add(
            add(a, d, 2.5 + i as f32 * (len - 4.0) / steps.max(1) as f32),
            nrm,
            if i % 2 == 0 { 2.0 } else { -2.0 },
        );
        z = if i == 0 { z.m(p) } else { z.l(p) };
    }
    let _ = write!(
        s,
        r#"<path d="{}" stroke="{CREAM}" stroke-width="1.1" fill="none"/>"#,
        z.s()
    );
    seg(s, add(a, d, -1.2), add(a, d, 1.6), 5.6, GOLD, Some(GOLD_L));
    let ang = dy.atan2(dx).to_degrees();
    let _ = write!(
        s,
        r#"<ellipse cx="{}" cy="{}" rx="1.9" ry="5.6" transform="rotate({} {} {})" fill="{GOLD}" stroke="{OL}" stroke-width="1.5"/>"#,
        n(b.0),
        n(b.1),
        n(ang),
        n(b.0),
        n(b.1)
    );
}

/// The scabbard: lacquered plum with gold fittings and a red cord.
fn saya(s: &mut String, (k, a): (P, f32)) {
    let d = dir(a);
    let end = add(k, d, SAYA_LEN);
    seg(s, k, end, 6.4, SAYA, Some(SAYA_HI));
    seg(s, add(end, d, -4.0), end, 6.8, GOLD, Some(GOLD_L));
    seg(s, k, add(k, d, 3.0), 7.0, GOLD, Some(GOLD_L));
    // The sageo cord, looped once.
    let p = add(k, d, 8.0);
    let nrm = (-d.1, d.0);
    let c = D::new()
        .m(p)
        .q(
            add(add(p, nrm, 9.0), d, -4.0),
            add(add(p, nrm, 11.0), d, 6.0),
        )
        .q(add(add(p, nrm, 8.0), d, 12.0), add(p, d, 9.0))
        .s();
    let _ = write!(
        s,
        r#"<path d="{c}" stroke="{OL}" stroke-width="3.2" fill="none"/><path d="{c}" stroke="{EMBER}" stroke-width="1.6" fill="none"/>"#
    );
}

/// The drawn brush-katana: grip, guard, the curved blade, a brass ferrule
/// and the ink-soaked brush at the end.
fn drawn_sword(s: &mut String, sw: &Sword, frayed: bool) {
    let d = (sw.tsuba.0 - sw.kashira.0, sw.tsuba.1 - sw.kashira.1);
    let dl = (d.0 * d.0 + d.1 * d.1).sqrt().max(0.001);
    let d = (d.0 / dl, d.1 / dl);
    let nrm = (-d.1, d.0);
    // Steel: the back edge on the curve's outside, the cutting edge inside.
    let w0 = 2.8;
    let w1 = 2.1;
    let bn = (-sw.bdir.1, sw.bdir.0);
    let blade = D::new()
        .m(add(sw.base, nrm, w0))
        .q(add(sw.ctrl, nrm, w0), add(sw.end, bn, w1))
        .l(add(sw.end, bn, -w1))
        .q(add(sw.ctrl, nrm, -w0), add(sw.base, nrm, -w0))
        .z();
    let _ = write!(
        s,
        r#"<path d="{blade}" fill="{STEEL}" stroke="{OL}" stroke-width="1.6"/>"#
    );
    // The temper line and a gleam.
    let hamon = D::new()
        .m(add(add(sw.base, d, 3.0), nrm, -0.7))
        .q(
            add(sw.ctrl, nrm, -0.8),
            add(add(sw.end, bn, -0.4), sw.bdir, -2.0),
        )
        .s();
    let _ = write!(
        s,
        r#"<path d="{hamon}" stroke="{STEEL_SH}" stroke-width="1" fill="none"/>"#
    );
    let g = lerp(sw.base, sw.end, 0.35);
    let _ = write!(
        s,
        r#"<path d="M{} {}L{} {}" stroke="white" stroke-width="1.3"/>"#,
        n(add(g, nrm, 1.0).0),
        n(add(g, nrm, 1.0).1),
        n(add(add(g, d, 7.0), nrm, 1.0).0),
        n(add(add(g, d, 7.0), nrm, 1.0).1)
    );
    // Gold collar at the base.
    seg(
        s,
        add(sw.tsuba, d, 0.5),
        add(sw.tsuba, d, 2.6),
        5.0,
        GOLD,
        None,
    );
    hilt(s, sw.kashira, sw.tsuba);
    // Brass ferrule and the brush.
    let f0 = sw.ferrule;
    let f1 = add(f0, sw.bdir, 3.5);
    seg(s, f0, f1, 4.6, GOLD, Some(GOLD_L));
    let b = sw.bdir;
    let bn = (-b.1, b.0);
    let p = |along: f32, side: f32| add(add(f1, b, along), bn, side);
    if frayed {
        // Splayed bristles, spitting ink.
        let d = D::new()
            .m(p(0.0, 3.2))
            .q(p(4.0, 5.6), p(BRUSH_LEN - 1.0, 6.5))
            .l(p(7.0, 1.6))
            .l(p(BRUSH_LEN + 1.0, 0.0))
            .l(p(7.0, -1.6))
            .l(p(BRUSH_LEN - 1.0, -6.5))
            .q(p(4.0, -5.6), p(0.0, -3.2))
            .z();
        let _ = write!(
            s,
            r#"<path d="{d}" fill="{INK}" stroke="{OL}" stroke-width="1.3"/>"#
        );
    } else {
        let d = D::new()
            .m(p(0.0, 3.1))
            .c(p(3.5, 4.6), p(8.5, 3.0), p(BRUSH_LEN, 0.0))
            .c(p(8.5, -3.0), p(3.5, -4.6), p(0.0, -3.1))
            .z();
        let _ = write!(
            s,
            r#"<path d="{d}" fill="{INK}" stroke="{OL}" stroke-width="1.3"/>"#
        );
        let h = D::new().m(p(2.0, 1.6)).q(p(5.0, 2.4), p(9.0, 0.9)).s();
        let _ = write!(
            s,
            r#"<path d="{h}" stroke="{INK_HI}" stroke-width="1.1" fill="none"/>"#
        );
    }
}

/// White tabi socks on straw sandals with red thongs, toes a little out.
fn feet(s: &mut String) {
    for (cx, m) in [(92.0f32, -1.0f32), (108.0, 1.0)] {
        let x = |dx: f32| n(cx + m * dx);
        let _ = write!(
            s,
            r#"<path d="M{} 297.5Q{} 301 {} 301L{} 301Q{} 301 {} 297.5Z" fill="{STRAW}" stroke="{OL}" stroke-width="1.6"/><path d="M{} 284L{} 284L{} 291Q{} 291 {} 294.5Q{} 298 {} 298L{} 298Q{} 298 {} 294Z" fill="{TABI}" stroke="{OL}" stroke-width="1.8"/><path d="M{} 293.5L{} 297M{} 293.5L{} 297" stroke="{EMBER}" stroke-width="1.6"/>"#,
            x(-8.0),
            x(-8.0),
            x(-5.0),
            x(10.0),
            x(13.0),
            x(13.0),
            x(-5.5),
            x(5.5),
            x(6.0),
            x(10.5),
            x(11.5),
            x(11.5),
            x(8.0),
            x(-4.0),
            x(-7.0),
            x(-6.5),
            x(4.0),
            x(1.0),
            x(4.0),
            x(8.5)
        );
    }
}

/// The straw kasa hanging on his back by its cord.
fn kasa_back(s: &mut String) {
    let c = (106.0, 178.0);
    let _ = write!(
        s,
        r#"<g transform="rotate(-8 {} {})"><ellipse cx="{}" cy="{}" rx="47" ry="43" fill="{STRAW}" stroke="{OL}" stroke-width="2.2"/><ellipse cx="{}" cy="{}" rx="40" ry="36" fill="none" stroke="{STRAW_SH}" stroke-width="1.6"/><ellipse cx="{}" cy="{}" rx="27" ry="24" fill="none" stroke="{STRAW_SH}" stroke-width="1.2"/>"#,
        c.0, c.1, c.0, c.1, c.0, c.1, c.0, c.1
    );
    let mut d = String::new();
    for k in 0..16 {
        let v = dir(k as f32 * 22.5 + 6.0);
        let _ = write!(
            d,
            "M{} {}L{} {}",
            n(c.0 + v.0 * 28.0),
            n(c.1 + v.1 * 25.5),
            n(c.0 + v.0 * 45.0),
            n(c.1 + v.1 * 41.0)
        );
    }
    let _ = write!(
        s,
        r#"<path d="{d}" stroke="{STRAW_SH}" stroke-width="1" opacity=".8"/><path d="M{} {}A46 42 0 0 1 {} {}" stroke="{STRAW_HI}" stroke-width="2.6" fill="none"/></g>"#,
        n(c.0 - 40.0),
        n(c.1 - 18.0),
        n(c.0 + 6.0),
        n(c.1 - 41.0)
    );
}

fn torso(s: &mut String, r: &Rig) {
    // Neck with the chin's shadow.
    let _ = write!(
        s,
        r#"<path d="M95.5 138L95.5 158L104.5 158L104.5 138Z" fill="{SKIN}" stroke="{OL}" stroke-width="1.8"/><path d="M96.4 143Q100 149 103.6 143L103.6 147Q100 151 96.4 147Z" fill="{SKIN_SH}"/>"#
    );
    let hem_y = if r.seated { 236.0 } else { 248.0 };
    if r.seated {
        // Sitting cross-legged: the hakama spreads into a wide heap, the
        // tabi soles peeking out at the front.
        let _ = write!(
            s,
            r#"<path d="M84 200L116 200C126 214 146 230 152 250Q153 259 140 260Q100 264 60 260Q47 259 48 250C54 230 74 214 84 200Z" fill="{HAKAMA}" stroke="{OL}" stroke-width="2"/><path d="M90 222Q78 240 70 258M110 222Q122 240 130 258M100 230V262" stroke="{HAKAMA_SH}" stroke-width="1.6" fill="none"/><path d="M56 252Q100 260 144 252" stroke="{HAKAMA_SH}" stroke-width="2" fill="none"/>"#
        );
        for (cx, m) in [(90.0f32, -1.0f32), (110.0, 1.0)] {
            let _ = write!(
                s,
                r#"<path d="M{} 253C{} 250 {} 250 {} 254C{} 259 {} 261 {} 261C{} 261 {} 258 {} 253Z" fill="{TABI}" stroke="{OL}" stroke-width="1.7"/>"#,
                n(cx - m * 7.0),
                n(cx - m * 3.0),
                n(cx + m * 5.0),
                n(cx + m * 8.0),
                n(cx + m * 9.5),
                n(cx + m * 6.0),
                n(cx + m * 1.0),
                n(cx - m * 4.0),
                n(cx - m * 7.5),
                n(cx - m * 7.0)
            );
        }
    } else {
        // Hakama: wide pleated trousers down to the ankles.
        let _ = write!(
            s,
            r#"<path d="M83 204L117 204C121 230 128 262 137 288Q120 292 104 289.5L100 268L96 289.5Q80 292 63 288C72 262 79 230 83 204Z" fill="{HAKAMA}" stroke="{OL}" stroke-width="2"/><path d="M91 212L84 287M109 212L116 287M100 214V268" stroke="{HAKAMA_SH}" stroke-width="1.5" fill="none"/><path d="M95 216L93 286M105 216L107 286" stroke="{HAKAMA_HI}" stroke-width="1.1" fill="none" opacity=".7"/>"#
        );
    }
    // The kimono under the haori, its crossed linen collar over a cream
    // under-collar.
    let _ = write!(
        s,
        r#"<path d="M86 151L114 151L117 206L83 206Z" fill="{KIMONO}" stroke="{OL}" stroke-width="1.6"/><path d="M107 152L95.5 186" stroke="{KIMONO_SH}" stroke-width="1.6"/><path d="M92 151L100 166L108 151" stroke="{OL}" stroke-width="4.6" fill="none"/><path d="M92 151L100 166L108 151" stroke="{CREAM}" stroke-width="2.6" fill="none"/>"#
    );
    // The obi: a red sash, tied flat.
    let _ = write!(
        s,
        r#"<path d="M82 196L118 196L118.5 207L81.5 207Z" fill="{EMBER}" stroke="{OL}" stroke-width="1.7"/><path d="M83 199.5H117" stroke="{TERRA}" stroke-width="1.3"/>"#
    );
    // The haori: two open front panels, darkening toward the hem like ink
    // soaking up a page, the hem frayed and stirring.
    for m in [-1.0f32, 1.0] {
        let x = |dx: f32| 100.0 + m * dx;
        let stir = r.hem * if m < 0.0 { 1.6 } else { -1.2 };
        let low = hem_y;
        let d = D::new()
            .m((x(7.0), 150.0))
            .c((x(13.0), 151.0), (x(18.0), 153.0), (x(21.0), 159.0))
            .c(
                (x(26.0), 185.0),
                (x(31.0), 216.0),
                (x(40.0) + stir, low + 1.0),
            )
            .q(
                (x(33.0) + stir * 0.6, low + 4.0),
                (x(29.0) + stir * 0.4, low + 2.5),
            )
            .l((x(27.0) + stir * 0.3, low - 1.0))
            .l((x(24.5) + stir * 0.2, low + 3.5))
            .q((x(16.0), low + 4.5), (x(9.0), low + 2.0))
            .l((x(8.0), 172.0))
            .c((x(7.0), 162.0), (x(6.0), 156.0), (x(7.0), 150.0))
            .z();
        let _ = write!(
            s,
            r#"<path d="{d}" fill="url(#jhw)" stroke="{OL}" stroke-width="2"/>"#
        );
        // The collar band down the front edge, a fold line, and the ensō
        // crest on the chest.
        let _ = write!(
            s,
            r#"<path d="M{} 153Q{} 160 {} 172L{} {}" stroke="{HAORI_SH}" stroke-width="3.6" fill="none"/><path d="M{} 190Q{} 214 {} {}" stroke="{HAORI_SH}" stroke-width="1.5" fill="none"/>"#,
            n(x(9.5)),
            n(x(10.5)),
            n(x(11.0)),
            n(x(11.5)),
            n(low),
            n(x(23.0)),
            n(x(26.0)),
            n(x(31.0) + stir * 0.5),
            n(low - 1.0)
        );
        brush(
            s,
            &arc_pts((x(19.5), 178.0), 4.2, 4.2, -60.0, 245.0, 14),
            |u| 0.6 + 1.4 * (PI * u).sin(),
            CREAM,
            0.85,
        );
    }
    // A few spots of ink at the hem.
    if !r.seated {
        for (x, y, rr) in [(70.0, 240.0, 1.4), (74.5, 236.0, 0.8), (128.0, 243.0, 1.1)] {
            let _ = write!(
                s,
                r#"<circle cx="{x}" cy="{y}" r="{rr}" fill="{INK}" opacity=".7"/>"#
            );
        }
    }
    // The haori's tie cord, a little knot with a tassel.
    let _ = write!(
        s,
        r#"<path d="M92 180Q96 184 100 183Q104 184 108 180" stroke="{OL}" stroke-width="3" fill="none"/><path d="M92 180Q96 184 100 183Q104 184 108 180" stroke="{CREAM}" stroke-width="1.4" fill="none"/><path d="M100 184L99 190.5" stroke="{OL}" stroke-width="2.6"/><path d="M100 184L99 190.5" stroke="{CREAM}" stroke-width="1.2"/><circle cx="100" cy="183" r="2.3" fill="{CREAM}" stroke="{OL}" stroke-width="1.3"/>"#
    );
    // The kasa's cord across his collarbones (not when it's on his head).
    if !r.hat_on {
        let _ = write!(
            s,
            r#"<path d="M83 157Q92 163 100 162Q108 163 117 157" stroke="{OL}" stroke-width="2.6" fill="none"/><path d="M83 157Q92 163 100 162Q108 163 117 157" stroke="{EMBER}" stroke-width="1.2" fill="none"/>"#
        );
    }
}

/// The wide haori sleeve: an arm-thick band with a square kimono sleeve
/// hanging from the forearm (the hand and cuff are drawn with `hand`).
fn sleeve(s: &mut String, sh: P, el: P, ha: P) {
    let (dx, dy) = (ha.0 - el.0, ha.1 - el.1);
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let d = (dx / len, dy / len);
    let wrist = add(ha, d, -4.5);
    // The hanging part of the sleeve.
    let a = lerp(el, wrist, 0.05);
    let b = lerp(el, wrist, 0.85);
    let drop = 15.0 + 5.0 * d.0.abs();
    let bag = D::new()
        .m(a)
        .l(b)
        .l((b.0, b.1 + drop * 0.85))
        .q(
            (b.0, b.1 + drop + 2.0),
            (lerp(a, b, 0.5).0, lerp(a, b, 0.5).1 + drop + 2.0),
        )
        .q((a.0, a.1 + drop + 2.0), (a.0, a.1 + drop * 0.7))
        .z();
    let _ = write!(
        s,
        r#"<path d="{bag}" fill="{HAORI_SH}" stroke="{OL}" stroke-width="1.8"/>"#
    );
    let _ = write!(
        s,
        r#"<path d="M{} {}L{} {}L{} {}" stroke="{OL}" stroke-width="16.6" fill="none"/><path d="M{} {}L{} {}L{} {}" stroke="{HAORI}" stroke-width="13" fill="none"/><path d="M{} {}Q{} {} {} {}" stroke="{HAORI_SH}" stroke-width="1.4" fill="none"/>"#,
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

/// His hand, coming out of the wide sleeve's mouth over a cream under-cuff
/// (drawn over whatever the hand holds).
fn hand(s: &mut String, el: P, ha: P) {
    let (dx, dy) = (ha.0 - el.0, ha.1 - el.1);
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let d = (dx / len, dy / len);
    let nrm = (-d.1, d.0);
    let b = add(ha, d, -10.0);
    let f = add(ha, d, -3.6);
    let q = [
        add(b, nrm, 6.8),
        add(f, nrm, 8.0),
        add(f, nrm, -8.0),
        add(b, nrm, -6.8),
    ];
    let _ = write!(
        s,
        r#"<path d="{}" fill="{HAORI_HI}" stroke="{OL}" stroke-width="1.8"/>"#,
        D::new().m(q[0]).l(q[1]).q(add(f, d, 1.4), q[2]).l(q[3]).z()
    );
    let _ = write!(
        s,
        r#"<path d="M{} {}L{} {}" stroke="{OL}" stroke-width="4.4"/><path d="M{} {}L{} {}" stroke="{CREAM}" stroke-width="2.4"/>"#,
        n(add(f, nrm, 4.6).0),
        n(add(f, nrm, 4.6).1),
        n(add(f, nrm, -4.6).0),
        n(add(f, nrm, -4.6).1),
        n(add(f, nrm, 4.6).0),
        n(add(f, nrm, 4.6).1),
        n(add(f, nrm, -4.6).0),
        n(add(f, nrm, -4.6).1)
    );
    let fist = add(ha, d, 0.4);
    let _ = write!(
        s,
        r#"<circle cx="{}" cy="{}" r="4.6" fill="{SKIN}" stroke="{OL}" stroke-width="1.6"/><path d="M{} {}Q{} {} {} {}" stroke="{SKIN_SH}" stroke-width="1.1" fill="none"/>"#,
        n(fist.0),
        n(fist.1),
        n(add(fist, nrm, 2.6).0),
        n(add(fist, nrm, 2.6).1),
        n(add(fist, d, 2.4).0),
        n(add(fist, d, 2.4).1),
        n(add(fist, nrm, -2.6).0),
        n(add(fist, nrm, -2.6).1)
    );
}

fn back_hair(s: &mut String, r: &Rig) {
    // The short ponytail, poking out behind his head and swinging.
    let _ = write!(
        s,
        r#"<g transform="translate(123 48) rotate({})"><path d="M-3 -6C10 -16 30 -14 40 -2L31 -1L38 10L25 5L27 17L16 7L11 15Q5 7 -3 6Z" fill="url(#jhg)" stroke="{OL}" stroke-width="2.2"/><path d="M5 -5Q18 -9 30 -3M7 1Q15 2 21 8" stroke="{HAIR_HI}" stroke-width="1.4" fill="none"/></g>"#,
        n(14.0 + r.swing)
    );
    // The tie: a red cord.
    let _ = write!(
        s,
        r#"<path d="M118 43Q125 46 124 56" stroke="{OL}" stroke-width="5" fill="none"/><path d="M118 43Q125 46 124 56" stroke="{EMBER}" stroke-width="3" fill="none"/>"#
    );
    let _ = write!(
        s,
        r#"<path d="M100 35C72 35 58 53 60 78C60 86 61 91 63 96L56 103L67 101L69 109L77 101L123 101L131 109L133 101L144 103L137 96C139 91 140 86 140 78C141 53 128 35 100 35Z" fill="{HAIR_SH}" stroke="{OL}" stroke-width="2.2"/>"#
    );
}

fn head(s: &mut String, r: &Rig) {
    // Face: chibi round, a slightly firmer chin than the girls'.
    let _ = write!(
        s,
        r#"<path d="M68 76C68 45 132 45 132 76C132 98 116 111.5 100 113.5C84 111.5 68 98 68 76Z" fill="{SKIN}" stroke="{OL}" stroke-width="2.2"/>"#
    );
    if r.blush > 0.0 {
        let _ = write!(
            s,
            r#"<g fill="{BLUSH}" opacity="{:.2}"><ellipse cx="79" cy="102" rx="5.5" ry="2.6"/><ellipse cx="121" cy="102" rx="5.5" ry="2.6"/></g>"#,
            r.blush
        );
    }
    // His ink smudge on the cheek (a big blot when things went wrong).
    if r.blot {
        splat(s, (79.0, 104.0), 3.4, 3, 0.9);
    } else {
        brush(
            s,
            &[(115.5, 104.5), (119.0, 103.6), (123.0, 104.2)],
            |u| 0.4 + 1.5 * (PI * u).sin(),
            INK,
            0.55,
        );
    }
    if !r.hat_on {
        eye(s, r, 86.0, -1.0);
        eye(s, r, 114.0, 1.0);
    }
    // Nose.
    let _ = write!(
        s,
        r#"<path d="M100.5 99l-.8 1.3" stroke="{SKIN_SH}" stroke-width="1.4"/>"#
    );
    mouth(s, r.mouth);
    // Front hair: a messy crown, spiky bangs falling over the brow and two
    // pointed side locks.
    let fringe = D::new()
        .m((63.0, 90.0))
        .c((59.0, 68.0), (62.0, 52.0), (71.0, 44.0))
        .l((68.0, 38.0))
        .l((79.0, 39.0))
        .c((86.0, 34.5), (93.0, 33.5), (101.0, 34.0))
        .c((122.0, 35.0), (135.0, 46.0), (138.0, 60.0))
        .l((146.0, 63.0))
        .l((139.0, 70.0))
        .c((140.0, 78.0), (139.0, 85.0), (137.0, 90.0))
        .q((136.0, 98.0), (134.0, 105.0))
        .q((128.0, 98.0), (127.0, 84.0))
        .q((125.0, 74.0), (121.0, 68.0))
        .l((119.5, 80.0))
        .q((116.0, 70.0), (110.0, 64.0))
        .l((107.5, 78.5))
        .q((104.0, 69.0), (97.0, 63.0))
        .l((93.0, 77.0))
        .q((90.0, 69.0), (84.0, 66.0))
        .l((80.5, 79.0))
        .q((78.0, 72.0), (76.0, 69.0))
        .q((74.0, 80.0), (73.0, 90.0))
        .q((70.0, 98.0), (66.0, 105.0))
        .q((64.0, 98.0), (63.0, 90.0))
        .z();
    let _ = write!(
        s,
        r#"<path d="{fringe}" fill="url(#jhg)" stroke="{OL}" stroke-width="2.2"/>"#
    );
    let _ = write!(
        s,
        r#"<path d="M110 64Q108 52 101 42M97 63Q93 53 86 47M121 68Q120 57 115 49M84 66Q82 58 80 53M127 84Q131 70 130 60M73 90Q69 72 71 62" stroke="{HAIR_SH}" stroke-width="1.2" fill="none"/><path d="M73 55Q85 44 97 43M106 43Q118 45 126 53" stroke="{HAIR_HI}" stroke-width="3" fill="none"/><path d="M98 35Q101 24 111 21Q104 27 104 35" fill="url(#jhg)" stroke="{OL}" stroke-width="1.8"/>"#
    );
    if !r.hat_on {
        // Brows, straight and low: cool.
        let (li, lo, arch) = match r.brow {
            Brow::Calm => (75.5, 74.5, -1.2),
            Brow::Stern => (77.0, 73.0, -0.6),
            Brow::Raised => (73.0, 72.5, -2.2),
            Brow::Worried => (72.0, 77.0, 0.4),
        };
        for m in [-1.0f32, 1.0] {
            let inner = 100.0 + m * 7.5;
            let outer = 100.0 + m * 19.0;
            let (li, lo) = if r.wink && m < 0.0 {
                (li + 1.5, lo + 1.5)
            } else {
                (li, lo)
            };
            let _ = write!(
                s,
                r#"<path d="M{} {}Q{} {} {} {}" stroke="{BROW}" stroke-width="2.4" fill="none"/>"#,
                n(inner),
                n(li),
                n((inner + outer) / 2.0),
                n((li + lo) / 2.0 + arch),
                n(outer),
                n(lo)
            );
        }
    } else {
        kasa_on(s);
    }
    if let Extra::Splatter(t) = r.extra {
        let dy = 3.0 * t;
        let _ = write!(
            s,
            r#"<g transform="translate(139 {})"><path d="M0 -6C3 -1.5 4.4 1.5 2.6 4C1 6 -1 6 -2.6 4C-4.4 1.5 -3 -1.5 0 -6Z" fill="{SWEAT}" stroke="{OL}" stroke-width="1.3"/><ellipse cx="-.9" cy="1.6" rx=".9" ry="1.5" fill="white"/></g>"#,
            n(70.0 + dy)
        );
    }
}

/// The kasa worn on his head, tipped forward over his eyes.
fn kasa_on(s: &mut String) {
    let _ = write!(
        s,
        r#"<g transform="rotate(-6 100 70)"><path d="M44 84Q100 106 156 84L104 34Q101 31.5 98 34Z" fill="{STRAW}" stroke="{OL}" stroke-width="2.2"/><path d="M56 82L100 36M72 90L101 36M88 94L101 36M112 94L101 36M128 90L101 36M144 83L101 36" stroke="{STRAW_SH}" stroke-width="1.1" opacity=".85"/><path d="M60 77Q100 92 140 77" stroke="{STRAW_SH}" stroke-width="1.5" fill="none"/><path d="M85 50Q75 62 66 74" stroke="{STRAW_HI}" stroke-width="2.6" fill="none"/><path d="M44 84Q100 106 156 84" stroke="{OL}" stroke-width="2.2" fill="none"/></g>"#
    );
}

fn eye(s: &mut String, r: &Rig, cx: f32, m: f32) {
    let cy = 91.0;
    let closed = |s: &mut String| {
        let _ = write!(
            s,
            r#"<path d="M{} {}Q{} {} {} {}" stroke="{OL}" stroke-width="2.4" fill="none"/><path d="M{} {}l{} 1.2" stroke="{OL}" stroke-width="1.6"/>"#,
            n(cx - m * 7.0),
            n(cy + 2.5),
            n(cx),
            n(cy + 6.0),
            n(cx + m * 8.0),
            n(cy + 1.5),
            n(cx + m * 8.0),
            n(cy + 1.5),
            n(m * 2.0)
        );
    };
    if r.wink && m < 0.0 {
        closed(s);
        return;
    }
    match r.eyes {
        Eyes::Closed => closed(s),
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
        Eyes::Open => {
            let ry = 9.6;
            let ix = cx + r.look.0;
            let iy = cy + 1.2 + r.look.1;
            let id = if m < 0.0 { "jel" } else { "jer" };
            let _ = write!(
                s,
                r#"<clipPath id="{id}"><ellipse cx="{}" cy="{}" rx="7.6" ry="{ry}"/></clipPath><ellipse cx="{}" cy="{}" rx="7.6" ry="{ry}" fill="white"/><g clip-path="url(#{id})"><ellipse cx="{}" cy="{}" rx="6" ry="7.9" fill="url(#jir)" stroke="{OL}" stroke-width=".9"/><ellipse cx="{}" cy="{}" rx="2.6" ry="3.7" fill="{OL}"/><ellipse cx="{}" cy="{}" rx="1.9" ry="2.5" fill="white"/><circle cx="{}" cy="{}" r="1" fill="white" opacity=".85"/>"#,
                n(cx),
                n(cy),
                n(cx),
                n(cy),
                n(ix),
                n(iy),
                n(ix),
                n(iy + 0.4),
                n(ix - 2.2),
                n(iy - 2.8),
                n(ix + 2.3),
                n(iy + 3.4)
            );
            let top = cy - ry + 0.6;
            let yl = top + r.lid * (2.0 * ry - 1.5);
            if r.lid > 0.0 {
                let _ = write!(
                    s,
                    r#"<rect x="{}" y="{}" width="19" height="{}" fill="{SKIN}"/>"#,
                    n(cx - 9.5),
                    n(cy - 13.0),
                    n(yl - cy + 13.0 + 0.6)
                );
            }
            s.push_str("</g>");
            if r.lid >= 0.95 {
                closed(s);
            } else {
                // A heavy, nearly straight upper lash line with a flick at
                // the outer corner.
                let _ = write!(
                    s,
                    r#"<path d="M{} {}Q{} {} {} {}L{} {}" stroke="{OL}" stroke-width="3" fill="none"/>"#,
                    n(cx - m * 7.4),
                    n(yl + 1.8),
                    n(cx),
                    n(yl - 1.6),
                    n(cx + m * 8.2),
                    n(yl + 0.4),
                    n(cx + m * 10.2),
                    n(yl - 1.6)
                );
                let _ = write!(
                    s,
                    r##"<path d="M{} {}Q{} {} {} {}" stroke="#4A4A6E" stroke-width="1.2" fill="none"/>"##,
                    n(cx - m * 3.0),
                    n(cy + ry - 0.2),
                    n(cx + m * 1.0),
                    n(cy + ry + 0.6),
                    n(cx + m * 5.0),
                    n(cy + ry - 1.2)
                );
            }
        }
    }
}

fn mouth(s: &mut String, m: Mouth) {
    match m {
        Mouth::Flat => {
            let _ = write!(
                s,
                r#"<path d="M97.2 106.2L102.8 105.8" stroke="{OL}" stroke-width="1.6" fill="none"/>"#
            );
        }
        Mouth::Smirk => {
            let _ = write!(
                s,
                r#"<path d="M96.8 106Q100 107 103.6 104.8" stroke="{OL}" stroke-width="1.6" fill="none"/>"#
            );
        }
        Mouth::Open(h) => {
            let b = 105.0 + h + 1.2;
            let _ = write!(
                s,
                r#"<path d="M96.6 104.8Q100 105.8 103.4 104.8Q103 {} 100 {}Q97 {} 96.6 104.8Z" fill="{MOUTH}" stroke="{OL}" stroke-width="1.4"/><ellipse cx="100" cy="{}" rx="1.9" ry="{}" fill="{TONGUE}"/>"#,
                n(b),
                n(b + 0.4),
                n(b),
                n(b - 0.8),
                n((h * 0.35).clamp(0.4, 1.2))
            );
        }
        Mouth::O(r) => {
            let _ = write!(
                s,
                r#"<ellipse cx="100" cy="106" rx="{}" ry="{}" fill="{MOUTH}" stroke="{OL}" stroke-width="1.3"/>"#,
                n(r * 0.85),
                n(r)
            );
        }
        Mouth::Worried => {
            let _ = write!(
                s,
                r#"<path d="M95.5 107q1.5-1.8 3 0t3 0 3 0" stroke="{OL}" stroke-width="1.5" fill="none"/>"#
            );
        }
    }
}

/// A paper strip held up by its lower end in his hand at `h`.
fn held_strip(s: &mut String, h: P, tilt: f32, kind: PointKind) {
    let _ = write!(
        s,
        r#"<g transform="translate({} {}) rotate({})">"#,
        n(h.0),
        n(h.1),
        n(tilt)
    );
    strip_shape(s, kind, -8.0, -40.0, 16.0, 44.0);
    s.push_str("</g>");
}

/// The ink trails of the writing strokes, and the paper strips they turn
/// into (`tf` is the frame time): the trails, or (`strips`) the strips.
fn strokes(s: &mut String, tf: f32, strips: bool) {
    for (i, &(a, b)) in CUTS.iter().enumerate() {
        if tf <= a {
            continue;
        }
        // The arc the brush tip swept, while it lasts.
        let fade = if tf <= b { 1.0 } else { 1.0 - (tf - b) / 2.0 };
        let from = a.max(tf - 3.0).max(a);
        let to = tf.min(b);
        if !strips && fade > 0.0 && to > from {
            let steps = 14;
            let pts: Vec<P> = (0..=steps)
                .map(|k| {
                    let (h, ang) = stroke_at(from + (to - from) * k as f32 / steps as f32);
                    sword(h, ang).tip
                })
                .collect();
            brush(
                s,
                &pts,
                |u| 1.0 + 11.0 * (PI * (0.12 + 0.8 * u.powf(0.8))).sin(),
                INK,
                0.92 * fade,
            );
            // Droplets flung off the stroke.
            if tf <= b {
                let p = pts[steps];
                for k in 0..3 {
                    let q = add(
                        p,
                        dir(40.0 + 110.0 * k as f32 + 30.0 * i as f32),
                        5.0 + 2.0 * k as f32,
                    );
                    let _ = write!(
                        s,
                        r#"<circle cx="{}" cy="{}" r="{}" fill="{INK}"/>"#,
                        n(q.0),
                        n(q.1),
                        n(1.3 - 0.3 * k as f32)
                    );
                }
            }
        }
        // Once the cut is done, a strip separates from the ink and flies
        // off to the rack.
        if strips && tf > b - 0.5 {
            let u = ((tf - b + 0.5) / 4.0).clamp(0.0, 1.0);
            if u < 1.0 {
                let (h, ang) = stroke_at((a + b) / 2.0);
                let start = sword(h, ang).tip;
                let start = lerp(start, (100.0, 150.0), 0.25);
                let e = 1.0 - (1.0 - u) * (1.0 - u);
                let p = (
                    start.0 + (170.0 - start.0) * e,
                    start.1 - 70.0 * e + 6.0 * (PI * u).sin(),
                );
                let sc = (0.5 + 1.4 * u).min(1.0) * (1.0 - 0.35 * u);
                let rot = -20.0 + 60.0 * u + 12.0 * (TAU * u * 1.5).sin();
                let op = if u < 0.7 { 1.0 } else { (1.0 - u) / 0.3 };
                let kind = [PointKind::Action, PointKind::Fact][i];
                let _ = write!(
                    s,
                    r#"<g transform="translate({} {}) rotate({}) scale({:.2})" opacity="{op:.2}">"#,
                    n(p.0),
                    n(p.1),
                    n(rot),
                    sc
                );
                strip_shape(s, kind, -8.0, -22.0, 16.0, 44.0);
                s.push_str("</g>");
            }
        }
    }
}

fn extras(s: &mut String, r: &Rig, sw: Option<&Sword>) {
    match r.extra {
        Extra::None | Extra::Strokes(_) => {}
        Extra::Listen(t) => {
            // Brush-stroke ripples arriving at his ear, one after another.
            let ear = head_point(r, (62.0, 90.0));
            for k in 0..3 {
                let u = (t * 2.0 + k as f32 / 3.0) % 1.0;
                let rr = 6.0 + 12.0 * u;
                let op = (PI * u).sin();
                let pts = arc_pts(ear, rr, rr * 1.15, 145.0, 215.0, 8);
                brush(s, &pts, |v| 0.4 + 2.2 * (PI * v).sin(), INK, op * 0.85);
            }
        }
        Extra::Meditate(t) => {
            if let Some(sw) = sw {
                // An ink drop gathers on the brush tip, falls, and again.
                let tip = sw.tip;
                let u = t;
                if u < 0.6 {
                    let k = u / 0.6;
                    let _ = write!(
                        s,
                        r#"<path d="{}" fill="{INK}" stroke="{OL}" stroke-width="1"/>"#,
                        drop_d((tip.0, tip.1 - 1.8 + 4.0 * k), 0.8 + 1.6 * k)
                    );
                } else {
                    let k = (u - 0.6) / 0.4;
                    let y = tip.1 + 2.2 + 150.0 * k * k;
                    if y < 290.0 {
                        let _ = write!(
                            s,
                            r#"<path d="{}" fill="{INK}" stroke="{OL}" stroke-width="1"/>"#,
                            drop_d((tip.0, y), 2.4)
                        );
                    }
                }
            }
            // A leaf drifting across, see-sawing down.
            let x = 186.0 - 120.0 * t;
            let y = 34.0 + 30.0 * t + 6.0 * wave(t, 2.0, 0.0);
            let rot = 40.0 * wave(t, 2.0, 0.25) - 20.0;
            let _ = write!(
                s,
                r#"<g transform="translate({} {}) rotate({})"><path d="M-7 0Q-2 -5 7 0Q-2 5 -7 0Z" fill="{TERRA}" stroke="{OL}" stroke-width="1.3"/><path d="M-8.5 .6L5 0" stroke="{OL}" stroke-width=".9"/></g>"#,
                n(x),
                n(y),
                n(rot)
            );
            // An ensō drawing itself in the air above him, then fading.
            let c = (166.0, 50.0);
            let grow = smooth(t / 0.65);
            let fade = if t < 0.8 { 1.0 } else { 1.0 - (t - 0.8) / 0.2 };
            if grow > 0.02 {
                let end = -70.0 + 320.0 * grow;
                let steps = (6.0 + 22.0 * grow) as usize;
                let pts = arc_pts(c, 15.0, 15.0, -70.0, end, steps);
                brush(
                    s,
                    &pts,
                    |u| {
                        1.0 + 3.6
                            * (PI * u.min(0.9) / 0.9).sin().max(0.0).powf(0.5)
                            * (1.0 - 0.4 * u)
                    },
                    INK,
                    0.88 * fade,
                );
            }
            let _ = write!(
                s,
                r#"<circle cx="{}" cy="{}" r="2" fill="{INK}" opacity="{:.2}"/>"#,
                n(150.0),
                n(31.0),
                0.6 * fade * grow
            );
        }
        Extra::Present(t) => {
            let k = (TAU * 2.0 * t).sin().abs();
            let p = (176.0, 104.0);
            let _ = write!(
                s,
                r#"<path d="M{} {}l1.6-4 1.6 4 4 1.6-4 1.6-1.6 4-1.6-4-4-1.6Z" fill="{GOLD_L}" stroke="{OL}" stroke-width="1" opacity="{:.2}"/>"#,
                n(p.0 - 1.6),
                n(p.1 - 1.6),
                0.35 + 0.65 * k
            );
            let _ = write!(
                s,
                r#"<circle cx="{}" cy="{}" r="{}" fill="{GOLD_L}" stroke="{OL}" stroke-width=".8"/>"#,
                n(155.0),
                n(122.0 - 2.0 * k),
                n(1.2 + 0.6 * (1.0 - k))
            );
        }
        Extra::Zzz(t) => {
            for k in 0..3 {
                let u = (t + k as f32 / 3.0) % 1.0;
                let x = 152.0 + u * 16.0 + 3.0 * (TAU * u).sin();
                let y = 120.0 - u * 50.0;
                let z = 4.0 + u * 4.5;
                let op = (PI * u).sin().powf(0.6);
                let pts = [
                    (x - z / 2.0, y - z / 2.0),
                    (x + z / 2.0, y - z / 2.0),
                    (x - z / 2.0, y + z / 2.0),
                    (x + z / 2.0, y + z / 2.0),
                ];
                brush(s, &pts, |v| 0.8 + z * 0.22 * (1.0 - 0.5 * v), INK, op);
            }
        }
        Extra::Splatter(t) => {
            // Ink flung off the frayed brush in all directions.
            if let Some(sw) = sw {
                let tip = sw.tip;
                for k in 0..6 {
                    let u = (t * 2.0 + k as f32 / 6.0) % 1.0;
                    let a = -150.0 + 70.0 * k as f32 + 25.0 * hash(k, 1);
                    let v = dir(a);
                    let p = (
                        tip.0 + v.0 * (6.0 + 28.0 * u),
                        tip.1 + v.1 * (6.0 + 28.0 * u) + 30.0 * u * u,
                    );
                    let op = (1.0 - u).powf(0.7);
                    splat(s, p, 1.6 + 1.4 * (1.0 - u), k + 20, op);
                }
            }
            // Little stress marks at his temple.
            let m = head_point(r, (148.0, 64.0));
            let _ = write!(
                s,
                r#"<path d="M{} {}l6-4M{} {}h7M{} {}l6 3" stroke="{OL}" stroke-width="1.8"/>"#,
                n(m.0),
                n(m.1),
                n(m.0 + 2.0),
                n(m.1 + 8.0),
                n(m.0),
                n(m.1 + 16.0)
            );
        }
    }
}

// ---------------------------------------------------------------- the rack

/// Size of the sword-rack panel `notes_svg` draws (its viewBox).
pub const NOTES_W: f32 = 240.0;
pub const NOTES_H: f32 = 160.0;
/// Frames in one loop of the strips swaying (2 s at `FPS`).
pub const NOTES_FRAMES: usize = 24;
/// Where strips may hang: between the rack's posts, under the top beam.
const RACK: (f32, f32, f32, f32) = (30.0, 24.0, 210.0, 142.0);
/// At most this many bars; past what they hold, the oldest strips are
/// tied into a bundle leaning on the rack.
const MAX_ROWS: usize = 3;
const MIN_STRIP_W: f32 = 7.0;

/// Paper colour and brush-mark colour per kind, close to the card colours
/// of the theme (every kind also has its own mark: an ensō for a decision,
/// a sword slash for an action item, a hook for an open question, three
/// bars for a fact or a key point).
fn kind_colors(kind: PointKind) -> (&'static str, &'static str) {
    match kind {
        PointKind::Decision => ("#F6CF72", "#C98A1E"),
        PointKind::Action => ("#F39A7E", "#C2532F"),
        PointKind::Question => ("#CFB2E6", "#8D63B3"),
        PointKind::Fact => ("#93D1CF", "#3F928F"),
        PointKind::Point => ("#A9C4FF", "#4F6FB8"),
    }
}

/// A kind's brush mark centered at `c`, about `g` in radius.
fn glyph(s: &mut String, kind: PointKind, c: P, g: f32) {
    let w = (g * 0.5).max(0.9);
    match kind {
        PointKind::Decision => {
            let pts = arc_pts(c, g, g, -30.0, 300.0, 20);
            brush(s, &pts, |u| w * (1.0 + 0.9 * (PI * u).sin()), INK, 1.0);
        }
        PointKind::Action => {
            let pts = [
                (c.0 + g * 0.9, c.1 - g * 1.1),
                (c.0 + g * 0.1, c.1 - g * 0.1),
                (c.0 - g * 0.9, c.1 + g * 1.2),
            ];
            brush(
                s,
                &pts,
                |u| w * (0.5 + 2.4 * (1.0 - u) * (0.3 + u)),
                INK,
                1.0,
            );
            let _ = write!(
                s,
                r#"<circle cx="{}" cy="{}" r="{}" fill="{INK}"/>"#,
                n(c.0 + g * 0.75),
                n(c.1 + g * 0.75),
                n(w * 0.55)
            );
        }
        PointKind::Question => {
            let mut pts = arc_pts((c.0, c.1 - g * 0.45), g * 0.75, g * 0.65, 190.0, 410.0, 10);
            pts.push((c.0 + g * 0.05, c.1 + g * 0.55));
            brush(
                s,
                &pts,
                |u| w * (0.5 + 0.9 * (PI * u).sin() + 0.2 * (1.0 - u)),
                INK,
                1.0,
            );
            let _ = write!(
                s,
                r#"<circle cx="{}" cy="{}" r="{}" fill="{INK}"/>"#,
                n(c.0 + g * 0.05),
                n(c.1 + g * 1.15),
                n(w * 0.6)
            );
        }
        PointKind::Fact | PointKind::Point => {
            for (k, len) in [(-1.0f32, 0.75f32), (0.0, 0.6), (1.0, 0.9)] {
                let y = c.1 + k * g * 0.8;
                let pts = [
                    (c.0 - g * len, y + 0.1 * g),
                    (c.0, y - 0.05 * g),
                    (c.0 + g * len, y + 0.05 * g),
                ];
                brush(s, &pts, |u| w * (1.4 - 0.7 * u), INK, 1.0);
            }
        }
    }
}

/// One paper strip (tanzaku) with its top-left corner at (x, y): the kind's
/// colour, a folded top band and the brush mark.
fn strip_shape(s: &mut String, kind: PointKind, x: f32, y: f32, w: f32, h: f32) {
    let (paper, deep) = kind_colors(kind);
    let sw = (w * 0.1).clamp(1.0, 1.7);
    let _ = write!(
        s,
        r#"<rect x="{}" y="{}" width="{}" height="{}" rx="{}" fill="{paper}" stroke="{OL}" stroke-width="{}"/><rect x="{}" y="{}" width="{}" height="{}" fill="{deep}" opacity=".55"/>"#,
        n(x),
        n(y),
        n(w),
        n(h),
        n((w * 0.08).min(1.2)),
        n(sw),
        n(x + sw * 0.5),
        n(y + sw * 0.5),
        n(w - sw),
        n((h * 0.09).max(1.6))
    );
    // A torn little notch at the bottom.
    let _ = write!(
        s,
        r#"<path d="M{} {}l{} {} {} {}" stroke="{OL}" stroke-width="{}" fill="{paper}"/>"#,
        n(x + w * 0.62),
        n(y + h + sw * 0.2),
        n(w * 0.1),
        n(-h * 0.04),
        n(w * 0.1),
        n(h * 0.04),
        n(sw * 0.7)
    );
    let g = (w * 0.34).min(h * 0.2);
    let tall = h > w * 3.0;
    glyph(
        s,
        kind,
        (x + w / 2.0, y + h * if tall { 0.45 } else { 0.5 }),
        g,
    );
    if tall && w >= 10.0 {
        // His red seal at the foot of the strip.
        let k = w * 0.26;
        let _ = write!(
            s,
            r#"<rect x="{}" y="{}" width="{}" height="{}" rx=".6" fill="{EMBER}" opacity=".85"/><path d="M{} {}h{}m{} {}h{}" stroke="{paper}" stroke-width=".7"/>"#,
            n(x + w / 2.0 - k / 2.0),
            n(y + h - k - h * 0.08),
            n(k),
            n(k),
            n(x + w / 2.0 - k * 0.3),
            n(y + h - k * 0.65 - h * 0.08),
            n(k * 0.6),
            n(-k * 0.6),
            n(k * 0.35),
            n(k * 0.6)
        );
    }
}

/// How the strips hang: number of bars, strips per bar, strip size.
struct Hang {
    rows: usize,
    per_row: usize,
    w: f32,
    h: f32,
}

fn hang(count: usize) -> Hang {
    let (x0, y0, x1, y1) = RACK;
    let width = x1 - x0;
    for rows in 1..=MAX_ROWS {
        let row_h = (y1 - y0) / rows as f32;
        let h = (row_h - 12.0).min(64.0);
        let per_row = count.div_ceil(rows).max(1);
        let w = (h * 0.32).min(17.0).min(width / per_row as f32 / 1.3);
        if w >= MIN_STRIP_W && w >= h * 0.18 {
            return Hang {
                rows,
                per_row,
                w,
                h,
            };
        }
    }
    let row_h = (y1 - y0) / MAX_ROWS as f32;
    let h = row_h - 12.0;
    let per_row = (width / (MIN_STRIP_W * 1.3)) as usize;
    Hang {
        rows: MAX_ROWS,
        per_row,
        w: MIN_STRIP_W,
        h,
    }
}

/// Where every shown strip hangs (top-center of its string) and how many
/// of the oldest notes went into the bundle instead.
fn layout(count: usize) -> (Hang, Vec<P>, usize) {
    let hg = hang(count);
    let cap = hg.rows * hg.per_row;
    // When it overflows, one slot stays free so the bundle can lean there.
    let shown = if count > cap { cap - 1 } else { count };
    let bundled = count - shown;
    let (x0, y0, x1, y1) = RACK;
    let row_h = (y1 - y0) / hg.rows as f32;
    let step = ((x1 - x0) / hg.per_row as f32).min(hg.w * 1.7);
    let mut out = Vec::with_capacity(shown);
    for i in 0..shown {
        let (row, col) = (i / hg.per_row, i % hg.per_row);
        let in_row = (shown - row * hg.per_row).min(hg.per_row);
        let span = step * in_row as f32;
        let x = (x0 + x1) / 2.0 - span / 2.0 + step * (col as f32 + 0.5);
        out.push((x, y0 + row as f32 * row_h + 2.0));
    }
    (hg, out, bundled)
}

/// Jin's sword rack: one paper strip per note, in order, hung from the
/// rack's bars (a second and third bar as they pile up, the oldest tied in
/// a bundle past that). The strips sway on their strings over a loop of
/// `NOTES_FRAMES` frames; the newest one still flutters into place. A
/// standalone SVG document of `NOTES_W` × `NOTES_H`.
pub fn notes_svg(notes: &[PointKind], frame: usize) -> String {
    let t = (frame % NOTES_FRAMES) as f32 / NOTES_FRAMES as f32;
    let (w, h) = (NOTES_W, NOTES_H);
    let mut s = String::with_capacity(6144 + notes.len() * 900);
    let _ = write!(
        s,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}"><defs><clipPath id="jpanel"><rect width="{w}" height="{h}" rx="14"/></clipPath></defs><g clip-path="url(#jpanel)" stroke-linecap="round" stroke-linejoin="round"><rect width="{w}" height="{h}" fill="#F4EAD6"/>"##
    );
    // Washi fibres, a red sun and ink-wash mountains far behind.
    for i in 0..22u32 {
        let p = (
            6.0 + hash(i, 201) * (w - 12.0),
            6.0 + hash(i, 202) * (h - 30.0),
        );
        let a = 360.0 * hash(i, 203);
        let q = add(p, dir(a), 4.0 + 6.0 * hash(i, 204));
        let _ = write!(
            s,
            r##"<path d="M{} {}L{} {}" stroke="#D9CBAE" stroke-width=".7" opacity=".7"/>"##,
            n(p.0),
            n(p.1),
            n(q.0),
            n(q.1)
        );
    }
    s.push_str(r##"<circle cx="186" cy="52" r="20" fill="#E8836A" opacity=".28"/><path d="M-5 132L30 104L52 118L84 92L118 124L150 110L180 126L214 100L245 122V165H-5Z" fill="#2B1730" opacity=".07"/><path d="M-5 140L40 122L76 134L120 118L168 134L210 120L245 132V165H-5Z" fill="#2B1730" opacity=".07"/>"##);
    // The floor.
    let _ = write!(
        s,
        r##"<rect x="0" y="148" width="{w}" height="14" fill="#C9A37A"/><path d="M0 148H{w}" stroke="{OL}" stroke-width="1.6"/><path d="M0 154H{w}" stroke="#B58C62" stroke-width=".8"/>"##
    );
    let (hg, pts, bundled) = layout(notes.len());
    rack(&mut s, hg.rows);
    if notes.is_empty() {
        // Nothing noted yet: only his kasa hangs on the rack.
        s.push_str(&kasa_hung((120.0, RACK.1 + 2.0), 3.0 * wave(t, 1.0, 0.0)));
    }
    let shown = &notes[bundled..];
    let newest = shown.len().saturating_sub(1);
    let gust = wave(t, 1.0, 0.0);
    for (i, (&kind, &top)) in shown.iter().zip(&pts).enumerate() {
        let ph = hash(i as u32, 7);
        let mut rot = 1.6 * wave(t, 1.0, ph) + 0.8 * gust;
        let mut lift = 0.0;
        if i == newest {
            // Fresh off the blade: still fluttering on its string.
            rot = 9.0 * wave(t, 2.0, 0.0) + 3.0 * wave(t, 3.0, 0.3);
            lift = 1.2 * (0.5 - 0.5 * (TAU * 2.0 * t).cos());
        }
        let string = 4.0;
        let _ = write!(
            s,
            r#"<path d="M{} {}V{}" stroke="{OL}" stroke-width="1"/><g transform="translate({} {}) rotate({})">"#,
            n(top.0),
            n(top.1),
            n(top.1 + string - lift),
            n(top.0),
            n(top.1 + string - lift),
            n(rot)
        );
        let _ = write!(s, r#"<path d="M0 -.5V2" stroke="{OL}" stroke-width="1"/>"#);
        strip_shape(&mut s, kind, -hg.w / 2.0, 1.5, hg.w, hg.h);
        s.push_str("</g>");
        if i == newest {
            // Two wisps of wind and a drop of fresh ink falling off it.
            let k = (TAU * t).sin().abs();
            let side = top.0 + hg.w * 0.9 + 3.0;
            let _ = write!(
                s,
                r#"<path d="M{} {}q3-2 6 0M{} {}q3-2 5 0" stroke="{OL}" stroke-width="1.1" fill="none" opacity="{:.2}"/>"#,
                n(side),
                n(top.1 + hg.h * 0.3),
                n(side + 2.0),
                n(top.1 + hg.h * 0.55),
                0.25 + 0.5 * k
            );
            let u = t;
            let y = top.1 + hg.h + 6.0 + 18.0 * u * u;
            if y < 146.0 {
                let _ = write!(
                    s,
                    r#"<path d="{}" fill="{INK}" opacity="{:.2}"/>"#,
                    drop_d((top.0, y), 1.3),
                    1.0 - u
                );
            }
        }
    }
    if bundled > 0 {
        bundle(
            &mut s,
            notes[..bundled].iter().rev().take(4).copied().collect(),
            bundled,
        );
    }
    s.push_str("</g></svg>");
    s
}

/// The rack: two posts on feet, an upturned top beam and one bar per row.
fn rack(s: &mut String, rows: usize) {
    let wood = "#8A5A3C";
    let wood_hi = "#B07B52";
    let (x0, y0, x1, y1) = RACK;
    for x in [x0 - 12.0, x1 + 12.0] {
        let _ = write!(
            s,
            r#"<rect x="{}" y="14" width="7" height="134" fill="{wood}" stroke="{OL}" stroke-width="1.6"/><path d="M{} 18V146" stroke="{wood_hi}" stroke-width="1.2"/><path d="M{} 148Q{} 141 {} 141L{} 141Q{} 141 {} 148Z" fill="{wood}" stroke="{OL}" stroke-width="1.6"/>"#,
            n(x - 3.5),
            n(x - 1.6),
            n(x - 15.0),
            n(x - 13.0),
            n(x - 6.0),
            n(x + 6.0),
            n(x + 13.0),
            n(x + 15.0)
        );
    }
    let _ = write!(
        s,
        r#"<path d="M6 9Q14 14 30 13L210 13Q226 14 234 9L232 18Q220 20 210 19L30 19Q20 20 8 18Z" fill="{SAYA}" stroke="{OL}" stroke-width="1.8"/><path d="M30 15.2H210" stroke="{SAYA_HI}" stroke-width="1.2"/>"#
    );
    let row_h = (y1 - y0) / rows as f32;
    for r in 0..rows {
        let y = y0 + r as f32 * row_h + 2.0;
        let _ = write!(
            s,
            r#"<path d="M{} {}H{}" stroke="{OL}" stroke-width="5"/><path d="M{} {}H{}" stroke="{wood}" stroke-width="2.6"/><path d="M{} {}H{}" stroke="{wood_hi}" stroke-width=".9"/>"#,
            n(x0 - 15.0),
            n(y),
            n(x1 + 15.0),
            n(x0 - 15.0),
            n(y),
            n(x1 + 15.0),
            n(x0 - 14.0),
            n(y - 0.6),
            n(x1 + 14.0)
        );
    }
}

/// His kasa hanging from the top beam by its cord, swaying (`rot`).
fn kasa_hung(c: P, rot: f32) -> String {
    let mut s = String::new();
    let (x, y) = c;
    let _ = write!(
        s,
        r#"<g transform="rotate({} {} {})"><path d="M{} {}L{} {}L{} {}" stroke="{OL}" stroke-width="2.4" fill="none"/><path d="M{} {}L{} {}L{} {}" stroke="{EMBER}" stroke-width="1.1" fill="none"/>"#,
        n(rot),
        n(x),
        n(y),
        n(x - 18.0),
        n(y + 46.0),
        n(x),
        n(y),
        n(x + 18.0),
        n(y + 46.0),
        n(x - 18.0),
        n(y + 46.0),
        n(x),
        n(y),
        n(x + 18.0),
        n(y + 46.0)
    );
    let _ = write!(
        s,
        r#"<path d="M{} {}Q{} {} {} {}L{} {}Q{} {} {} {}Z" fill="{STRAW}" stroke="{OL}" stroke-width="1.8"/>"#,
        n(x - 36.0),
        n(y + 62.0),
        n(x),
        n(y + 76.0),
        n(x + 36.0),
        n(y + 62.0),
        n(x + 1.5),
        n(y + 26.0),
        n(x),
        n(y + 24.5),
        n(x - 1.5),
        n(y + 26.0)
    );
    let mut d = String::new();
    for k in [-26.0f32, -14.0, 0.0, 14.0, 26.0] {
        let _ = write!(
            d,
            "M{} {}L{} {}",
            n(x),
            n(y + 27.0),
            n(x + k),
            n(y + 63.0 + 6.0 * (1.0 - (k / 36.0).powi(2)))
        );
    }
    let _ = write!(
        s,
        r#"<path d="{d}" stroke="{STRAW_SH}" stroke-width="1"/><path d="M{} {}Q{} {} {} {}" stroke="{STRAW_SH}" stroke-width="1.4" fill="none"/><path d="M{} {}Q{} {} {} {}" stroke="{STRAW_HI}" stroke-width="2.2" fill="none"/></g>"#,
        n(x - 24.0),
        n(y + 52.0),
        n(x),
        n(y + 61.0),
        n(x + 24.0),
        n(y + 52.0),
        n(x - 6.0),
        n(y + 34.0),
        n(x - 13.0),
        n(y + 45.0),
        n(x - 22.0),
        n(y + 54.0)
    );
    s
}

/// The oldest strips, tied in a bundle with a red cord and leaning against
/// the right post; a knot of the cord for every ten notes in it.
fn bundle(s: &mut String, kinds: Vec<PointKind>, count: usize) {
    let (x, y) = (RACK.2 - 4.0, 104.0);
    let _ = write!(
        s,
        r#"<g transform="translate({} {}) rotate(12)">"#,
        n(x),
        n(y)
    );
    for (k, &kind) in kinds.iter().enumerate().rev() {
        let dx = -6.0 + 4.0 * k as f32;
        let _ = write!(s, r#"<g transform="rotate({})">"#, n(-8.0 + 5.0 * k as f32));
        strip_shape(s, kind, dx - 4.0, -4.0, 9.0, 44.0);
        s.push_str("</g>");
    }
    let _ = write!(
        s,
        r#"<path d="M-9 20H11" stroke="{OL}" stroke-width="3.6"/><path d="M-9 20H11" stroke="{EMBER}" stroke-width="2"/>"#
    );
    for k in 0..count.div_ceil(10).min(4) {
        let _ = write!(
            s,
            r#"<circle cx="{}" cy="23.5" r="1.6" fill="{EMBER}" stroke="{OL}" stroke-width=".9"/>"#,
            n(-6.0 + 5.0 * k as f32)
        );
    }
    s.push_str("</g>");
}

/// Frames of `note_pop_svg`, played once (not looped).
pub const POP_FRAMES: usize = 12;
/// Size of the `note_pop_svg` square (its viewBox).
pub const POP_SIZE: f32 = 48.0;

/// A note arriving: an ink slash cuts across a `POP_SIZE` square, splits
/// with a flash, and a paper strip of the note's kind unfurls out of the
/// cut and settles. Frames past the last show the last.
pub fn note_pop_svg(kind: PointKind, frame: usize) -> String {
    let f = frame.min(POP_FRAMES - 1);
    let mut s = String::with_capacity(3072);
    let _ = write!(
        s,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {w}" width="{w}" height="{w}"><g stroke-linecap="round" stroke-linejoin="round">"#,
        w = POP_SIZE
    );
    let (a, b) = ((7.0, 9.0), (41.0, 39.0));
    let cut = |u: f32| -> Vec<P> {
        (0..=10)
            .map(|k| {
                let v = u * k as f32 / 10.0;
                let p = lerp(a, b, v);
                (p.0 - 5.0 * (PI * v).sin(), p.1 + 3.0 * (PI * v).sin())
            })
            .collect()
    };
    if f < 4 {
        // The slash sweeping across.
        let u = (f as f32 + 1.0) / 4.0;
        brush(
            &mut s,
            &cut(u),
            |v| 0.8 + 6.0 * v.powf(0.7) * (1.0 - 0.6 * v * u),
            INK,
            1.0,
        );
    } else if f < 8 {
        // The slash splits along its middle and the halves drift apart.
        let k = (f - 4) as f32 / 4.0;
        let pts = cut(1.0);
        let nrm = (0.6, -0.8);
        for side in [-1.0f32, 1.0] {
            let moved: Vec<P> = pts
                .iter()
                .map(|&p| add(p, nrm, side * (2.0 + 9.0 * k)))
                .collect();
            brush(
                &mut s,
                &moved,
                |v| (0.8 + 3.0 * (PI * v).sin()) * (1.0 - k),
                INK,
                1.0 - k,
            );
        }
        // Flash along the cut.
        let _ = write!(
            s,
            r#"<path d="{}" stroke="white" stroke-width="{}" fill="none" opacity="{:.2}"/>"#,
            {
                let mut d = D::new().m(pts[0]);
                for &p in &pts[1..] {
                    d = d.l(p);
                }
                d.s()
            },
            n(2.4 * (1.0 - k) + 0.4),
            1.0 - k
        );
        for k2 in 0..4 {
            let v = dir(-30.0 + 90.0 * k2 as f32);
            let p = add((24.0, 24.0), v, 10.0 + 10.0 * k);
            let _ = write!(
                s,
                r#"<circle cx="{}" cy="{}" r="{}" fill="{INK}" opacity="{:.2}"/>"#,
                n(p.0),
                n(p.1),
                n(1.6 * (1.0 - k) + 0.3),
                1.0 - k
            );
        }
    }
    if f >= 4 {
        // The strip unfurls out of the cut, with a little overshoot.
        let k = (f - 4) as f32 / (POP_FRAMES - 5) as f32;
        let c = 1.7;
        let e = 1.0 + (c + 1.0) * (k - 1.0).powi(3) + c * (k - 1.0).powi(2);
        let (sw, sh) = (15.0, 36.0 * e.max(0.05));
        let rot = -38.0 * (1.0 - k).powi(2) + 4.0 * (PI * k).sin();
        let _ = write!(s, r#"<g transform="translate(24 24) rotate({})">"#, n(rot));
        strip_shape(&mut s, kind, -sw / 2.0, -sh / 2.0, sw, sh);
        s.push_str("</g>");
    }
    s.push_str("</g></svg>");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [PointKind; 5] = PointKind::ALL;

    #[test]
    fn every_frame_is_a_small_svg_without_text() {
        for pose in Pose::ALL {
            for f in 0..frame_count(pose) {
                let svg = frame_svg(pose, f);
                assert!(svg.starts_with("<svg") && svg.ends_with("</svg>"));
                assert!(!svg.contains("<text") && !svg.contains("filter"));
                assert!(!svg.contains("NaN") && !svg.contains("inf"), "{pose:?} {f}");
                assert!(svg.len() < 30_000, "{pose:?} {f}: {} bytes", svg.len());
            }
        }
    }

    #[test]
    fn poses_loop() {
        for pose in Pose::ALL {
            let count = frame_count(pose);
            assert_eq!(frame_svg(pose, 0), frame_svg(pose, count));
            assert_ne!(
                frame_svg(pose, 0),
                frame_svg(pose, count / 2),
                "{pose:?} doesn't move"
            );
        }
    }

    #[test]
    fn the_strokes_close_their_loop() {
        let (p0, a0) = stroke_at(0.0);
        let (p1, a1) = stroke_at(16.0 - 1e-4);
        assert!((p0.0 - p1.0).abs() < 0.01 && (p0.1 - p1.1).abs() < 0.01);
        assert!(((a1 - a0) - 360.0).abs() < 0.01);
    }

    fn mixed(count: usize) -> Vec<PointKind> {
        (0..count)
            .map(|i| KINDS[(i * 7 + i / 3) % KINDS.len()])
            .collect()
    }

    #[test]
    fn the_rack_draws_any_number_of_notes() {
        for count in [0, 1, 2, 5, 6, 20, 47, 120] {
            let notes = mixed(count);
            for f in 0..NOTES_FRAMES {
                let svg = notes_svg(&notes, f);
                assert!(svg.starts_with("<svg") && svg.ends_with("</svg>"));
                assert!(!svg.contains("<text") && !svg.contains("NaN"));
                assert!(svg.len() < 200_000, "{count}: {} bytes", svg.len());
            }
            assert_eq!(notes_svg(&notes, 0), notes_svg(&notes, NOTES_FRAMES));
        }
    }

    #[test]
    fn strips_stay_inside_the_frame_and_apart() {
        for count in [1, 5, 20, 47, 120] {
            let (hg, pts, bundled) = layout(count);
            assert_eq!(pts.len() + bundled, count);
            assert!(hg.w >= MIN_STRIP_W);
            for (i, a) in pts.iter().enumerate() {
                assert!(
                    a.0 - hg.w / 2.0 > 0.0 && a.0 + hg.w / 2.0 < NOTES_W,
                    "{count}: strip {i} at {a:?}"
                );
                assert!(
                    a.1 > 0.0 && a.1 + hg.h + 8.0 < 148.0,
                    "{count}: strip {i} hangs to {}",
                    a.1 + hg.h
                );
                for b in &pts[..i] {
                    if (a.1 - b.1).abs() < 1.0 {
                        assert!(
                            (a.0 - b.0).abs() > hg.w * 1.1,
                            "{count}: strips {a:?} and {b:?} overlap"
                        );
                    }
                }
            }
        }
        let (_, one, _) = layout(1);
        assert!((one[0].0 - (RACK.0 + RACK.2) / 2.0).abs() < 1.0);
        assert_eq!(layout(5).2, 0);
        assert_eq!(layout(20).2, 0);
    }

    #[test]
    fn a_strip_pops_for_every_kind() {
        for kind in KINDS {
            for f in 0..POP_FRAMES {
                let svg = note_pop_svg(kind, f);
                assert!(svg.starts_with("<svg") && svg.ends_with("</svg>"));
                assert!(!svg.contains("<text") && !svg.contains("NaN"));
            }
            assert_eq!(
                note_pop_svg(kind, POP_FRAMES - 1),
                note_pop_svg(kind, POP_FRAMES + 3)
            );
            assert_ne!(note_pop_svg(kind, 0), note_pop_svg(kind, POP_FRAMES - 1));
        }
    }
}
