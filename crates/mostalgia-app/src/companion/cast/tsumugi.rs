//! Tsumugi's artwork: a tiny weaver with a drop spindle. She spins what is
//! said into silk thread and weaves the meeting's notes into a scarf, which
//! is the meeting's summary; when the recording pauses she wraps herself in
//! it and naps. Every frame is generated as an SVG document (transparent
//! background, viewBox FRAME_W × FRAME_H), the same way as Mori's.
//!
//! She is drawn as a small rig: the body parts are fixed shapes, and each
//! pose only decides where the joints go for a phase `t` in `0..1` (head
//! tilt, eye state, where her hands, the spindle and the scarf ends are; the
//! arms follow by two-bone IK). All motion is built from `sin`/`cos` of
//! `2πt`, so every pose loops seamlessly. No `<text>` and no filters: frames
//! must render the same in resvg and GPUI without fonts.
//!
//! Her notes are not cards but bands of the scarf (`notes_svg`): one woven
//! band per point, its colour and its motif telling the kind (diamonds for
//! a decision, chevrons for an action, rings for a question, blocks for a
//! fact), the newest at the bottom where the shuttle is still going.

use std::f32::consts::{PI, TAU};
use std::fmt::{self, Write as _};

use super::PointKind;

/// What Tsumugi is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pose {
    /// Breathing, blinking, spindle at her side.
    Idle,
    /// Someone started speaking: looks up, the hanging spindle at rest.
    Listening,
    /// Speech going on: the spindle spins, words drift in and become thread.
    Writing,
    /// Waiting for the language model: picking at a knot.
    Thinking,
    /// Showing an answer or a point: holds up the end of her scarf.
    Talking,
    /// Recording paused: wrapped up in her scarf, napping.
    Sleeping,
    /// The provider failed: tangled in her own thread.
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

const OL: &str = "#2B1730"; // Night Plum: outlines
const SKIN: &str = "#FFE7D6";
const SKIN_SH: &str = "#F4C5AE";
const HAIR: &str = "#6E3A2C"; // Chestnut
const HAIR_HI: &str = "#A8604A";
const HAIR_SH: &str = "#542A20";
const CREAM: &str = "#FBF3E4";
const CREAM_DEEP: &str = "#F0DFC2";
const SAGE: &str = "#9DB8A0";
const SAGE_SH: &str = "#7F9E84";
const SAGE_DEEP: &str = "#3E7A52";
const PLUM: &str = "#5B2A4E";
const PLUM_HI: &str = "#9A5C88";
const TERRA: &str = "#D9653B";
const EMBER: &str = "#B8472A";
const AMBER: &str = "#F2A541";
const AMBER_L: &str = "#F7C26B";
const WOOD: &str = "#C98B5A";
const WOOD_SH: &str = "#9A6239";
const MOUTH: &str = "#8A3444";
const TONGUE: &str = "#E9877A";
const BLUSH: &str = "#F2876F";
const BROW: &str = "#4A2420";
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
    let a = ((l1 * l1 + d * d - l2 * l2) / (2.0 * l1 * d))
        .clamp(-1.0, 1.0)
        .acos();
    let ang = base + bend * a;
    let elbow = (s.0 + l1 * ang.cos(), s.1 + l1 * ang.sin());
    let hand = (s.0 + dx / dist * d, s.1 + dy / dist * d);
    (elbow, hand)
}

/// Point on the quadratic Bézier `a`–`c`–`b` at `k`.
fn quad(a: P, c: P, b: P, k: f32) -> P {
    let ac = lerp(a, c, k);
    let cb = lerp(c, b, k);
    lerp(ac, cb, k)
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

/// What her hands are busy with.
#[derive(Clone, Copy)]
enum Prop {
    /// The spindle held upright in her right hand (viewer's left).
    Held,
    /// The spindle hanging on its thread from her raised left hand: `hook`
    /// is where the thread meets it, `spin` its turns so far, `fluff` the
    /// phase of the silk she drafts from the puff in her other hand.
    Drop {
        hook: P,
        spin: f32,
        fluff: Option<f32>,
    },
    /// A knot held up between both hands.
    Knot(f32),
    /// Thread tangled all around her, the spindle swinging.
    Tangle(f32),
    /// Wrapped in the scarf: the spindle lies on the floor.
    Floor,
}

#[derive(Clone, Copy)]
enum Extra {
    None,
    Listen(f32),
    Words(f32),
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
    /// Swing of the thread bow on her bun (degrees).
    swing: f32,
    /// Where the free ends of the scarf are (her right, her left).
    scarf_r: P,
    scarf_l: P,
    /// The front end is held up in her hand (drawn over the arm).
    scarf_held: bool,
    /// Hands: her right (viewer's left) and her left, with the elbow side.
    hand_r: P,
    bend_r: f32,
    hand_l: P,
    bend_l: f32,
    prop: Prop,
    extra: Extra,
}

const SHOULDER_R: P = (80.0, 131.0);
const SHOULDER_L: P = (120.0, 131.0);
const UPPER_ARM: f32 = 25.0;
const FOREARM: f32 = 24.0;
/// The head is drawn at a larger native scale, then scaled around the chin.
const HEAD_SCALE: f32 = 0.92;
const NECK: P = (100.0, 112.0);
/// Where the scarf ends leave the loop around her neck.
const SCARF_ROOT_R: P = (91.0, 127.0);
const SCARF_ROOT_L: P = (114.0, 121.0);
/// The spindle: thread hook to tip, and where its whorl sits.
const SPINDLE_LEN: f32 = 42.0;
const WHORL_AT: f32 = 33.0;

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
        swing: 0.0,
        scarf_r: (84.0, 178.0),
        scarf_l: (136.0, 168.0),
        scarf_held: false,
        hand_r: (69.0, 178.0),
        bend_r: 1.0,
        hand_l: (131.0, 177.0),
        bend_l: -1.0,
        prop: Prop::Held,
        extra: Extra::None,
    }
}

fn rig(pose: Pose, frame: usize, t: f32) -> Rig {
    let mut r = base();
    match pose {
        Pose::Idle => {
            r.sway = 0.8 * wave(t, 1.0, 0.0);
            r.bob = -0.9 * (0.5 - 0.5 * (TAU * 2.0 * t).cos());
            r.head_rot = 1.8 * wave(t, 1.0, 0.1);
            r.swing = 6.0 * wave(t, 1.0, 0.3);
            r.lid = match frame {
                14 => 0.55,
                15 => 1.0,
                16 => 0.35,
                _ => 0.0,
            };
            r.scarf_r.0 += 1.2 * wave(t, 1.0, 0.2);
            r.scarf_l.0 += 1.0 * wave(t, 1.0, 0.35);
            r.hand_r.1 += 0.8 * wave(t, 1.0, 0.0);
        }
        Pose::Listening => {
            let nod = wave(t, 2.0, 0.0);
            r.head_rot = 7.0 + 1.2 * wave(t, 1.0, 0.0);
            r.head_dy = -1.0 + 0.9 * nod;
            r.bob = -0.5 * (0.5 - 0.5 * (TAU * t).cos());
            r.eyes = Eyes::Wide;
            r.look = (1.4, -1.6);
            r.brow = Brow::Raised;
            r.mouth = Mouth::O(1.6 + 0.2 * wave(t, 2.0, 0.0));
            r.swing = 8.0 * wave(t, 2.0, 0.1);
            // Thread held up, the spindle at rest, swaying a little.
            r.hand_l = (146.0, 104.0 + 0.6 * nod);
            r.bend_l = 1.0;
            let sw = 3.0 * wave(t, 1.0, 0.0);
            r.prop = Prop::Drop {
                hook: (146.0 + sw, 152.0),
                spin: 0.1 * wave(t, 1.0, 0.0),
                fluff: None,
            };
            r.hand_r = (70.0, 176.0);
            r.extra = Extra::Listen(t);
        }
        Pose::Writing => {
            r.head_rot = 4.0 + 1.0 * wave(t, 1.0, 0.0);
            r.head_dy = 2.0;
            r.head_dx = 1.0;
            r.look = (1.8, 1.6);
            r.lid = 0.3;
            r.swing = 5.0 * wave(t, 2.0, 0.0);
            r.bob = -0.5 * (0.5 - 0.5 * (TAU * t).cos());
            // Fingers rolling the thread, the spindle spinning below.
            r.hand_l = (
                145.0 + 1.2 * wave(t, 2.0, 0.0),
                102.0 + 1.0 * wave(t, 2.0, 0.25),
            );
            r.bend_l = 1.0;
            r.prop = Prop::Drop {
                hook: (
                    147.0 + 0.8 * wave(t, 2.0, 0.1),
                    152.0 + 1.2 * wave(t, 1.0, 0.0),
                ),
                spin: 3.0 * t,
                fluff: Some(t),
            };
            r.hand_r = (104.0, 160.0 + 1.0 * wave(t, 2.0, 0.4));
            r.bend_r = 1.0;
            r.extra = Extra::Words(t);
        }
        Pose::Thinking => {
            r.head_rot = -6.0 + 1.2 * wave(t, 1.0, 0.0);
            r.head_dx = -0.5;
            r.head_dy = 1.0;
            r.look = (0.4, 2.4);
            r.lid = 0.2;
            r.brow = Brow::Raised;
            r.mouth = Mouth::Hmm;
            r.swing = -4.0 + 4.0 * wave(t, 1.0, 0.0);
            r.bob = -0.6 * (0.5 - 0.5 * (TAU * t).cos());
            // Both hands at the knot, tugging in turn.
            let tug = wave(t, 2.0, 0.0);
            r.hand_r = (84.0 - 1.5 * tug, 151.0 + 1.0 * tug);
            r.hand_l = (117.0 - 1.5 * tug, 149.0 - 1.0 * tug);
            r.prop = Prop::Knot(t);
            r.extra = Extra::Bubble(t);
        }
        Pose::Talking => {
            let syl = (TAU * 2.0 * t).sin().abs();
            r.head_rot = 2.0 * wave(t, 1.0, 0.0);
            r.head_dy = -0.6 * syl;
            r.bob = -0.6 * (0.5 - 0.5 * (TAU * t).cos());
            r.mouth = if syl > 0.2 {
                Mouth::Open(3.4 * syl)
            } else {
                Mouth::Smile
            };
            r.eyes = if (6..=7).contains(&frame) {
                Eyes::Happy
            } else {
                Eyes::Open
            };
            r.brow = Brow::Raised;
            r.blush = 0.55;
            r.swing = 5.0 * wave(t, 2.0, 0.2);
            // Holding up the end of the scarf: here is what was woven.
            let g = wave(t, 1.0, 0.0);
            r.hand_r = (54.0 - 2.0 * g, 134.0 - 3.0 * syl);
            r.bend_r = 1.0;
            r.scarf_r = (r.hand_r.0 - 3.0, r.hand_r.1 + 22.0);
            r.scarf_held = true;
            r.scarf_l.0 += 2.0 * wave(t, 1.0, 0.3);
        }
        Pose::Sleeping => {
            let s = wave(t, 1.0, 0.0);
            r.head_rot = 9.0 + 2.5 * s;
            r.head_dy = 4.0 + 1.5 * s;
            r.head_dx = 1.0;
            r.bob = -1.2 * (0.5 - 0.5 * (TAU * t).cos());
            r.eyes = Eyes::Closed;
            r.brow = Brow::Sleepy;
            r.mouth = Mouth::O(1.1 + 0.3 * s);
            r.blush = 0.6;
            r.swing = 14.0 + 3.0 * s;
            r.prop = Prop::Floor;
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
            r.swing = 14.0 * wave(t, 3.0, 0.3);
            // Hands up, caught in the thread.
            r.hand_r = (62.0, 122.0 + 1.5 * wave(t, 3.0, 0.0));
            r.bend_r = -1.0;
            r.hand_l = (138.0, 120.0 + 1.5 * wave(t, 3.0, 0.5));
            r.bend_l = 1.0;
            r.prop = Prop::Tangle(t);
            r.extra = Extra::Sweat(t);
        }
    }
    r
}

// ---------------------------------------------------------------- drawing

fn draw(r: &Rig) -> String {
    let mut s = String::with_capacity(20 * 1024);
    let _ = write!(
        s,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}"><defs><linearGradient id="tir" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#4A2216"/><stop offset=".55" stop-color="#9A4E22"/><stop offset="1" stop-color="#F2A541"/></linearGradient><linearGradient id="thg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#8A4A36"/><stop offset="1" stop-color="{HAIR}"/></linearGradient></defs>"##,
        w = FRAME_W,
        h = FRAME_H
    );
    // Ground shadow (not swayed).
    s.push_str(r##"<ellipse cx="100" cy="300" rx="30" ry="4" fill="#2B1730" opacity=".13"/>"##);
    if let Prop::Floor = r.prop {
        // Dropped at her feet, its thread still running up into the wrap.
        thread(&mut s, (118.0, 240.0), (150.0, 268.0), (172.0, 292.0), None);
        spindle(&mut s, (172.0, 292.0), 184.0, 0.15);
    }
    let _ = write!(
        s,
        r#"<g transform="translate({} 0) rotate({} 100 298)" stroke-linejoin="round" stroke-linecap="round">"#,
        n(r.shake),
        n(r.sway)
    );
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
    if let Prop::Floor = r.prop {
        // Wrapped up: no arms, the scarf is a cocoon from chin to knees.
        neck(&mut s);
        let _ = write!(s, r#"<g transform="{head_tf}">"#);
        head(&mut s, r);
        s.push_str("</g>");
        cocoon(&mut s, r);
        s.push_str("</g>");
        extras(&mut s, r);
        s.push_str("</g></svg>");
        return s;
    }
    scarf_back(&mut s, r);
    torso(&mut s);
    if !r.scarf_held {
        scarf(&mut s, r);
    }
    let (er, hr) = ik(SHOULDER_R, r.hand_r, UPPER_ARM, FOREARM, r.bend_r);
    let (el, hl) = ik(SHOULDER_L, r.hand_l, UPPER_ARM, FOREARM, r.bend_l);
    arm(&mut s, SHOULDER_R, er, hr);
    arm(&mut s, SHOULDER_L, el, hl);
    if r.scarf_held {
        // Held up in front of her arm.
        scarf(&mut s, r);
    }
    let _ = write!(s, r#"<g transform="{head_tf}">"#);
    head(&mut s, r);
    s.push_str("</g>");
    props(&mut s, r, hr, hl);
    hand(&mut s, hr, true);
    hand(&mut s, hl, !matches!(r.prop, Prop::Tangle(_)));
    s.push_str("</g>");
    extras(&mut s, r);
    s.push_str("</g></svg>");
    s
}

fn legs(s: &mut String) {
    for (hip, ankle) in [
        ((94.0, 222.0), (93.0, 286.0)),
        ((106.0, 222.0), (107.0, 286.0)),
    ] {
        let _ = write!(
            s,
            r#"<path d="M{} {}L{} {}" stroke="{OL}" stroke-width="12" fill="none"/><path d="M{} {}L{} {}" stroke="{SKIN}" stroke-width="8.6" fill="none"/>"#,
            n(hip.0),
            n(hip.1),
            n(ankle.0),
            n(ankle.1),
            n(hip.0),
            n(hip.1),
            n(ankle.0),
            n(ankle.1)
        );
        // Ankle socks.
        let (ax, ay) = ankle;
        let _ = write!(
            s,
            r#"<path d="M{} {}L{} {}" stroke="{OL}" stroke-width="12" fill="none"/><path d="M{} {}L{} {}" stroke="{CREAM}" stroke-width="8.6" fill="none"/>"#,
            n(ax),
            n(ay - 6.0),
            n(ax),
            n(ay + 3.0),
            n(ax),
            n(ay - 6.0),
            n(ax),
            n(ay + 3.0)
        );
    }
    // Little wooden clogs with a plum strap, toes slightly outward.
    for (cx, m) in [(91.5, -1.0), (108.5, 1.0)] {
        let x = |dx: f32| n(cx + m * dx);
        let _ = write!(
            s,
            r#"<path d="M{} 293.5H{}V297.5H{}Z" fill="{WOOD_SH}" stroke="{OL}" stroke-width="1.4"/><path d="M{} 293.5H{}V297.5H{}Z" fill="{WOOD_SH}" stroke="{OL}" stroke-width="1.4"/><rect x="{}" y="290" width="17" height="4.4" rx="2" fill="{WOOD}" stroke="{OL}" stroke-width="1.6"/><path d="M{} 287.5Q{} 291.5 {} 291M{} 287.5Q{} 291.5 {} 291" stroke="{PLUM}" stroke-width="2" fill="none"/>"#,
            x(-6.5),
            x(-3.0),
            x(-6.5),
            x(3.0),
            x(6.5),
            x(3.0),
            n(cx - 8.5),
            x(0.0),
            x(-3.0),
            x(-6.0),
            x(0.0),
            x(3.0),
            x(6.0)
        );
    }
}

/// Neck with the chin's shadow.
fn neck(s: &mut String) {
    let _ = write!(
        s,
        r#"<path d="M95 100L95 123L105 123L105 100Z" fill="{SKIN}" stroke="{OL}" stroke-width="1.8"/><path d="M95.9 106Q100 112 104.1 106L104.1 110Q100 114 95.9 110Z" fill="{SKIN_SH}"/>"#
    );
}

fn torso(s: &mut String) {
    neck(s);
    // Pleated plum skirt with a cream hem stripe.
    let _ = write!(
        s,
        r#"<path d="M81 166L119 166C125 186 133 206 138 226Q100 234 62 226C67 206 75 186 81 166Z" fill="{PLUM}" stroke="{OL}" stroke-width="2"/><path d="M65 218Q100 225.5 135 218" stroke="{CREAM_DEEP}" stroke-width="2.2" fill="none"/><path d="M90 172L84 228M100 172V231M110 172L116 228M81 175L72 224M119 175L128 224" stroke="{PLUM_HI}" stroke-width="1.3" fill="none" opacity=".8"/>"#
    );
    // Sage kimono top with a crossed cream collar.
    let _ = write!(
        s,
        r#"<path d="M88 119L112 119C119 122 124 128 125 138C125 150 122 162 121 172L79 172C78 162 75 150 75 138C76 128 81 122 88 119Z" fill="{SAGE}" stroke="{OL}" stroke-width="2"/><path d="M84 140Q83 155 84 168M116 140Q117 155 116 168" stroke="{SAGE_SH}" stroke-width="1.3" fill="none"/>"#
    );
    for d in ["M107.5 120L99 141", "M92.5 120L103 147"] {
        let _ = write!(
            s,
            r#"<path d="{d}" stroke="{OL}" stroke-width="6.6" fill="none"/><path d="{d}" stroke="{CREAM}" stroke-width="3.8" fill="none"/>"#
        );
    }
    // Wide terracotta sash with an amber cord and its knot.
    let _ = write!(
        s,
        r#"<path d="M77.5 149Q100 153 122.5 149L121.5 169Q100 173 78.5 169Z" fill="{TERRA}" stroke="{OL}" stroke-width="2"/><path d="M78 159.5Q100 163.5 122 159.5" stroke="{OL}" stroke-width="3.6" fill="none"/><path d="M78 159.5Q100 163.5 122 159.5" stroke="{AMBER}" stroke-width="1.8" fill="none"/><path d="M110 162.5l-2.5 9M113 162l1 9.5" stroke="{OL}" stroke-width="3.4"/><path d="M110 162.5l-2.5 9M113 162l1 9.5" stroke="{AMBER}" stroke-width="1.6"/><circle cx="111.5" cy="162" r="2.8" fill="{AMBER}" stroke="{OL}" stroke-width="1.3"/>"#
    );
}

/// The scarf colours, repeating along a strand: cream with woven bands.
const SCARF_BANDS: [&str; 4] = [SAGE, AMBER, TERRA, PLUM_HI];

/// One loose end of the scarf, from `a` to `b` bowing towards `c`.
fn scarf_end(s: &mut String, a: P, c: P, b: P, w: f32, offset: usize) {
    let steps = 10;
    let mut left = Vec::with_capacity(steps + 1);
    let mut right = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let k = i as f32 / steps as f32;
        let p = quad(a, c, b, k);
        let q = quad(a, c, b, (k + 0.02).min(1.0));
        let q0 = quad(a, c, b, (k - 0.02).max(0.0));
        let (dx, dy) = (q.0 - q0.0, q.1 - q0.1);
        let l = (dx * dx + dy * dy).sqrt().max(0.001);
        let nrm = (-dy / l, dx / l);
        let hw = w / 2.0 * (1.0 - 0.08 * k);
        left.push((p.0 + nrm.0 * hw, p.1 + nrm.1 * hw));
        right.push((p.0 - nrm.0 * hw, p.1 - nrm.1 * hw));
    }
    let mut d = String::new();
    for (i, p) in left.iter().enumerate() {
        let _ = write!(d, "{}{} {}", if i == 0 { "M" } else { "L" }, n(p.0), n(p.1));
    }
    for p in right.iter().rev() {
        let _ = write!(d, "L{} {}", n(p.0), n(p.1));
    }
    d.push('Z');
    // Fringe past the end.
    let e = steps;
    let (tx, ty) = {
        let (dx, dy) = (b.0 - quad(a, c, b, 0.9).0, b.1 - quad(a, c, b, 0.9).1);
        let l = (dx * dx + dy * dy).sqrt().max(0.001);
        (dx / l, dy / l)
    };
    for k in 0..4 {
        let p = lerp(left[e], right[e], (k as f32 + 0.5) / 4.0);
        let _ = write!(
            s,
            r#"<path d="M{} {}l{} {}" stroke="{OL}" stroke-width="3.2"/><path d="M{} {}l{} {}" stroke="{CREAM_DEEP}" stroke-width="1.4"/>"#,
            n(p.0),
            n(p.1),
            n(tx * 5.0),
            n(ty * 5.0),
            n(p.0),
            n(p.1),
            n(tx * 5.0),
            n(ty * 5.0)
        );
    }
    let _ = write!(
        s,
        r#"<path d="{d}" fill="{CREAM}" stroke="{OL}" stroke-width="1.8"/>"#
    );
    // Woven bands across the strand.
    for (j, k) in [0.3f32, 0.55, 0.8].iter().enumerate() {
        let i = (k * steps as f32) as usize;
        let c = SCARF_BANDS[(j + offset) % SCARF_BANDS.len()];
        let (p0, p1, p2, p3) = (left[i], right[i], right[i + 1], left[i + 1]);
        let _ = write!(
            s,
            r#"<path d="M{} {}L{} {}L{} {}L{} {}Z" fill="{c}"/>"#,
            n(p0.0),
            n(p0.1),
            n(p1.0),
            n(p1.1),
            n(lerp(p1, p2, 0.6).0),
            n(lerp(p1, p2, 0.6).1),
            n(lerp(p0, p3, 0.6).0),
            n(lerp(p0, p3, 0.6).1)
        );
    }
    let _ = write!(
        s,
        r#"<path d="{d}" fill="none" stroke="{OL}" stroke-width="1.8"/>"#
    );
}

/// The tail of the scarf thrown back over her left shoulder (behind her).
fn scarf_back(s: &mut String, r: &Rig) {
    let a = SCARF_ROOT_L;
    let b = r.scarf_l;
    let c = (a.0 + 22.0, a.1 + 4.0);
    scarf_end(s, a, c, b, 10.5, 1);
}

/// The loop round her neck and the end hanging in front.
fn scarf(s: &mut String, r: &Rig) {
    let a = SCARF_ROOT_R;
    let b = r.scarf_r;
    let c = if r.scarf_held {
        // Up over her hand, then hanging from it.
        let h = r.hand_r;
        (h.0 + 2.0 * (h.0 - a.0) / 10.0 + 6.0, h.1 - 16.0)
    } else {
        (a.0 - 3.0 + (b.0 - a.0) * 0.3, a.1 + (b.1 - a.1) * 0.5)
    };
    scarf_end(s, a, c, b, 12.0, 0);
    let _ = write!(
        s,
        r#"<path d="M82.5 116Q100 124 117.5 116L119 124.5Q100 135.5 81 124.5Z" fill="{CREAM}" stroke="{OL}" stroke-width="2"/><path d="M90 119.6L89.4 129.4M110 119.6L110.6 129.4" stroke="{TERRA}" stroke-width="3"/><path d="M100 121.5V132" stroke="{SAGE}" stroke-width="3"/>"#
    );
}

/// The sleeping wrap: the scarf wound round her from shoulders to knees,
/// her hands peeking out to hold it shut under her chin.
fn cocoon(s: &mut String, r: &Rig) {
    let breathe = -r.bob * 0.8;
    let shape = format!(
        "M76 118C64 134 60 162 61 188C62 212 68 230 80 238Q100 {} 120 238C132 230 138 212 139 188C140 162 136 134 124 118Q100 110 76 118Z",
        n(244.0 + breathe)
    );
    let _ = write!(
        s,
        r#"<clipPath id="tcc"><path d="{shape}"/></clipPath><path d="{shape}" fill="{CREAM}" stroke="{OL}" stroke-width="2.2"/><g clip-path="url(#tcc)">"#
    );
    // Wound bands slanting round her, a row of stitches on each.
    for (k, c) in [SAGE, AMBER, TERRA, PLUM_HI, SAGE].iter().enumerate() {
        let y = 140.0 + k as f32 * 21.0;
        let _ = write!(
            s,
            r#"<path d="M50 {}Q100 {} 150 {}" stroke="{c}" stroke-width="8" fill="none"/>"#,
            n(y + 6.0),
            n(y + 1.0),
            n(y - 9.0)
        );
        for j in 0..4 {
            let u = 0.2 + j as f32 * 0.2;
            let p = quad((50.0, y + 6.0), (100.0, y + 1.0), (150.0, y - 9.0), u);
            let _ = write!(
                s,
                r#"<path d="M{} {}l2.4 -2.4l2.4 2.4l-2.4 2.4Z" fill="{CREAM}"/>"#,
                n(p.0 - 2.4),
                n(p.1)
            );
        }
        let _ = write!(
            s,
            r#"<path d="M50 {}Q100 {} 150 {}" stroke="{CREAM_DEEP}" stroke-width="1.4" fill="none"/>"#,
            n(y + 16.0),
            n(y + 11.0),
            n(y + 1.0)
        );
    }
    // Shade along her far side.
    let _ = write!(
        s,
        r#"<path d="M130 120C142 150 142 205 126 238L150 238V110Z" fill="{CREAM_DEEP}" opacity=".7"/></g><path d="{shape}" fill="none" stroke="{OL}" stroke-width="2.2"/>"#
    );
    // Fringe of the wound-in end.
    for k in 0..5 {
        let x = 112.0 + k as f32 * 3.6;
        let y = 239.0 - k as f32 * 1.2 + breathe * 0.5;
        let _ = write!(
            s,
            r#"<path d="M{} {}l.6 6" stroke="{OL}" stroke-width="3.2"/><path d="M{} {}l.6 6" stroke="{CREAM_DEEP}" stroke-width="1.4"/>"#,
            n(x),
            n(y),
            n(x),
            n(y)
        );
    }
    // A collar roll snug under her chin, held shut by both hands.
    let _ = write!(
        s,
        r#"<path d="M76 116Q100 106 124 116Q128 121 124 126Q100 118 76 126Q72 121 76 116Z" fill="{CREAM}" stroke="{OL}" stroke-width="2"/><path d="M86 113.5L87 123M114 113.5L113 123" stroke="{TERRA}" stroke-width="3"/>"#
    );
    hand(s, (93.0, 126.0 + breathe * 0.3), true);
    hand(s, (107.0, 125.5 + breathe * 0.3), true);
}

/// Arm in a wide kimono sleeve: the cloth hangs from the forearm (down,
/// or out to the side when the arm itself hangs down), lined in cream.
fn arm(s: &mut String, sh: P, el: P, ha: P) {
    let cuff = lerp(el, ha, 0.68);
    let side = if sh.0 < 100.0 { -1.0 } else { 1.0 };
    let (ux, uy) = {
        let (dx, dy) = (cuff.0 - el.0, cuff.1 - el.1);
        let l = (dx * dx + dy * dy).sqrt().max(0.001);
        (dx / l, dy / l)
    };
    // Gravity across the forearm; where that vanishes, outward.
    let mut hang = (-uy * ux, 1.0 - uy * uy);
    let hl = (hang.0 * hang.0 + hang.1 * hang.1).sqrt();
    if hl < 0.7 {
        let k = 0.7 - hl;
        let nrm = if -uy * side > 0.0 {
            (-uy, ux)
        } else {
            (uy, -ux)
        };
        hang = (hang.0 + nrm.0 * k * 1.4, hang.1 + nrm.1 * k * 1.4);
    }
    let hl = (hang.0 * hang.0 + hang.1 * hang.1).sqrt().max(0.001);
    let hang = (hang.0 / hl * 15.0, hang.1 / hl * 15.0);
    let a = lerp(el, cuff, 0.1);
    let b = (cuff.0 + ux * 1.5, cuff.1 + uy * 1.5);
    let c = (b.0 + hang.0, b.1 + hang.1);
    let d = (a.0 + hang.0 * 0.75, a.1 + hang.1 * 0.75);
    let _ = write!(
        s,
        r#"<path d="M{} {}L{} {}L{} {}" stroke="{OL}" stroke-width="13" fill="none"/><path d="M{} {}L{} {}Q{} {} {} {}Q{} {} {} {}Z" fill="{SAGE}" stroke="{OL}" stroke-width="1.9"/><path d="M{} {}L{} {}" stroke="{CREAM}" stroke-width="2.2" fill="none"/><path d="M{} {}L{} {}L{} {}" stroke="{SAGE}" stroke-width="9.4" fill="none"/>"#,
        n(sh.0),
        n(sh.1),
        n(el.0),
        n(el.1),
        n(cuff.0),
        n(cuff.1),
        n(a.0),
        n(a.1),
        n(b.0),
        n(b.1),
        n(c.0 + ux * 1.5),
        n(c.1 + uy * 1.5),
        n(c.0 - ux * 3.0),
        n(c.1 - uy * 3.0),
        n(lerp(c, d, 0.5).0 + hang.0 * 0.12),
        n(lerp(c, d, 0.5).1 + hang.1 * 0.12),
        n(d.0),
        n(d.1),
        n(b.0 - hang.0 * 0.08),
        n(b.1 - hang.1 * 0.08),
        n(c.0 - ux * 0.5 - hang.0 * 0.1),
        n(c.1 - uy * 0.5 - hang.1 * 0.1),
        n(sh.0),
        n(sh.1),
        n(el.0),
        n(el.1),
        n(cuff.0),
        n(cuff.1)
    );
    // Wrist.
    let _ = write!(
        s,
        r#"<path d="M{} {}L{} {}" stroke="{OL}" stroke-width="8.6" fill="none"/><path d="M{} {}L{} {}" stroke="{SKIN}" stroke-width="5.4" fill="none"/>"#,
        n(cuff.0),
        n(cuff.1),
        n(ha.0),
        n(ha.1),
        n(cuff.0),
        n(cuff.1),
        n(ha.0),
        n(ha.1)
    );
}

fn hand(s: &mut String, h: P, grip: bool) {
    let _ = write!(
        s,
        r#"<circle cx="{}" cy="{}" r="5.2" fill="{SKIN}" stroke="{OL}" stroke-width="1.6"/>"#,
        n(h.0),
        n(h.1)
    );
    if grip {
        let _ = write!(
            s,
            r#"<path d="M{} {}q2.2 1.4 4.4 0" stroke="{SKIN_SH}" stroke-width="1.2" fill="none"/>"#,
            n(h.0 - 2.4),
            n(h.1 + 1.2)
        );
    }
}

/// A drop spindle hanging from `hook`, its shaft pointing `ang` degrees
/// (90 = straight down), turned `spin` turns: a slim cop of silk wound on
/// the shaft above a round amber whorl.
fn spindle(s: &mut String, hook: P, ang: f32, spin: f32) {
    let _ = write!(
        s,
        r#"<g transform="translate({} {}) rotate({})">"#,
        n(hook.0),
        n(hook.1),
        n(ang - 90.0)
    );
    // Shaft and hook.
    let _ = write!(
        s,
        r#"<path d="M0 -1V{SPINDLE_LEN}" stroke="{OL}" stroke-width="4.4"/><path d="M0 -1V{SPINDLE_LEN}" stroke="{WOOD}" stroke-width="2.2"/><path d="M0 -1q0-4 3-4.2q2.2 0 2 2.2" stroke="{OL}" stroke-width="1.5" fill="none"/>"#
    );
    // The cop: silk wound so far, its wraps turning with the shaft.
    let _ = write!(
        s,
        r#"<path d="M0 9C4 11 5.6 17 5.4 28H-5.4C-5.6 17 -4 11 0 9Z" fill="{CREAM}" stroke="{OL}" stroke-width="1.5"/>"#
    );
    for k in 0..4 {
        let y = 13.0 + (k as f32 + (spin * 4.0).rem_euclid(1.0)) * 3.6;
        if y > 26.5 {
            continue;
        }
        let hw = (1.2 + (y - 9.0) / 19.0 * 4.6) - 0.6;
        let _ = write!(
            s,
            r#"<path d="M{} {}L{} {}" stroke="{AMBER}" stroke-width="1.3"/>"#,
            n(-hw),
            n(y + 1.0),
            n(hw),
            n(y - 1.0)
        );
    }
    // Whorl seen from a little above, with marks going round.
    let wy = WHORL_AT;
    let _ = write!(
        s,
        r#"<ellipse cx="0" cy="{}" rx="11" ry="3.8" fill="{EMBER}" stroke="{OL}" stroke-width="1.6"/><ellipse cx="0" cy="{}" rx="11" ry="3.8" fill="{AMBER}" stroke="{OL}" stroke-width="1.6"/>"#,
        n(wy + 2.2),
        n(wy)
    );
    for k in 0..3 {
        let a = TAU * (spin + k as f32 / 3.0);
        let _ = write!(
            s,
            r#"<circle cx="{}" cy="{}" r="1.3" fill="{EMBER}"/>"#,
            n(7.4 * a.cos()),
            n(wy + 2.3 * a.sin())
        );
    }
    let _ = write!(
        s,
        r#"<circle cx="0" cy="{}" r="1.7" fill="{WOOD}" stroke="{OL}" stroke-width="1.3"/>"#,
        n(wy)
    );
    s.push_str("</g>");
}

/// A silk thread from `a` to `b` bowing through `c`, twist marks running
/// along it when `twist` is given (phase 0..1).
fn thread(s: &mut String, a: P, c: P, b: P, twist: Option<f32>) {
    let d = format!(
        "M{} {}Q{} {} {} {}",
        n(a.0),
        n(a.1),
        n(c.0),
        n(c.1),
        n(b.0),
        n(b.1)
    );
    let _ = write!(
        s,
        r#"<path d="{d}" stroke="{OL}" stroke-width="2.6" fill="none"/><path d="{d}" stroke="{AMBER_L}" stroke-width="1.2" fill="none"/>"#
    );
    if let Some(ph) = twist {
        for k in 0..4 {
            let u = (k as f32 + ph * 4.0).rem_euclid(4.0) / 4.0;
            let p = quad(a, c, b, 0.08 + u * 0.84);
            let _ = write!(
                s,
                r#"<path d="M{} {}l2.4 -1.6" stroke="{TERRA}" stroke-width="1.2"/>"#,
                n(p.0 - 1.2),
                n(p.1 + 0.8)
            );
        }
    }
}

fn props(s: &mut String, r: &Rig, hr: P, hl: P) {
    match r.prop {
        Prop::Held => {
            // Upright in her hand, gripped below the cop.
            let ang = 84.0f32;
            let d = dir(ang);
            let hook = (hl.0 - d.0 * 12.0, hl.1 - d.1 * 12.0);
            spindle(s, hook, ang, 0.0);
        }
        Prop::Drop { hook, spin, fluff } => {
            let top = (hl.0, hl.1 + 2.0);
            thread(
                s,
                top,
                lerp(top, hook, 0.5),
                hook,
                fluff.map(|_| spin / 3.0 * 4.0),
            );
            spindle(s, hook, 90.0, spin);
            if let Some(ph) = fluff {
                // Silk drawn from the puff up to her fingers.
                let mid = lerp(hr, top, 0.5);
                let wob = 3.0 * wave(ph, 2.0, 0.0);
                let c = (mid.0 - 4.0, mid.1 + wob);
                let d = format!(
                    "M{} {}Q{} {} {} {}",
                    n(hr.0 + 3.0),
                    n(hr.1 - 4.0),
                    n(c.0),
                    n(c.1),
                    n(top.0 - 2.0),
                    n(top.1)
                );
                let _ = write!(
                    s,
                    r#"<path d="{d}" stroke="{CREAM}" stroke-width="2.4" fill="none" opacity=".95"/><path d="{d}" stroke="{CREAM_DEEP}" stroke-width=".8" fill="none" stroke-dasharray="2 2"/>"#
                );
                fluff_puff(s, (hr.0 + 1.0, hr.1 - 4.0), ph);
            }
        }
        Prop::Knot(t) => {
            // Loops round each other, turning slowly between her hands.
            let c = lerp(hr, hl, 0.5);
            let c = (c.0, c.1 - 7.0);
            thread(s, hr, (hr.0 + 3.0, hr.1 - 8.0), (c.0 - 8.0, c.1), None);
            thread(s, (c.0 + 8.0, c.1), (hl.0 - 3.0, hl.1 - 8.0), hl, None);
            for k in 0..4 {
                let a = 45.0 * k as f32 + 20.0 * wave(t, 1.0, k as f32 / 4.0);
                let _ = write!(
                    s,
                    r#"<ellipse cx="{}" cy="{}" rx="9.5" ry="4.6" transform="rotate({} {} {})" fill="none" stroke="{OL}" stroke-width="3.4"/><ellipse cx="{}" cy="{}" rx="9.5" ry="4.6" transform="rotate({} {} {})" fill="none" stroke="{AMBER_L}" stroke-width="1.7"/>"#,
                    n(c.0),
                    n(c.1),
                    n(a),
                    n(c.0),
                    n(c.1),
                    n(c.0),
                    n(c.1),
                    n(a),
                    n(c.0),
                    n(c.1)
                );
            }
        }
        Prop::Tangle(t) => {
            // Loops all round her, the spindle swinging from her left hand.
            let mut d = String::new();
            let steps = 72;
            for i in 0..=steps {
                let k = i as f32 / steps as f32;
                let a = TAU * 2.2 * k + 0.4;
                let rx = 30.0 + 6.0 * (TAU * 3.0 * k).sin();
                let ry = 14.0 + 4.0 * (TAU * 2.0 * k + 1.0).cos();
                let loop_ = 6.0 * (TAU * 7.0 * k + TAU * t).sin();
                let x = 100.0 + rx * a.cos() + loop_ * (a * 1.7).cos();
                let y = 150.0 + ry * a.sin() + 12.0 * (k - 0.5) + loop_ * (a * 1.3).sin();
                let _ = write!(d, "{}{} {}", if i == 0 { "M" } else { "L" }, n(x), n(y));
            }
            let _ = write!(
                s,
                r#"<path d="M{} {}L{} {}" stroke="{OL}" stroke-width="2.6"/><path d="{d}" stroke="{OL}" stroke-width="2.6" fill="none"/><path d="{d}" stroke="{AMBER_L}" stroke-width="1.2" fill="none"/>"#,
                n(hr.0),
                n(hr.1),
                n(hr.0 + 8.0),
                n(hr.1 + 20.0)
            );
            let sw = 28.0 * wave(t, 1.0, 0.0);
            let len = 34.0;
            let dd = dir(90.0 - sw);
            let top = (hl.0 + 1.0, hl.1 + 3.0);
            let hook = (top.0 + dd.0 * len, top.1 + dd.1 * len);
            thread(s, top, lerp(top, hook, 0.5), hook, None);
            spindle(s, hook, 90.0 - sw, t * 2.0);
        }
        Prop::Floor => {}
    }
}

/// The puff of raw silk she drafts from.
fn fluff_puff(s: &mut String, c: P, t: f32) {
    let puffs = [
        (-5.0, 1.0, 5.0),
        (0.0, -3.0, 5.5),
        (5.0, 0.0, 5.0),
        (1.0, 3.5, 4.5),
    ];
    for (dx, dy, rr) in puffs {
        let _ = write!(
            s,
            r#"<circle cx="{}" cy="{}" r="{}" fill="{OL}"/>"#,
            n(c.0 + dx),
            n(c.1 + dy),
            n(rr + 1.6)
        );
    }
    for (k, (dx, dy, rr)) in puffs.iter().enumerate() {
        let b = 0.4 * wave(t, 2.0, k as f32 / 4.0);
        let _ = write!(
            s,
            r#"<circle cx="{}" cy="{}" r="{}" fill="{CREAM}"/>"#,
            n(c.0 + dx),
            n(c.1 + dy),
            n(rr + b)
        );
    }
}

fn back_hair(s: &mut String, r: &Rig) {
    // Two buns, wound with thread, behind the crown.
    for (cx, m) in [(69.0f32, -1.0f32), (131.0, 1.0)] {
        let _ = write!(
            s,
            r#"<circle cx="{cx}" cy="42" r="14" fill="{HAIR}" stroke="{OL}" stroke-width="2.2"/><path d="M{} 33Q{} 40 {} 52M{} 31Q{} 38 {} 50" stroke="{HAIR_SH}" stroke-width="1.3" fill="none"/><path d="M{} 36Q{} 33 {} 37" stroke="{HAIR_HI}" stroke-width="2.6" fill="none"/>"#,
            n(cx - m * 8.0),
            n(cx - m * 2.0),
            n(cx + m * 2.0),
            n(cx + m * 2.0),
            n(cx + m * 7.0),
            n(cx + m * 9.0),
            n(cx - m * 6.0),
            n(cx + m * 1.0),
            n(cx + m * 6.0)
        );
    }
    // A short bob, its ends curling under.
    let _ = write!(
        s,
        r#"<path d="M100 36C72 36 60 56 61 80C61 94 62 102 66 108Q74 112 80 106L120 106Q126 112 134 108C138 102 139 94 139 80C140 56 128 36 100 36Z" fill="{HAIR_SH}" stroke="{OL}" stroke-width="2.2"/>"#
    );
    let _ = r;
}

fn head(s: &mut String, r: &Rig) {
    // Face.
    let _ = write!(
        s,
        r#"<path d="M70 76C70 46 130 46 130 76C130 98 114 112.5 100 113C86 112.5 70 98 70 76Z" fill="{SKIN}" stroke="{OL}" stroke-width="2.2"/>"#
    );
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
    let _ = write!(
        s,
        r#"<path d="M100.6 96.6l-.9 1.5" stroke="{SKIN_SH}" stroke-width="1.4"/>"#
    );
    mouth(s, r.mouth);
    // Front hair: crown, a blunt scalloped fringe, short side locks.
    let _ = write!(
        s,
        r#"<path d="M65 106C58 92 59 72 63 60C69 44 83 37 100 37C117 37 131 44 137 60C141 72 142 92 135 106C130 103 128 98 127 90C126 82 126 75 125 68Q119.5 72.5 113.5 66.5Q107 72.5 100 67Q93 72.5 86.5 66.5Q80.5 72.5 75 68C74 75 74 82 73 90C72 98 70 103 65 106Z" fill="url(#thg)" stroke="{OL}" stroke-width="2.2"/>"#
    );
    // Strand lines and a highlight band.
    let _ = write!(
        s,
        r#"<path d="M113.5 66.5Q112 55 106 44M100 67V45M86.5 66.5Q88 55 94 44M131 70Q131 82 133 96M69 70Q69 82 67 96" stroke="{HAIR_SH}" stroke-width="1.2" fill="none"/><path d="M73 54Q86 44 97 43M104 43Q115 44 127 54" stroke="{HAIR_HI}" stroke-width="3" fill="none"/>"#
    );
    // Brows, just under the fringe.
    let (li, lo, arch) = match r.brow {
        Brow::Soft => (76.0, 76.5, -1.8),
        Brow::Raised => (73.5, 74.5, -2.4),
        Brow::Worried => (72.0, 78.0, 0.6),
        Brow::Sleepy => (77.5, 77.5, -0.8),
    };
    for m in [-1.0f32, 1.0] {
        let inner = 100.0 + m * 9.0;
        let outer = 100.0 + m * 18.5;
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
    // Red thread tied in a bow on her right bun, its tails swinging.
    let w = r.swing;
    let _ = write!(
        s,
        r#"<g transform="translate(62 52) rotate({})"><path d="M0 0Q-3 7 -1 13M0 0Q3 6 4 11" stroke="{OL}" stroke-width="3.2" fill="none"/><path d="M0 0Q-3 7 -1 13M0 0Q3 6 4 11" stroke="{TERRA}" stroke-width="1.6" fill="none"/></g><path d="M62 52Q54 44 55.5 52.5Q55 59 62 52ZM62 52Q70 45 69 53Q68 59 62 52Z" fill="{TERRA}" stroke="{OL}" stroke-width="1.5"/><circle cx="62" cy="52" r="2.2" fill="{EMBER}" stroke="{OL}" stroke-width="1.2"/>"#,
        n(w)
    );
    // A tiny shuttle pinned through her left bun.
    let _ = write!(
        s,
        r#"<g transform="translate(133 38) rotate(-30)"><path d="M-13 0Q-6 -4.5 0 -4.5Q6 -4.5 13 0Q6 4.5 0 4.5Q-6 4.5 -13 0Z" fill="{AMBER}" stroke="{OL}" stroke-width="1.6"/><rect x="-4.2" y="-1.8" width="8.4" height="3.6" rx="1.6" fill="{EMBER}" stroke="{OL}" stroke-width="1"/><path d="M-4 0H4" stroke="{AMBER_L}" stroke-width="1"/></g>"#
    );
    if let Extra::Sweat(t) = r.extra {
        let dy = 3.0 * t;
        let _ = write!(
            s,
            r#"<g transform="translate(139 {})"><path d="M0 -6C3 -1.5 4.4 1.5 2.6 4C1 6 -1 6 -2.6 4C-4.4 1.5 -3 -1.5 0 -6Z" fill="{SWEAT}" stroke="{OL}" stroke-width="1.3"/><ellipse cx="-.9" cy="1.6" rx=".9" ry="1.5" fill="white"/></g>"#,
            n(74.0 + dy)
        );
        let _ = write!(
            s,
            r#"<path d="M58 62l-6-4M56 70h-7M58 78l-6 3" stroke="{OL}" stroke-width="1.8"/>"#
        );
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
            n(cx + m * 9.6),
            n(yl + 1.4)
        );
    };
    let closed = |s: &mut String| {
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
    };
    match r.eyes {
        Eyes::Closed => {
            closed(s);
            let _ = write!(
                s,
                r#"<path d="M{} {}l{} 1.8" stroke="{OL}" stroke-width="1.5"/>"#,
                n(cx + m * 7.5),
                n(cy + 1.5),
                n(m * 1.6)
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
            let (ry, irx, iry) = if wide {
                (10.0, 5.0, 6.6)
            } else {
                (9.4, 6.0, 7.8)
            };
            let ix = cx + r.look.0;
            let iy = cy + 1.0 + r.look.1;
            let id = if m < 0.0 { "tel" } else { "ter" };
            let _ = write!(
                s,
                r#"<clipPath id="{id}"><ellipse cx="{}" cy="{}" rx="7.6" ry="{ry}"/></clipPath><ellipse cx="{}" cy="{}" rx="7.6" ry="{ry}" fill="white"/><g clip-path="url(#{id})"><ellipse cx="{}" cy="{}" rx="{irx}" ry="{iry}" fill="url(#tir)" stroke="{OL}" stroke-width=".9"/><ellipse cx="{}" cy="{}" rx="2.5" ry="3.4" fill="{OL}"/><ellipse cx="{}" cy="{}" rx="3.6" ry="1.8" fill="{AMBER_L}" opacity=".6"/><circle cx="{}" cy="{}" r="2.5" fill="white"/><circle cx="{}" cy="{}" r="1.1" fill="white" opacity=".9"/>"#,
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
                n(ix + 2.0),
                n(iy - 3.0),
                n(ix - 2.3),
                n(iy + 3.0)
            );
            let top = cy - ry + 0.6;
            let yl = top + r.lid * (2.0 * ry - 1.5);
            if r.lid > 0.0 {
                let _ = write!(
                    s,
                    r#"<rect x="{}" y="{}" width="18" height="{}" fill="{SKIN}"/>"#,
                    n(cx - 9.0),
                    n(cy - 13.0),
                    n(yl - cy + 13.0 + 0.6)
                );
            }
            s.push_str("</g>");
            if r.lid >= 0.95 {
                closed(s);
            } else {
                lash(s, yl);
                let _ = write!(
                    s,
                    r##"<path d="M{} {}Q{} {} {} {}" stroke="#7A3D2E" stroke-width="1.2" fill="none"/>"##,
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
            // A small cat-like smile.
            let _ = write!(
                s,
                r#"<path d="M95.5 103Q97.8 105.6 100 103.4Q102.2 105.6 104.5 103" stroke="{OL}" stroke-width="1.6" fill="none"/>"#
            );
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
            let _ = write!(
                s,
                r#"<path d="M96.5 104.6Q99 103 103.5 104.2" stroke="{OL}" stroke-width="1.6" fill="none"/>"#
            );
        }
        Mouth::Worried => {
            let _ = write!(
                s,
                r#"<path d="M95.5 105.5q1.5-1.8 3 0t3 0 3 0" stroke="{OL}" stroke-width="1.5" fill="none"/>"#
            );
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
                    n(162.0 + rad * 0.5),
                    n(70.0 - rad * 0.85),
                    n(rad),
                    n(rad),
                    n(162.0 + rad * 0.5),
                    n(70.0 + rad * 0.85),
                    op
                );
            }
        }
        Extra::Words(t) => {
            // Spoken words drifting in as little wisps, wound into thread
            // at her fingers.
            for k in 0..3 {
                let u = (t + k as f32 / 3.0) % 1.0;
                let from = (192.0, 46.0 + 16.0 * k as f32);
                let to = (151.0, 99.0);
                let p = lerp(from, to, u);
                let op = (PI * u).sin().powf(0.6);
                let sz = 8.0 * (1.0 - 0.55 * u);
                let d = format!(
                    "M{} {}q{} {} {} 0t{} 0",
                    n(p.0 - sz),
                    n(p.1),
                    n(sz / 2.0),
                    n(-sz * 0.6),
                    n(sz),
                    n(sz)
                );
                let _ = write!(
                    s,
                    r#"<g opacity="{op:.2}"><path d="{d}" stroke="{OL}" stroke-width="3.2" fill="none"/><path d="{d}" stroke="{AMBER_L}" stroke-width="1.6" fill="none"/></g>"#
                );
            }
        }
        Extra::Bubble(t) => {
            let cloud = [
                (152.0, 26.0, 9.0),
                (163.0, 18.0, 10.5),
                (176.0, 21.0, 9.5),
                (182.0, 30.0, 7.0),
                (169.0, 32.0, 8.5),
                (156.0, 33.0, 7.0),
            ];
            let lift = 1.0 * wave(t, 1.0, 0.0);
            let _ = write!(s, r#"<g transform="translate(0 {})">"#, n(lift));
            let _ = write!(
                s,
                r#"<circle cx="134" cy="54" r="2.6" fill="{CREAM}" stroke="{OL}" stroke-width="1.6"/><circle cx="142" cy="44" r="4" fill="{CREAM}" stroke="{OL}" stroke-width="1.6"/>"#
            );
            for (x, y, rr) in cloud {
                let _ = write!(
                    s,
                    r#"<circle cx="{x}" cy="{y}" r="{}" fill="{OL}"/>"#,
                    n(rr + 1.8)
                );
            }
            for (x, y, rr) in cloud {
                let _ = write!(s, r#"<circle cx="{x}" cy="{y}" r="{rr}" fill="{CREAM}"/>"#);
            }
            for k in 0..3 {
                let up = (TAU * (t * 2.0 - k as f32 / 6.0)).sin().max(0.0);
                let c = [SAGE_DEEP, AMBER, TERRA][k];
                let _ = write!(
                    s,
                    r#"<circle cx="{}" cy="{}" r="2.5" fill="{c}"/>"#,
                    158 + k * 9,
                    n(26.0 - 3.0 * up)
                );
            }
            s.push_str("</g>");
        }
        Extra::Zz(t) => {
            for k in 0..3 {
                let u = (t + k as f32 / 3.0) % 1.0;
                let x = 148.0 + u * 24.0 + 3.0 * (TAU * u).sin();
                let y = 70.0 - u * 50.0;
                let z = 4.0 + u * 5.0;
                let op = (PI * u).sin().powf(0.6);
                let d = format!(
                    "M{} {}h{}l-{} {}h{}",
                    n(x - z / 2.0),
                    n(y - z / 2.0),
                    n(z),
                    n(z),
                    n(z),
                    n(z)
                );
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

// ---------------------------------------------------------------- the scarf

/// Size of the scarf strip (`notes_svg`'s viewBox): as tall as a frame, so
/// it can hang next to her. It hangs from a rod at the top and grows down.
pub const NOTES_W: f32 = 96.0;
pub const NOTES_H: f32 = 320.0;
/// Frames in one loop of the scarf (the shuttle going across and back).
pub const NOTES_FRAMES: usize = 24;

/// Where the cloth hangs: left edge, width, top of the first band.
const CLOTH_X: f32 = 18.0;
const CLOTH_W: f32 = 60.0;
const CLOTH_TOP: f32 = 29.0;
/// Room under the last band for the warp, the shuttle and the beam.
const LOOM_H: f32 = 40.0;
/// Band height range: bands shrink from the largest as notes pile up; past
/// the smallest, the oldest roll up round the rod.
const BAND_MAX: f32 = 22.0;
const BAND_MIN: f32 = 10.0;
const MAX_BANDS: usize = ((NOTES_H - CLOTH_TOP - LOOM_H - 4.0) / BAND_MIN) as usize;

/// Base colour and motif colour of a kind's band.
fn kind_colors(kind: PointKind) -> (&'static str, &'static str) {
    match kind {
        PointKind::Decision => (SAGE, SAGE_DEEP),
        PointKind::Action => (TERRA, CREAM),
        PointKind::Question => (AMBER, PLUM),
        PointKind::Fact => (PLUM, CREAM),
        PointKind::Point => (SAGE_DEEP, CREAM),
    }
}

/// One woven band: the kind's colour with its motif in a row across it:
/// diamonds (decision), chevrons (action), rings (question), blocks (fact
/// and key point).
fn band(s: &mut String, kind: PointKind, x: f32, y: f32, w: f32, h: f32) {
    let (bg, fg) = kind_colors(kind);
    let _ = write!(
        s,
        r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{bg}"/>"#,
        n(x),
        n(y),
        n(w),
        n(h)
    );
    let m = (h * 0.62).min(11.0);
    let count = ((w / (m * 1.55)).floor() as usize).max(1);
    let step = w / count as f32;
    let cy = y + h / 2.0;
    for k in 0..count {
        let cx = x + step * (k as f32 + 0.5);
        let r = m / 2.0;
        let _ = match kind {
            PointKind::Decision => write!(
                s,
                r#"<path d="M{} {}L{} {}L{} {}L{} {}Z" fill="{fg}"/><circle cx="{}" cy="{}" r="{}" fill="{CREAM}"/>"#,
                n(cx),
                n(cy - r),
                n(cx + r),
                n(cy),
                n(cx),
                n(cy + r),
                n(cx - r),
                n(cy),
                n(cx),
                n(cy),
                n(r * 0.28)
            ),
            PointKind::Action => write!(
                s,
                r#"<path d="M{} {}L{} {}L{} {}" stroke="{fg}" stroke-width="{}" fill="none"/>"#,
                n(cx - r * 0.55),
                n(cy - r * 0.8),
                n(cx + r * 0.45),
                n(cy),
                n(cx - r * 0.55),
                n(cy + r * 0.8),
                n((m * 0.24).max(1.4))
            ),
            PointKind::Question => write!(
                s,
                r#"<circle cx="{}" cy="{}" r="{}" stroke="{fg}" stroke-width="{}" fill="none"/>"#,
                n(cx),
                n(cy),
                n(r * 0.62),
                n((m * 0.18).max(1.2))
            ),
            PointKind::Fact | PointKind::Point => {
                let q = r * 0.62;
                write!(
                    s,
                    r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{fg}"/>"#,
                    n(cx - q),
                    n(cy - q),
                    n(2.0 * q),
                    n(2.0 * q)
                )
            }
        };
    }
    // Weft ribs along the edges.
    let _ = write!(
        s,
        r#"<path d="M{} {}H{}M{} {}H{}" stroke="{CREAM_DEEP}" stroke-width="1.2" opacity=".85"/>"#,
        n(x),
        n(y + 0.6),
        n(x + w),
        n(x),
        n(y + h - 0.6),
        n(x + w)
    );
}

/// The scarf woven so far, one band per note (oldest at the top), hanging
/// from a rod; under the newest band the shuttle keeps going across the
/// warp, drawing in a thread of the newest band's colour. A complete SVG
/// document, NOTES_W × NOTES_H, looping over NOTES_FRAMES. Notes past
/// what fits roll up round the rod.
pub fn notes_svg(notes: &[PointKind], frame: usize) -> String {
    let t = (frame % NOTES_FRAMES) as f32 / NOTES_FRAMES as f32;
    let rolled = notes.len().saturating_sub(MAX_BANDS);
    let shown = &notes[rolled..];
    let avail = NOTES_H - CLOTH_TOP - LOOM_H - 4.0;
    let bh = if shown.is_empty() {
        BAND_MAX
    } else {
        (avail / shown.len() as f32).clamp(BAND_MIN, BAND_MAX)
    };
    let (x0, w) = (CLOTH_X, CLOTH_W);
    let fell = CLOTH_TOP + bh * shown.len() as f32;
    let mut s = String::with_capacity(8 * 1024);
    let _ = write!(
        s,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {NOTES_W} {NOTES_H}" width="{NOTES_W}" height="{NOTES_H}">"#
    );
    // It sways a little on its peg.
    let _ = write!(
        s,
        r#"<g transform="rotate({} 48 4)" stroke-linejoin="round" stroke-linecap="round">"#,
        n(0.7 * wave(t, 1.0, 0.0))
    );
    let _ = write!(
        s,
        r#"<path d="M14 18L48 4L82 18" stroke="{OL}" stroke-width="2.6" fill="none"/><path d="M14 18L48 4L82 18" stroke="{AMBER_L}" stroke-width="1.1" fill="none"/><circle cx="48" cy="4" r="2.4" fill="{WOOD}" stroke="{OL}" stroke-width="1.4"/>"#
    );
    // Warp threads below the cloth, held by the beam at the bottom.
    let beam = fell + 30.0;
    for k in 0..9 {
        let x = x0 + 3.0 + k as f32 * (w - 6.0) / 8.0;
        let xb = 48.0 + (x - 48.0) * 0.9;
        let _ = write!(
            s,
            r#"<path d="M{} {}L{} {}" stroke="{OL}" stroke-width="2.2" opacity=".55"/><path d="M{} {}L{} {}" stroke="{CREAM_DEEP}" stroke-width="1"/>"#,
            n(x),
            n(fell),
            n(xb),
            n(beam),
            n(x),
            n(fell),
            n(xb),
            n(beam)
        );
    }
    let _ = write!(
        s,
        r#"<rect x="{}" y="{}" width="{}" height="5" rx="2.5" fill="{WOOD}" stroke="{OL}" stroke-width="1.6"/>"#,
        n(x0 - 4.0),
        n(beam - 2.5),
        n(w + 8.0)
    );
    // The cloth: a cream heading, then the bands.
    let cloth_h = fell - 16.0;
    let _ = write!(
        s,
        r#"<rect x="{}" y="16" width="{}" height="{}" fill="{CREAM}"/><path d="M{} 24H{}" stroke="{PLUM_HI}" stroke-width="2"/>"#,
        n(x0),
        n(w),
        n(cloth_h),
        n(x0),
        n(x0 + w)
    );
    for (i, kind) in shown.iter().enumerate() {
        band(&mut s, *kind, x0, CLOTH_TOP + i as f32 * bh, w, bh);
    }
    if !shown.is_empty() {
        // The newest band: a sheen sweeping across it.
        let y = fell - bh;
        let u = -20.0 + 120.0 * t;
        let _ = write!(
            s,
            r#"<clipPath id="tnb"><rect x="{}" y="{}" width="{}" height="{}"/></clipPath><path clip-path="url(#tnb)" d="M{} {}L{} {}L{} {}L{} {}Z" fill="white" opacity=".35"/>"#,
            n(x0),
            n(y),
            n(w),
            n(bh),
            n(x0 + u),
            n(y + bh),
            n(x0 + u + 8.0),
            n(y + bh),
            n(x0 + u + 8.0 + bh * 0.6),
            n(y),
            n(x0 + u + bh * 0.6),
            n(y)
        );
    }
    let _ = write!(
        s,
        r#"<rect x="{}" y="16" width="{}" height="{}" fill="none" stroke="{OL}" stroke-width="1.8"/>"#,
        n(x0),
        n(w),
        n(cloth_h)
    );
    if !shown.is_empty() {
        // A spark twinkling on the newest band's edge.
        let a = 0.5 + 0.5 * wave(t, 2.0, 0.0);
        let (cx, cy) = (x0 + w, fell - bh / 2.0);
        let r = 2.0 + 1.8 * a;
        let _ = write!(
            s,
            r#"<path d="M{} {}L{} {}L{} {}L{} {}L{} {}L{} {}L{} {}L{} {}Z" fill="{AMBER_L}" stroke="{OL}" stroke-width=".9" opacity="{:.2}"/>"#,
            n(cx),
            n(cy - r * 2.0),
            n(cx + r * 0.45),
            n(cy - r * 0.45),
            n(cx + r * 2.0),
            n(cy),
            n(cx + r * 0.45),
            n(cy + r * 0.45),
            n(cx),
            n(cy + r * 2.0),
            n(cx - r * 0.45),
            n(cy + r * 0.45),
            n(cx - r * 2.0),
            n(cy),
            n(cx - r * 0.45),
            n(cy - r * 0.45),
            0.4 + 0.6 * a
        );
    }
    // The rod, with the rolled-up older part of the scarf round it.
    let roll = 3.5 + (rolled as f32 * 0.5).min(5.0);
    if rolled > 0 {
        let _ = write!(
            s,
            r#"<rect x="{}" y="{}" width="{}" height="{}" rx="{}" fill="{CREAM}" stroke="{OL}" stroke-width="1.6"/><path d="M{} {}V{}M{} {}V{}" stroke="{TERRA}" stroke-width="2.4"/>"#,
            n(x0 - 1.0),
            n(16.0 - roll),
            n(w + 2.0),
            n(roll * 2.0),
            n(roll),
            n(x0 + 12.0),
            n(16.0 - roll + 1.0),
            n(16.0 + roll - 1.0),
            n(x0 + w - 12.0),
            n(16.0 - roll + 1.0),
            n(16.0 + roll - 1.0)
        );
    }
    let _ = write!(
        s,
        r#"<rect x="6" y="13.5" width="84" height="5" rx="2.5" fill="{WOOD}" stroke="{OL}" stroke-width="1.6"/><circle cx="6" cy="16" r="3.6" fill="{WOOD_SH}" stroke="{OL}" stroke-width="1.5"/><circle cx="90" cy="16" r="3.6" fill="{WOOD_SH}" stroke="{OL}" stroke-width="1.5"/>"#
    );
    // The shuttle going across and back, pulling in the weft.
    let weft = shown
        .last()
        .map(|k| kind_colors(*k).0)
        .unwrap_or(CREAM_DEEP);
    let going = t < 0.5;
    let k = 0.5 - 0.5 * (TAU * t).cos();
    let sx = 14.0 + 68.0 * k;
    let sy = fell + 9.0;
    let from = if going { x0 } else { x0 + w };
    let _ = write!(
        s,
        r#"<path d="M{} {}L{} {}" stroke="{OL}" stroke-width="2.8"/><path d="M{} {}L{} {}" stroke="{weft}" stroke-width="1.4"/>"#,
        n(from),
        n(fell + 1.5),
        n(sx),
        n(sy),
        n(from),
        n(fell + 1.5),
        n(sx),
        n(sy)
    );
    let _ = write!(
        s,
        r#"<g transform="translate({} {})"><path d="M-12 0Q-5 -4.6 0 -4.6Q5 -4.6 12 0Q5 4.6 0 4.6Q-5 4.6 -12 0Z" fill="{AMBER}" stroke="{OL}" stroke-width="1.6"/><rect x="-4" y="-1.8" width="8" height="3.6" rx="1.6" fill="{weft}" stroke="{OL}" stroke-width="1"/></g>"#,
        n(sx),
        n(sy)
    );
    s.push_str("</g></svg>");
    s
}

/// Size of a single note leaving her spindle (`note_pop_svg`).
pub const POP_W: f32 = 48.0;
pub const POP_H: f32 = 48.0;
/// Frames of the pop. It plays once and rests on the last frame.
pub const POP_FRAMES: usize = 12;

/// A new note as a small woven swatch flying up off her spindle on its
/// thread and settling, for when a point arrives. A complete SVG document,
/// POP_W × POP_H; frames past the last show the last.
pub fn note_pop_svg(kind: PointKind, frame: usize) -> String {
    let k = frame.min(POP_FRAMES - 1) as f32 / (POP_FRAMES - 1) as f32;
    // Ease out with a little overshoot.
    let c = 1.9;
    let e = 1.0 + (c + 1.0) * (k - 1.0).powi(3) + c * (k - 1.0).powi(2);
    let pos = lerp((6.0, 44.0), (24.0, 21.0), e);
    let scale = 0.35 + 0.65 * e;
    let rot = -35.0 * (1.0 - e);
    let mut s = String::with_capacity(2048);
    let _ = write!(
        s,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {POP_W} {POP_H}" width="{POP_W}" height="{POP_H}"><g stroke-linejoin="round" stroke-linecap="round">"#
    );
    // The thread it trails back to the spindle.
    let end = (pos.0 - 15.0 * scale, pos.1 + 6.0 * scale);
    let d = format!(
        "M0 48Q{} {} {} {}",
        n(end.0 - 2.0),
        n(48.0 - (48.0 - end.1) * 0.1),
        n(end.0),
        n(end.1)
    );
    let _ = write!(
        s,
        r#"<path d="{d}" stroke="{OL}" stroke-width="2.4" fill="none"/><path d="{d}" stroke="{AMBER_L}" stroke-width="1.1" fill="none"/>"#
    );
    let _ = write!(
        s,
        r#"<g transform="translate({} {}) rotate({}) scale({:.3})">"#,
        n(pos.0),
        n(pos.1),
        n(rot),
        scale
    );
    // Fringe, swatch, outline.
    for j in 0..5 {
        let x = -11.0 + j as f32 * 5.5;
        let _ = write!(
            s,
            r#"<path d="M{} 9V14" stroke="{OL}" stroke-width="3"/><path d="M{} 9V14" stroke="{CREAM_DEEP}" stroke-width="1.3"/>"#,
            n(x),
            n(x)
        );
    }
    let _ = write!(
        s,
        r#"<rect x="-15" y="-10" width="30" height="20" fill="{CREAM}"/>"#
    );
    band(&mut s, kind, -15.0, -7.0, 30.0, 14.0);
    let _ = write!(
        s,
        r#"<rect x="-15" y="-10" width="30" height="20" fill="none" stroke="{OL}" stroke-width="1.8"/></g>"#
    );
    // A spark as it lands.
    if k > 0.55 {
        let a = ((k - 0.55) / 0.45 * PI).sin();
        let _ = write!(
            s,
            r#"<path d="M40 7l1.4-3.6 1.4 3.6 3.6 1.4-3.6 1.4-1.4 3.6-1.4-3.6-3.6-1.4Z" fill="{AMBER_L}" stroke="{OL}" stroke-width=".9" opacity="{a:.2}"/>"#
        );
    }
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
                assert!(!svg.contains("NaN") && !svg.contains("inf"), "{pose:?} {f}");
                assert!(svg.len() < 25_000, "{pose:?} {f}: {} bytes", svg.len());
            }
        }
    }

    #[test]
    fn poses_loop() {
        for pose in Pose::ALL {
            let count = frame_count(pose);
            assert_eq!(frame_svg(pose, 0), frame_svg(pose, count), "{pose:?}");
            // Neighbouring frames differ: every pose actually moves.
            assert_ne!(frame_svg(pose, 0), frame_svg(pose, 1), "{pose:?}");
        }
    }

    #[test]
    fn scarf_grows_one_band_per_note() {
        let kinds = [
            PointKind::Decision,
            PointKind::Action,
            PointKind::Question,
            PointKind::Fact,
        ];
        for count in [0, 1, 5, 20, 60] {
            let notes: Vec<PointKind> = (0..count).map(|i| kinds[i % 4]).collect();
            for f in 0..NOTES_FRAMES {
                let svg = notes_svg(&notes, f);
                assert!(svg.starts_with("<svg") && svg.ends_with("</svg>"));
                assert!(!svg.contains("<text") && !svg.contains("NaN"));
                assert!(svg.len() < 60_000, "{count}: {} bytes", svg.len());
            }
            let svg = notes_svg(&notes, 0);
            // One base rect per shown band, plus the heading, its outline
            // and the newest band's clip.
            let bands = svg.matches(r#"<rect x="18" y="#).count();
            assert_eq!(
                bands,
                count.min(MAX_BANDS) + 2 + usize::from(count > 0),
                "{count}"
            );
            assert_eq!(svg, notes_svg(&notes, NOTES_FRAMES));
        }
    }

    #[test]
    fn note_pop_rests_on_its_last_frame() {
        for kind in [
            PointKind::Decision,
            PointKind::Action,
            PointKind::Question,
            PointKind::Fact,
        ] {
            let last = note_pop_svg(kind, POP_FRAMES - 1);
            assert!(last.starts_with("<svg") && last.ends_with("</svg>"));
            assert_eq!(last, note_pop_svg(kind, POP_FRAMES + 5));
            assert_ne!(note_pop_svg(kind, 0), last);
        }
    }
}
