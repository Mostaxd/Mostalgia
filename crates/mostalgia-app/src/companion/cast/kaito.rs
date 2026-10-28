//! Kaito's artwork: the tea-house apprentice, a calm, polite boy in a short
//! matcha-green happi jacket with its sleeves tied up by a tasuki cord, an
//! indigo apron over grey hakama and a striped tenugui headband. He carries
//! a lacquered tray and a clay kyusu, and brews tea while you talk; steam
//! wisps are his recurring motif. Every animation frame is generated as an
//! SVG document (transparent background, viewBox FRAME_W × FRAME_H), in the
//! same way as Mori's (`companion::art`).
//!
//! He is drawn as a small rig: the body parts are fixed shapes, and each
//! pose only decides where the joints go for a phase `t` in `0..1` (head
//! tilt, eye state, where the tray and the teapot are; the arms follow by
//! two-bone IK). All motion is built from `sin`/`cos` of `2πt`, so every
//! pose loops seamlessly. No `<text>` and no filters: frames must render the
//! same in resvg and GPUI without fonts.
//!
//! His way of showing the meeting's notes is a tea shelf (`notes_svg`):
//! every note is a cup on a tray, five cups to a tray. The cup tells the
//! kind: a steaming yunomi for a decision, a matcha bowl with its whisk for
//! an action item, an iced glass for an open question and a small cup with
//! a standing tea stalk for a fact.

use std::f32::consts::{PI, TAU};
use std::fmt::{self, Write as _};

use super::PointKind;

/// What Kaito is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pose {
    /// Breathing, blinking, holding his tray in both hands; a faint wisp
    /// of steam rises from the cup on it.
    Idle,
    /// Someone started speaking: looks up attentively, the teapot paused
    /// in mid-air.
    Listening,
    /// Speech going on: pours tea into the cup on his tray, the stream
    /// flowing and the steam rising.
    Writing,
    /// Waiting for the language model: hand at his chin, watching a little
    /// hourglass on the tray while the tea steeps.
    Thinking,
    /// Showing an answer or a point: offers a cup forward with a small bow.
    Talking,
    /// Recording paused: dozing, kneeling beside his tray, the iron kettle
    /// softly steaming.
    Sleeping,
    /// The provider failed: the tray tips, the cup topples and tea splashes;
    /// flustered.
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
const HAIR: &str = "#4A3340"; // Roasted cocoa
const HAIR_TOP: &str = "#654857";
const HAIR_HI: &str = "#9C8091";
const HAIR_SH: &str = "#33232D";
const JACKET: &str = "#5E9068"; // Matcha
const JACKET_SH: &str = "#467352";
const JACKET_HI: &str = "#7FAF85";
const INDIGO: &str = "#34437C";
const INDIGO_SH: &str = "#27325E";
const INDIGO_HI: &str = "#4F62A3";
const HAKAMA: &str = "#7F7584";
const HAKAMA_SH: &str = "#665D6C";
const CREAM: &str = "#FBF3E4";
const CREAM_DEEP: &str = "#EADBC2";
const TERRA: &str = "#D9653B"; // tasuki cord
const EMBER: &str = "#B8472A";
const GOLD: &str = "#F2A541";
const GOLD_L: &str = "#F7C26B";
const WOOD: &str = "#C98B5A";
const WOOD_SH: &str = "#9A6239";
const WOOD_HI: &str = "#E0AE7E";
const TRAY: &str = "#A3402F"; // vermilion lacquer
const TRAY_SH: &str = "#74291F";
const TRAY_HI: &str = "#C9634A";
const CLAY: &str = "#C66A3E"; // tokoname clay kyusu
const CLAY_SH: &str = "#9C4A27";
const CLAY_HI: &str = "#E39466";
const IRON: &str = "#4F4A63";
const IRON_HI: &str = "#77708F";
const TEA: &str = "#B5CC62"; // green tea
const MATCHA: &str = "#86B04A";
const MATCHA_L: &str = "#BCD77F";
const GLASS: &str = "#DDF1F6";
const ICE: &str = "#7FB7D6";
const LEAF: &str = "#6FA34A";
const LEAF_HI: &str = "#A7CF7A";
const CORAL: &str = "#F59B7B";
const BAMBOO: &str = "#E8C77E";
const STEAM: &str = "#FFFFFF";
const MOUTH: &str = "#8A3444";
const TONGUE: &str = "#E9877A";
const BLUSH: &str = "#F2876F";
const BROW: &str = "#33232D";
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

/// A point as "x y", for path data.
fn xy(p: P) -> String {
    format!("{} {}", n(p.0), n(p.1))
}

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

/// `p` turned `deg` degrees around the origin (clockwise on screen).
fn turn(p: P, deg: f32) -> P {
    let (c, s) = (deg.to_radians().cos(), deg.to_radians().sin());
    (p.0 * c - p.1 * s, p.0 * s + p.1 * c)
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

/// A four-pointed sparkle with curved sides, as path data.
fn sparkle_d(c: P, r: f32) -> String {
    let (x, y, k) = (c.0, c.1, r * 0.16);
    format!(
        "M{}Q{} {}Q{} {}Q{} {}Q{} {}Z",
        xy((x, y - r)),
        xy((x + k, y - k)),
        xy((x + r, y)),
        xy((x + k, y + k)),
        xy((x, y + r)),
        xy((x - k, y + k)),
        xy((x - r, y)),
        xy((x - k, y - k)),
        xy((x, y - r))
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

/// Something standing on the tray, at a tray-local x.
#[derive(Clone, Copy)]
enum Item {
    /// A cup of the given kind; `steam` is how strongly it steams (0..1).
    Cup(PointKind, f32),
    /// The kyusu, resting on the tray.
    Pot,
    /// The little steeping hourglass, at phase `t` of its loop.
    Hourglass(f32),
    /// A cup toppling off the tray's low edge, tipped this many degrees.
    Tipped(f32),
}

#[derive(Clone)]
struct Tray {
    /// Centre of the tray's top and its tilt (degrees).
    c: P,
    rot: f32,
    /// Held level in both hands (`c` decides where the hands go); otherwise
    /// carried in his left hand (the tray follows the hand).
    both: bool,
    items: Vec<(f32, Item)>,
}

#[derive(Clone, Copy)]
enum Extra {
    None,
    /// Little sound arcs at his ear.
    Listen(f32),
    /// Tea leaves drifting round over his head while the tea steeps.
    Leaves(f32),
    /// Drifting z's.
    Zz(f32),
    /// Tea splashing from the toppling cup, a sweat drop, stress marks.
    Spill(f32),
}

struct Rig {
    /// Phase of the loop, for steam.
    t: f32,
    /// Whole figure: horizontal shake and sway (degrees around the feet).
    shake: f32,
    sway: f32,
    /// Upper body offset (breathing).
    bob: f32,
    /// Kneeling (sleeping): the upper body sinks by `SIT`.
    seated: bool,
    head_rot: f32,
    head_dx: f32,
    head_dy: f32,
    eyes: Eyes,
    look: P,
    /// Upper lid, 0 = open, 1 = shut. He is calm: it rests a little low.
    lid: f32,
    mouth: Mouth,
    brow: Brow,
    blush: f32,
    /// Swing of the headband's tails (degrees).
    swing: f32,
    /// His right hand (viewer's left) and his left, with the elbow side.
    hand_r: P,
    bend_r: f32,
    hand_l: P,
    bend_l: f32,
    tray: Option<Tray>,
    /// The kyusu in his right hand, tilted this many degrees (pouring when
    /// it's well over 20).
    pot: Option<f32>,
    /// A cup offered on his right hand.
    offer: bool,
    /// Pouring from the pot into the tray's first cup.
    pour: bool,
    /// The open right hand raised (flustered), fingers spread.
    open_r: bool,
    extra: Extra,
}

const SHOULDER_R: P = (82.0, 165.0);
const SHOULDER_L: P = (118.0, 165.0);
const UPPER_ARM: f32 = 22.0;
const FOREARM: f32 = 21.0;
/// The head is drawn at its native scale (chin at y 113), then scaled
/// around the chin onto the neck. He's a kid: a big head on a small body.
const HEAD_SCALE: f32 = 1.04;
const NECK: P = (100.0, 150.0);
/// How far the upper body sinks when he kneels.
const SIT: f32 = 40.0;
/// Half the tray's width and depth (its top is an ellipse).
const TRAY_RX: f32 = 25.0;
const TRAY_RY: f32 = 6.0;
/// Where his left hand holds the tray when he carries it in one hand.
const TRAY_GRIP: P = (23.5, 3.0);
/// The kyusu, in its own coordinates (origin where his hand grips the
/// handle): the body's centre and the spout's tip.
const POT_BODY: P = (21.0, 0.0);
const POT_SPOUT: P = (38.0, -7.5);

fn base(t: f32) -> Rig {
    Rig {
        t,
        shake: 0.0,
        sway: 0.0,
        bob: 0.0,
        seated: false,
        head_rot: 0.0,
        head_dx: 0.0,
        head_dy: 0.0,
        eyes: Eyes::Open,
        look: (0.0, 0.0),
        lid: 0.14,
        mouth: Mouth::Smile,
        brow: Brow::Soft,
        blush: 0.4,
        swing: 0.0,
        hand_r: (72.0, 206.0),
        bend_r: 1.0,
        hand_l: (130.0, 200.0),
        bend_l: -1.0,
        tray: None,
        pot: None,
        offer: false,
        pour: false,
        open_r: false,
        extra: Extra::None,
    }
}

fn tray_point(tr: &Tray, local: P) -> P {
    let p = turn(local, tr.rot);
    (tr.c.0 + p.0, tr.c.1 + p.1)
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

fn rig(pose: Pose, frame: usize, t: f32) -> Rig {
    let mut r = base(t);
    match pose {
        Pose::Idle => {
            let breath = 0.5 - 0.5 * (TAU * 2.0 * t).cos();
            r.sway = 0.7 * wave(t, 1.0, 0.0);
            r.bob = -0.9 * breath;
            r.head_rot = 2.0 * wave(t, 1.0, 0.1);
            r.swing = -r.head_rot * 1.5 + 3.0 * wave(t, 2.0, 0.2);
            r.lid = match frame {
                14 => 0.6,
                15 => 1.0,
                16 => 0.4,
                _ => 0.14,
            };
            r.tray = Some(Tray {
                c: (100.0, 202.0 - 0.8 * breath),
                rot: 0.0,
                both: true,
                items: vec![
                    (-10.0, Item::Pot),
                    (13.0, Item::Cup(PointKind::Decision, 0.6)),
                ],
            });
        }
        Pose::Listening => {
            let nod = wave(t, 2.0, 0.0);
            r.head_rot = -6.0 + 1.0 * wave(t, 1.0, 0.0);
            r.head_dy = -1.0 + 0.5 * nod;
            r.bob = -0.5 * (0.5 - 0.5 * (TAU * t).cos());
            r.eyes = Eyes::Wide;
            r.lid = 0.0;
            r.look = (-1.6, -1.6);
            r.brow = Brow::Raised;
            r.mouth = Mouth::O(1.4 + 0.2 * nod);
            r.swing = 5.0 + 3.0 * wave(t, 2.0, 0.1);
            r.hand_r = (64.0, 184.0 + 0.6 * nod);
            r.bend_r = -1.0;
            r.pot = Some(-6.0 + 1.0 * wave(t, 1.0, 0.0));
            r.hand_l = (132.0, 202.0);
            r.tray = Some(Tray {
                c: (0.0, 0.0),
                rot: -3.0,
                both: false,
                items: vec![(-6.0, Item::Cup(PointKind::Decision, 0.35))],
            });
            r.extra = Extra::Listen(t);
        }
        Pose::Writing => {
            let pulse = wave(t, 1.0, 0.0);
            r.head_rot = 5.0 + 1.0 * pulse;
            r.head_dy = 2.0;
            r.head_dx = 0.5;
            r.look = (0.6, 2.6);
            r.lid = 0.38;
            r.mouth = Mouth::Smile;
            r.swing = -4.0 + 2.0 * wave(t, 2.0, 0.0);
            r.bob = -0.5 * (0.5 - 0.5 * (TAU * t).cos());
            r.hand_l = (130.0, 205.0);
            r.tray = Some(Tray {
                c: (0.0, 0.0),
                rot: -2.0,
                both: false,
                items: vec![(-3.0, Item::Cup(PointKind::Decision, 1.0))],
            });
            r.hand_r = (70.0, 151.0 + 1.0 * pulse);
            r.bend_r = -1.0;
            r.pot = Some(35.0 + 2.5 * pulse);
            r.pour = true;
        }
        Pose::Thinking => {
            r.head_rot = 7.0 + 1.2 * wave(t, 1.0, 0.0);
            r.head_dx = 0.5;
            r.look = (1.8, 2.4);
            r.lid = 0.3;
            r.brow = Brow::Raised;
            r.mouth = Mouth::Hmm;
            r.swing = -r.head_rot * 0.8;
            r.bob = -0.6 * (0.5 - 0.5 * (TAU * t).cos());
            r.hand_l = (132.0, 204.0);
            r.tray = Some(Tray {
                c: (0.0, 0.0),
                rot: -3.0,
                both: false,
                items: vec![
                    (-13.0, Item::Hourglass(t)),
                    (4.0, Item::Cup(PointKind::Decision, 0.5)),
                ],
            });
            let tap = wave(t, 2.0, 0.0).max(0.0);
            let chin = head_point(&r, (104.0, 114.0));
            r.hand_r = (chin.0 - 1.0, chin.1 + 4.0 - 1.4 * tap);
            r.bend_r = 1.0;
            r.extra = Extra::Leaves(t);
        }
        Pose::Talking => {
            let syl = (TAU * 2.0 * t).sin().abs();
            let bow = 0.5 - 0.5 * (TAU * t).cos();
            r.head_rot = 3.0 + 4.0 * bow;
            r.head_dy = 1.5 * bow - 0.5 * syl;
            r.bob = 1.0 * bow;
            r.mouth = if syl > 0.2 {
                Mouth::Open(3.0 * syl)
            } else {
                Mouth::Smile
            };
            r.eyes = if (6..=9).contains(&frame) {
                Eyes::Happy
            } else {
                Eyes::Open
            };
            r.lid = 0.2;
            r.blush = 0.55;
            r.swing = -r.head_rot * 1.2 + 3.0 * wave(t, 2.0, 0.2);
            r.hand_r = (60.0 - 2.0 * bow, 182.0 - 3.0 * bow);
            r.bend_r = 1.0;
            r.offer = true;
            r.hand_l = (133.0, 204.0);
            r.tray = Some(Tray {
                c: (0.0, 0.0),
                rot: -2.0,
                both: false,
                items: vec![(-8.0, Item::Pot)],
            });
        }
        Pose::Sleeping => {
            let s = wave(t, 1.0, 0.0);
            r.seated = true;
            r.head_rot = 9.0 + 2.5 * s;
            r.head_dy = 3.0 + 1.2 * s;
            r.head_dx = 1.0;
            r.bob = -1.2 * (0.5 - 0.5 * (TAU * t).cos());
            r.eyes = Eyes::Closed;
            r.brow = Brow::Sleepy;
            r.mouth = Mouth::O(1.0 + 0.3 * s);
            r.blush = 0.6;
            r.swing = 10.0 - 2.0 * s;
            r.hand_r = (93.0, 214.0);
            r.bend_r = 1.0;
            r.hand_l = (107.0, 214.0);
            r.bend_l = -1.0;
            r.extra = Extra::Zz(t);
        }
        Pose::Error => {
            r.shake = 1.4 * wave(t, 3.0, 0.0);
            r.head_rot = -3.0 + 1.5 * wave(t, 3.0, 0.15);
            r.eyes = Eyes::Squeeze;
            r.brow = Brow::Worried;
            r.mouth = Mouth::Worried;
            r.blush = 0.7;
            r.swing = 9.0 * wave(t, 3.0, 0.3);
            r.hand_r = (70.0, 150.0 + 2.0 * wave(t, 3.0, 0.0));
            r.bend_r = 1.0;
            r.open_r = true;
            r.hand_l = (134.0, 198.0);
            let tip = 34.0 + 10.0 * wave(t, 3.0, 0.1);
            r.tray = Some(Tray {
                c: (0.0, 0.0),
                rot: 16.0 + 3.0 * wave(t, 3.0, 0.0),
                both: false,
                items: vec![(13.0, Item::Tipped(tip))],
            });
            r.extra = Extra::Spill(t);
        }
    }
    if let Some(tr) = &r.tray
        && tr.both
    {
        r.hand_r = tray_point(tr, (-TRAY_RX + 1.0, 2.5));
        r.hand_l = tray_point(tr, (TRAY_RX - 1.0, 2.5));
    }
    r
}

// ---------------------------------------------------------------- drawing

fn draw(r: &Rig) -> String {
    let mut s = String::with_capacity(24 * 1024);
    let _ = write!(
        s,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}"><defs><linearGradient id="kir" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#22301E"/><stop offset=".55" stop-color="#4E7A3E"/><stop offset="1" stop-color="#B9D47A"/></linearGradient><linearGradient id="khg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{HAIR_TOP}"/><stop offset="1" stop-color="{HAIR}"/></linearGradient></defs>"##,
        w = FRAME_W,
        h = FRAME_H
    );
    // Ground shadow (not swayed).
    let rx = if r.seated { 54 } else { 34 };
    let _ = write!(
        s,
        r##"<ellipse cx="100" cy="300" rx="{rx}" ry="4.5" fill="#2B1730" opacity=".13"/>"##
    );
    if r.seated {
        // The iron kettle on its trivet behind him, softly steaming.
        kettle(&mut s, (163.0, 296.0), r.t);
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
    let _ = write!(s, r#"<g transform="{head_tf}">"#);
    back_hair(&mut s, r);
    s.push_str("</g>");
    torso(&mut s, r.seated);
    // Arms: his left (viewer's right) carries the tray.
    let (el, hl) = ik(SHOULDER_L, r.hand_l, UPPER_ARM, FOREARM, r.bend_l);
    let (er, hr) = ik(SHOULDER_R, r.hand_r, UPPER_ARM, FOREARM, r.bend_r);
    let tray = r.tray.clone().map(|mut tr| {
        if !tr.both {
            let g = turn(TRAY_GRIP, tr.rot);
            tr.c = (hl.0 - g.0, hl.1 - g.1);
        }
        tr
    });
    arm(&mut s, SHOULDER_L, el, hl);
    arm(&mut s, SHOULDER_R, er, hr);
    // Head.
    let _ = write!(s, r#"<g transform="{head_tf}">"#);
    head(&mut s, r);
    s.push_str("</g>");
    if let Some(tr) = &tray {
        draw_tray(&mut s, tr, r.t);
    }
    hand(&mut s, el, hl, false);
    if r.offer {
        hand(&mut s, er, hr, false);
        let base = (hr.0 + 0.5, hr.1 - 3.0);
        cup(&mut s, PointKind::Decision, base, 1.05, 0.0);
        steam(&mut s, (base.0, base.1 - 16.0), 1.0, r.t, 0.9);
    } else {
        if let Some(a) = r.pot {
            pot(&mut s, hr, a);
        }
        hand(&mut s, er, hr, r.open_r);
    }
    if r.pour
        && let (Some(a), Some(tr)) = (r.pot, &tray)
        && let Some(&(x, _)) = tr.items.first()
    {
        let sp = turn(POT_SPOUT, a);
        let spout = (hr.0 + sp.0, hr.1 + sp.1);
        let mouth = tray_point(tr, (x, -14.0));
        stream(&mut s, spout, mouth, r.t);
    }
    s.push_str("</g>");
    extras(&mut s, r, tray.as_ref());
    s.push_str("</g>");
    if r.seated {
        // His tray set down on the floor in front of him, a cup on it.
        let tr = Tray {
            c: (40.0, 289.0),
            rot: 0.0,
            both: true,
            items: vec![
                (-9.0, Item::Pot),
                (13.0, Item::Cup(PointKind::Decision, 0.5)),
            ],
        };
        draw_tray(&mut s, &tr, r.t);
    }
    s.push_str("</svg>");
    s
}

/// White tabi socks in wooden zori, peeking out under the hakama.
fn feet(s: &mut String) {
    for (cx, m) in [(87.0f32, -1.0f32), (113.0, 1.0)] {
        let _ = write!(
            s,
            r#"<ellipse cx="{}" cy="298" rx="8.5" ry="2.6" fill="{WOOD}" stroke="{OL}" stroke-width="1.6"/><path d="M{}L{}Q{} {}L{}Q{} {}Z" fill="white" stroke="{OL}" stroke-width="1.6"/><path d="M{}L{}" stroke="{CREAM_DEEP}" stroke-width="1.1"/><path d="M{}L{}L{}" stroke="{EMBER}" stroke-width="1.8" fill="none"/>"#,
            n(cx + m * 0.5),
            xy((cx - 5.5, 286.0)),
            xy((cx + 5.5, 286.0)),
            xy((cx + 7.2, 292.0)),
            xy((cx + 6.4, 296.5)),
            xy((cx - 6.4, 296.5)),
            xy((cx - 7.2, 292.0)),
            xy((cx - 5.5, 286.0)),
            xy((cx + m * 2.2, 296.0)),
            xy((cx + m * 2.4, 292.5)),
            xy((cx - 4.5, 295.0)),
            xy((cx + m * 2.3, 291.8)),
            xy((cx + 4.5, 295.0))
        );
    }
}

fn torso(s: &mut String, seated: bool) {
    // Neck with the chin's shadow.
    let _ = write!(
        s,
        r#"<path d="M95.5 138L95.5 158L104.5 158L104.5 138Z" fill="{SKIN}" stroke="{OL}" stroke-width="1.8"/><path d="M96.4 143Q100 149 103.6 143L103.6 147Q100 151 96.4 147Z" fill="{SKIN_SH}"/>"#
    );
    if seated {
        // Kneeling (seiza): the hakama spreads into a round heap.
        let _ = write!(
            s,
            r#"<path d="M78 203L122 203C128 214 146 232 150 249Q151 258 141 258.5L59 258.5Q49 258 50 249C54 232 72 214 78 203Z" fill="{HAKAMA}" stroke="{OL}" stroke-width="2"/><path d="M60 246Q76 238 96 245M104 245Q124 238 140 246" stroke="{HAKAMA_SH}" stroke-width="1.6" fill="none"/><path d="M54 251Q100 259 146 251" stroke="{HAKAMA_SH}" stroke-width="2.2" fill="none"/><path d="M84 215L74 240M116 215L126 240" stroke="{HAKAMA_SH}" stroke-width="1.4" fill="none"/>"#
        );
    } else {
        // Wide pleated hakama, split into two legs at the bottom.
        let _ = write!(
            s,
            r#"<path d="M79 204L121 204C124 228 130 258 134 289Q118 292 103 289L100 262L97 289Q82 292 66 289C70 258 76 228 79 204Z" fill="{HAKAMA}" stroke="{OL}" stroke-width="2"/><path d="M89 212L81 288M111 212L119 288M93 230L90 288M107 230L110 288" stroke="{HAKAMA_SH}" stroke-width="1.5" fill="none"/>"#
        );
    }
    // The short happi jacket, flaring a little at the hips.
    let _ = write!(
        s,
        r#"<path d="M90 151L110 151C116 153 120 156 122 162C124 178 127 197 130 218Q115 222 100 221Q85 222 70 218C73 197 76 178 78 162C80 156 84 153 90 151Z" fill="{JACKET}" stroke="{OL}" stroke-width="2"/><path d="M82 172Q80 195 75 216M118 172Q120 195 125 216" stroke="{JACKET_SH}" stroke-width="1.6" fill="none"/>"#
    );
    // Cream under-kimono in the V and the indigo collar band of the happi
    // running down both front edges.
    let _ = write!(
        s,
        r#"<path d="M92 151L108 151L100 168Z" fill="{CREAM}" stroke="{OL}" stroke-width="1.4"/>"#
    );
    for m in [-1.0f32, 1.0] {
        let x = |dx: f32| n(100.0 + m * dx);
        let _ = write!(
            s,
            r#"<path d="M{} 150.5L{} 150.5L{} 170L{} 206L{} 206L{} 172Z" fill="{INDIGO}" stroke="{OL}" stroke-width="1.6"/><path d="M{} 153L{} 171L{} 205" stroke="{INDIGO_HI}" stroke-width="1" fill="none"/>"#,
            x(10.5),
            x(5.5),
            x(-1.0),
            x(-0.5),
            x(4.5),
            x(4.0),
            x(7.5),
            x(2.0),
            x(2.0)
        );
    }
    // The tasuki cord over his shoulders, tying the sleeves back.
    let _ = write!(
        s,
        r#"<path d="M91 152Q84 154 80 162M109 152Q116 154 120 162" stroke="{OL}" stroke-width="4.4" fill="none"/><path d="M91 152Q84 154 80 162M109 152Q116 154 120 162" stroke="{TERRA}" stroke-width="2.2" fill="none"/>"#
    );
    // The indigo apron with a cream tea-leaf crest, tied at the waist.
    if seated {
        let _ = write!(
            s,
            r#"<path d="M73 201L127 201L132 236Q100 243 68 236Z" fill="{INDIGO}" stroke="{OL}" stroke-width="2"/>"#
        );
        crest(s, (100.0, 221.0), 0.75);
    } else {
        let _ = write!(
            s,
            r#"<path d="M77 201L123 201L125 242Q125.5 248 119 248L81 248Q74.5 248 75 242Z" fill="{INDIGO}" stroke="{OL}" stroke-width="2"/><path d="M78 243.5Q100 246 122 243.5" stroke="{INDIGO_SH}" stroke-width="1.6" fill="none"/>"#
        );
        crest(s, (100.0, 223.0), 0.9);
    }
    let _ = write!(
        s,
        r#"<path d="M73 196L127 196L127.5 203.5L72.5 203.5Z" fill="{INDIGO_SH}" stroke="{OL}" stroke-width="1.8"/><path d="M75 199.8H125" stroke="{CREAM}" stroke-width="1" stroke-dasharray="2 2"/><path d="M124 201Q130 206 128 214M126 201Q133 204 134 210" stroke="{OL}" stroke-width="3.6" fill="none"/><path d="M124 201Q130 206 128 214M126 201Q133 204 134 210" stroke="{CREAM_DEEP}" stroke-width="1.8" fill="none"/>"#
    );
}

/// The apron's crest: a tea leaf in a ring.
fn crest(s: &mut String, c: P, k: f32) {
    let _ = write!(
        s,
        r#"<g transform="translate({} {}) scale({k})"><circle r="8.5" fill="none" stroke="{CREAM}" stroke-width="1.6"/><path d="M-4.5 4Q-5 -4 4 -5Q5 3 -4.5 4Z" fill="{CREAM}"/><path d="M-4.5 4L2 -2.5" stroke="{INDIGO}" stroke-width="1"/></g>"#,
        n(c.0),
        n(c.1)
    );
}

/// An arm: the happi sleeve rolled back and tied above the elbow, the bare
/// forearm below (the hand is drawn on its own, over what it holds).
fn arm(s: &mut String, sh: P, el: P, ha: P) {
    let (dx, dy) = (el.0 - sh.0, el.1 - sh.1);
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let d = (dx / len, dy / len);
    let nrm = (-d.1, d.0);
    // Forearm.
    let _ = write!(
        s,
        r#"<path d="M{}L{}" stroke="{OL}" stroke-width="8.6" fill="none"/><path d="M{}L{}" stroke="{SKIN}" stroke-width="5.4" fill="none"/>"#,
        xy(el),
        xy(ha),
        xy(el),
        xy(ha)
    );
    // Upper sleeve, puffed by the cord.
    let cuff = add(el, d, 1.5);
    let _ = write!(
        s,
        r#"<path d="M{}L{}" stroke="{OL}" stroke-width="15.4" fill="none"/><path d="M{}L{}" stroke="{JACKET}" stroke-width="11.8" fill="none"/><path d="M{}L{}" stroke="{JACKET_SH}" stroke-width="2" fill="none"/>"#,
        xy(sh),
        xy(cuff),
        xy(sh),
        xy(cuff),
        xy(add(lerp(sh, cuff, 0.35), nrm, 3.5)),
        xy(add(lerp(sh, cuff, 0.9), nrm, 3.5))
    );
    // Rolled cuff round the elbow.
    let c = add(el, d, 0.5);
    let ang = d.1.atan2(d.0).to_degrees();
    let _ = write!(
        s,
        r#"<ellipse cx="{}" cy="{}" rx="3.6" ry="7.6" transform="rotate({} {} {})" fill="{JACKET_HI}" stroke="{OL}" stroke-width="1.8"/>"#,
        n(c.0),
        n(c.1),
        n(ang),
        n(c.0),
        n(c.1)
    );
    // The tasuki loop round the upper arm.
    let p = lerp(sh, el, 0.45);
    let (a, b) = (add(p, nrm, 6.6), add(p, nrm, -6.6));
    let _ = write!(
        s,
        r#"<path d="M{}L{}" stroke="{OL}" stroke-width="4.4"/><path d="M{}L{}" stroke="{TERRA}" stroke-width="2.2"/>"#,
        xy(a),
        xy(b),
        xy(a),
        xy(b)
    );
}

/// His hand at `ha` (reaching from the elbow `el`): a small fist, or
/// spread open when flustered.
fn hand(s: &mut String, el: P, ha: P, open: bool) {
    let (dx, dy) = (ha.0 - el.0, ha.1 - el.1);
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let d = (dx / len, dy / len);
    if open {
        let mut f = String::new();
        for a in [-50.0f32, -20.0, 10.0, 40.0] {
            let v = turn(d, a);
            let _ = write!(f, "M{}L{}", xy(ha), xy(add(ha, v, 8.0)));
        }
        let _ = write!(
            s,
            r#"<path d="{f}" stroke="{OL}" stroke-width="4.6"/><path d="{f}" stroke="{SKIN}" stroke-width="2.4"/>"#
        );
    }
    let _ = write!(
        s,
        r#"<circle cx="{}" cy="{}" r="4.8" fill="{SKIN}" stroke="{OL}" stroke-width="1.6"/>"#,
        n(ha.0),
        n(ha.1)
    );
    if !open {
        let k = add(ha, d, 1.4);
        let _ = write!(
            s,
            r#"<path d="M{}q2 1.3 4 0" stroke="{SKIN_SH}" stroke-width="1.2" fill="none"/>"#,
            xy((k.0 - 2.0, k.1 + 0.6))
        );
    }
}

/// The lacquered round tray and what stands on it.
fn draw_tray(s: &mut String, tr: &Tray, t: f32) {
    let _ = write!(
        s,
        r#"<g transform="translate({}) rotate({})"><path d="M-{rx} 0L-{rx} 3.6A{rx} {ry} 0 0 0 {rx} 3.6L{rx} 0Z" fill="{TRAY_SH}" stroke="{OL}" stroke-width="1.8"/><ellipse rx="{rx}" ry="{ry}" fill="{TRAY}" stroke="{OL}" stroke-width="1.8"/><ellipse cy=".6" rx="{}" ry="{}" fill="{TRAY_SH}" opacity=".45"/><path d="M-{}-1.5Q-6 -4.8 6 -4.6" stroke="{TRAY_HI}" stroke-width="1.4" fill="none"/>"#,
        xy(tr.c),
        n(tr.rot),
        n(TRAY_RX - 3.5),
        n(TRAY_RY - 1.6),
        n(TRAY_RX - 6.0),
        rx = TRAY_RX,
        ry = TRAY_RY,
    );
    for &(x, item) in &tr.items {
        match item {
            Item::Cup(kind, _) => cup(s, kind, (x, 1.0), 1.0, 0.0),
            Item::Pot => pot_on_tray(s, x),
            Item::Hourglass(ph) => hourglass(s, (x, 1.0), ph),
            Item::Tipped(a) => cup(s, PointKind::Decision, (x, 1.0), 1.0, a),
        }
    }
    s.push_str("</g>");
    // Steam is drawn upright, whatever the tray's tilt.
    for &(x, item) in &tr.items {
        if let Item::Cup(kind, k) = item
            && k > 0.0
            && kind == PointKind::Decision
        {
            steam(s, tray_point(tr, (x, -15.5)), 1.0, t, k);
        }
    }
}

/// The kyusu, its handle end in his hand at `g`, tilted `ang` degrees.
fn pot(s: &mut String, g: P, ang: f32) {
    let _ = write!(
        s,
        r#"<g transform="translate({}) rotate({})">"#,
        xy(g),
        n(ang)
    );
    pot_shape(s);
    s.push_str("</g>");
}

/// The kyusu standing on the tray at tray-local `x`.
fn pot_on_tray(s: &mut String, x: f32) {
    let _ = write!(
        s,
        r#"<g transform="translate({} -7.6)">"#,
        n(x - POT_BODY.0)
    );
    pot_shape(s);
    s.push_str("</g>");
}

/// A squat clay kyusu with a straight side handle, spout to the right
/// (origin at the handle's end).
fn pot_shape(s: &mut String) {
    let _ = write!(
        s,
        r#"<path d="M1 0L12 0" stroke="{OL}" stroke-width="5.6"/><path d="M1 0L12 0" stroke="{CLAY_SH}" stroke-width="2.8"/><path d="M29 -3L37.5 -9.5Q39.5 -9.5 39 -7.5L31 3Z" fill="{CLAY}" stroke="{OL}" stroke-width="1.7"/><path d="M11 -1C11 -8 15 -9 21 -9C27 -9 31 -8 31 -1C31 5 27 8 21 8C15 8 11 5 11 -1Z" fill="{CLAY}" stroke="{OL}" stroke-width="1.9"/><path d="M13.5 2.5Q21 7 28.5 2.5" stroke="{CLAY_SH}" stroke-width="1.6" fill="none"/><ellipse cx="21" cy="-8.4" rx="6.6" ry="1.9" fill="{CLAY_HI}" stroke="{OL}" stroke-width="1.5"/><circle cx="21" cy="-10.6" r="1.9" fill="{CLAY}" stroke="{OL}" stroke-width="1.3"/><path d="M14.5 -5Q15.5 -7 18 -7.2" stroke="{CLAY_HI}" stroke-width="1.6" fill="none"/>"#
    );
}

/// One teacup, its kind told by its shape: a cream yunomi with an amber
/// band (decision), a coral matcha bowl with a whisk standing in it
/// (action), a glass of iced tea with ice and a straw (question), a small
/// teal cup with a tea stalk standing up in it (fact). `base` is the
/// middle of its foot; `k` its scale; `tilt` tips it over (degrees).
fn cup(s: &mut String, kind: PointKind, base: P, k: f32, tilt: f32) {
    let _ = write!(
        s,
        r#"<g transform="translate({}) scale({k:.3}) rotate({})">"#,
        xy(base),
        n(tilt)
    );
    match kind {
        PointKind::Decision => {
            let _ = write!(
                s,
                r#"<path d="M-6.4 -15L6.4 -15L5.5 -1.8Q5.3 0 3.6 0L-3.6 0Q-5.3 0 -5.5 -1.8Z" fill="{CREAM}" stroke="{OL}" stroke-width="1.6"/><path d="M-6 -9.6L6 -9.6L5.7 -5.6L-5.7 -5.6Z" fill="{GOLD}"/><path d="M3.6 -14.2L3.1 -1" stroke="{CREAM_DEEP}" stroke-width="1.6"/><path d="M-3.9 -13L-3.6 -11" stroke="white" stroke-width="1.2"/><ellipse cy="-15" rx="6.4" ry="1.9" fill="{TEA}" stroke="{OL}" stroke-width="1.4"/>"#
            );
        }
        PointKind::Action => {
            let _ = write!(
                s,
                r#"<path d="M-9.6 -9Q-9.6 -1.6 -4 -.7L-3.6 0L3.6 0L4 -.7Q9.6 -1.6 9.6 -9Z" fill="{CORAL}" stroke="{OL}" stroke-width="1.6"/><path d="M-9.4 -8.4L9.4 -8.4Q9 -6 7 -6.3Q6 -4.5 4.6 -6.2Q2 -5.4 0 -6.3Q-3 -5 -4.5 -6.4Q-7.5 -5.4 -9.2 -6.8Z" fill="{CREAM_DEEP}"/><path d="M6.5 -4.5Q6 -2.2 4 -1.6" stroke="{EMBER}" stroke-width="1.1" fill="none" opacity=".6"/><ellipse cy="-9" rx="9.6" ry="2.4" fill="{MATCHA}" stroke="{OL}" stroke-width="1.4"/><ellipse cx="-2" cy="-9.2" rx="4" ry="1" fill="{MATCHA_L}"/><path d="M-1.4 -9.4L.3 -14.5L3.4 -13.6L2.6 -9.2Z" fill="{BAMBOO}" stroke="{OL}" stroke-width="1.2"/><path d="M1.8 -14L5.6 -22.5" stroke="{OL}" stroke-width="4"/><path d="M1.8 -14L5.6 -22.5" stroke="{BAMBOO}" stroke-width="2"/>"#
            );
        }
        PointKind::Question => {
            let _ = write!(
                s,
                r##"<path d="M1.8 -10L5.8 -22" stroke="{OL}" stroke-width="3.6"/><path d="M1.8 -10L5.8 -22" stroke="{TERRA}" stroke-width="1.8"/><path d="M-6.4 -16L6.4 -16L5.2 0L-5.2 0Z" fill="{GLASS}" stroke="{OL}" stroke-width="1.6"/><path d="M-6 -10.6L6 -10.6L5.2 -1L-5.2 -1Z" fill="#CDB2E6"/><rect x="-4.6" y="-14.2" width="4.8" height="4.8" rx="1" transform="rotate(-12 -2.2 -11.8)" fill="white" stroke="{ICE}" stroke-width="1"/><rect x=".2" y="-12.6" width="4.4" height="4.4" rx="1" transform="rotate(16 2.4 -10.4)" fill="white" stroke="{ICE}" stroke-width="1" opacity=".95"/><path d="M-4.4 -8L-3.8 -2.4" stroke="white" stroke-width="1.3"/><ellipse cy="-16" rx="6.4" ry="1.5" fill="none" stroke="{OL}" stroke-width="1.3"/>"##
            );
        }
        // A key point is the same leaf cup, glazed blue.
        PointKind::Fact | PointKind::Point => {
            let (body, band) = if kind == PointKind::Point {
                ("#A9C4FF", "#7A98D6")
            } else {
                ("#8FD3D4", "#5FA9AC")
            };
            let _ = write!(
                s,
                r##"<path d="M-6.6 -11Q-6.9 -2 -3 -.4L-2.6 0L2.6 0L3 -.4Q6.9 -2 6.6 -11Z" fill="{body}" stroke="{OL}" stroke-width="1.6"/><path d="M-6.4 -6.4Q0 -4.6 6.4 -6.4" stroke="{band}" stroke-width="1.4" fill="none"/><path d="M-4.2 -9L-3.9 -7.2" stroke="white" stroke-width="1.2"/><ellipse cy="-11" rx="6.6" ry="1.8" fill="#D8B95A" stroke="{OL}" stroke-width="1.4"/><path d="M.4 -11.2L1 -15.6" stroke="{OL}" stroke-width="2.6"/><path d="M.4 -11.2L1 -15.6" stroke="{LEAF}" stroke-width="1.1"/><path d="M1 -15Q4.6 -20.6 8 -17Q4.2 -13.4 1 -15Z" fill="{LEAF}" stroke="{OL}" stroke-width="1.2"/><path d="M1.6 -15.2Q4.6 -16.6 6.6 -16.8" stroke="#A7CF7A" stroke-width=".8" fill="none"/>"##
            );
        }
    }
    s.push_str("</g>");
}

/// Three wisps of steam rising from `base`, `k` their scale, `strength`
/// their opacity (0..1). They loop with `t`.
fn steam(s: &mut String, base: P, k: f32, t: f32, strength: f32) {
    if strength <= 0.0 {
        return;
    }
    for i in 0..3 {
        let u = (t * 2.0 + i as f32 / 3.0) % 1.0;
        let op = strength * (PI * u).sin().powf(0.8);
        if op < 0.03 {
            continue;
        }
        let x = base.0 + (i as f32 - 1.0) * 3.0 * k + 1.4 * k * wave(u, 1.0, i as f32 * 0.3);
        let y = base.1 - (1.0 + 13.0 * u) * k;
        let h = (4.0 + 2.0 * u) * k;
        let d = format!(
            "M{}q{} {} 0 {}t0 {}",
            xy((x, y)),
            n(-2.0 * k),
            n(-h / 2.0),
            n(-h),
            n(-h)
        );
        let _ = write!(
            s,
            r#"<g opacity="{op:.2}" fill="none"><path d="{d}" stroke="{OL}" stroke-width="{}" opacity=".45"/><path d="{d}" stroke="{STEAM}" stroke-width="{}"/></g>"#,
            n(3.8 * k),
            n(2.0 * k)
        );
    }
}

/// Frost glinting round an iced glass whose foot is at `base`.
fn frost(s: &mut String, base: P, k: f32, t: f32) {
    for i in 0..3 {
        let u = (t + i as f32 / 3.0) % 1.0;
        let op = (PI * u).sin();
        let p = (
            base.0 + [-8.5, 8.0, -7.0][i] * k,
            base.1 - [13.0, 7.0, 4.0][i] * k,
        );
        let r = (0.7 + 1.0 * op) * k;
        if op < 0.05 {
            continue;
        }
        let _ = write!(
            s,
            r#"<path d="{}" fill="white" stroke="{ICE}" stroke-width="{}" opacity="{op:.2}"/>"#,
            sparkle_d(p, r * 1.4),
            n(0.8 * k)
        );
    }
}

/// The tea pouring from the spout into the cup's mouth, flowing.
fn stream(s: &mut String, a: P, b: P, t: f32) {
    let c = (b.0, a.1 + 1.0);
    let d = format!("M{}Q{} {}", xy(a), xy(c), xy(b));
    let _ = write!(
        s,
        r##"<path d="{d}" stroke="{OL}" stroke-width="4.2" fill="none"/><path d="{d}" stroke="#D3DE8C" stroke-width="2.2" fill="none"/><path d="{d}" stroke="#F4F7D2" stroke-width="1" fill="none" stroke-dasharray="3 5" stroke-dashoffset="{}"/><ellipse cx="{}" cy="{}" rx="3" ry=".9" fill="#F4F7D2" stroke="{OL}" stroke-width=".8"/>"##,
        n(-16.0 * t),
        n(b.0),
        n(b.1 + 0.6)
    );
}

/// The little steeping hourglass: the sand runs down, then it is turned
/// over, which brings it back to where it started.
fn hourglass(s: &mut String, base: P, t: f32) {
    let flow = (t / 0.8).min(1.0);
    let flip = ((t - 0.8) / 0.2).clamp(0.0, 1.0);
    let flip = 0.5 - 0.5 * (PI * flip).cos();
    let top = 1.0 - flow;
    let _ = write!(
        s,
        r#"<g transform="translate({}) rotate({} 0 -11)"><clipPath id="kgl"><path d="M-4 -19.4Q-4 -13 -.8 -11Q-4 -9 -4 -2.6L4 -2.6Q4 -9 .8 -11Q4 -13 4 -19.4Z"/></clipPath><path d="M-4 -19.4Q-4 -13 -.8 -11Q-4 -9 -4 -2.6L4 -2.6Q4 -9 .8 -11Q4 -13 4 -19.4Z" fill="{GLASS}"/><g clip-path="url(#kgl)" fill="{GOLD_L}"><rect x="-5" y="{}" width="10" height="{}"/><path d="M-5 -2.6L-5 {}Q0 {} 5 {}L5 -2.6Z"/></g>"#,
        xy(base),
        n(180.0 * flip),
        n(-11.8 - 7.0 * top),
        n(7.0 * top + 0.6),
        n(-2.6 - 6.0 * flow),
        n(-2.6 - 8.5 * flow),
        n(-2.6 - 6.0 * flow)
    );
    if flow < 1.0 {
        let _ = write!(
            s,
            r#"<path d="M0 -11L0 {}" stroke="{GOLD}" stroke-width=".8"/>"#,
            n(-3.0 - 6.0 * flow)
        );
    }
    let _ = write!(
        s,
        r#"<path d="M-4 -19.4Q-4 -13 -.8 -11Q-4 -9 -4 -2.6M4 -2.6Q4 -9 .8 -11Q4 -13 4 -19.4" stroke="{OL}" stroke-width="1.3" fill="none"/><path d="M-5 -19.4V-2.6M5 -19.4V-2.6" stroke="{OL}" stroke-width="2.6"/><path d="M-5 -19.4V-2.6M5 -19.4V-2.6" stroke="{WOOD}" stroke-width="1.2"/><rect x="-6.5" y="-2.8" width="13" height="2.8" rx="1" fill="{WOOD}" stroke="{OL}" stroke-width="1.3"/><rect x="-6.5" y="-22" width="13" height="2.8" rx="1" fill="{WOOD}" stroke="{OL}" stroke-width="1.3"/></g>"#
    );
}

/// A round iron tetsubin kettle standing at `base`, steam curling from
/// its spout.
fn kettle(s: &mut String, base: P, t: f32) {
    let _ = write!(
        s,
        r#"<g transform="translate({})" stroke-linejoin="round" stroke-linecap="round"><path d="M-9 -17Q-9 -31 0 -31Q9 -31 9 -17" stroke="{OL}" stroke-width="3.8" fill="none"/><path d="M-9 -17Q-9 -31 0 -31Q9 -31 9 -17" stroke="{IRON_HI}" stroke-width="1.8" fill="none"/><path d="M-11 -9L-19.5 -15.5Q-21 -15 -20 -13L-12 -3Z" fill="{IRON}" stroke="{OL}" stroke-width="1.7"/><path d="M-12 -1C-16 -9 -11 -18 0 -18C11 -18 16 -9 12 -1Q0 2 -12 -1Z" fill="{IRON}" stroke="{OL}" stroke-width="1.9"/><ellipse cy="-17.6" rx="6.2" ry="1.9" fill="{IRON_HI}" stroke="{OL}" stroke-width="1.4"/><circle cy="-19.8" r="1.8" fill="{IRON}" stroke="{OL}" stroke-width="1.2"/>"#,
        xy(base)
    );
    for (x, y) in [
        (-6.0, -12.0),
        (-2.0, -13.0),
        (2.0, -13.0),
        (6.0, -12.0),
        (-4.0, -8.5),
        (0.0, -9.0),
        (4.0, -8.5),
    ] {
        let _ = write!(s, r#"<circle cx="{x}" cy="{y}" r=".9" fill="{IRON_HI}"/>"#);
    }
    s.push_str(r##"<ellipse cy="0" rx="13" ry="2.4" fill="#2B1730" opacity=".15"/></g>"##);
    steam(s, (base.0 - 20.0, base.1 - 14.0), 0.9, t, 0.8);
}

fn back_hair(s: &mut String, r: &Rig) {
    // The tails of his headband, knotted at his left temple.
    for (k, (a, l)) in [(-6.0f32, 20.0f32), (14.0, 16.0)].into_iter().enumerate() {
        let _ = write!(
            s,
            r#"<g transform="translate(135 58) rotate({})"><path d="M0 -2C5 0 8 {} 9 {}L4.5 {}C4 {} 2 4 -1 2.5Z" fill="{INDIGO}" stroke="{OL}" stroke-width="1.8"/><path d="M2 1Q5 {} 6 {}" stroke="{CREAM}" stroke-width="1" stroke-dasharray="1.2 2.6" fill="none"/></g>"#,
            n(a + r.swing * (1.0 + k as f32 * 0.3)),
            n(l * 0.4),
            n(l),
            n(l - 0.5),
            n(l * 0.5),
            n(l * 0.5),
            n(l - 1.5)
        );
    }
    let _ = write!(
        s,
        r#"<path d="M100 38C74 38 62 55 63 79C63 88 65 94 68 98L132 98C135 94 137 88 137 79C138 55 126 38 100 38Z" fill="{HAIR_SH}" stroke="{OL}" stroke-width="2.2"/>"#
    );
}

fn head(s: &mut String, r: &Rig) {
    // Ears.
    for m in [-1.0f32, 1.0] {
        let x = 100.0 + m * 32.5;
        let _ = write!(
            s,
            r#"<ellipse cx="{}" cy="92" rx="4.4" ry="6" fill="{SKIN}" stroke="{OL}" stroke-width="1.9"/><path d="M{} 89.5Q{} 92 {} 95" stroke="{SKIN_SH}" stroke-width="1.4" fill="none"/>"#,
            n(x),
            n(x + m * 0.6),
            n(x + m * 2.2),
            n(x + m * 0.4)
        );
    }
    // Face.
    let _ = write!(
        s,
        r#"<path d="M68 77C68 45 132 45 132 77C132 99 116 112.5 100 113C84 112.5 68 99 68 77Z" fill="{SKIN}" stroke="{OL}" stroke-width="2.2"/>"#
    );
    if r.blush > 0.0 {
        let _ = write!(
            s,
            r#"<g fill="{BLUSH}" opacity="{:.2}"><ellipse cx="79" cy="102" rx="5.6" ry="2.7"/><ellipse cx="121" cy="102" rx="5.6" ry="2.7"/></g>"#,
            r.blush
        );
    }
    eye(s, r, 86.0, -1.0);
    eye(s, r, 114.0, 1.0);
    // Nose.
    let _ = write!(
        s,
        r#"<path d="M100.5 99l-.8 1.3" stroke="{SKIN_SH}" stroke-width="1.4"/>"#
    );
    mouth(s, r.mouth);
    // Brows, under the fringe, peeking between its locks.
    let (li, lo, arch) = match r.brow {
        Brow::Soft => (78.6, 79.0, -1.2),
        Brow::Raised => (77.0, 77.4, -2.0),
        Brow::Worried => (76.0, 79.6, 0.6),
        Brow::Sleepy => (79.6, 79.6, -0.8),
    };
    for m in [-1.0f32, 1.0] {
        let inner = 100.0 + m * 8.0;
        let outer = 100.0 + m * 19.0;
        let _ = write!(
            s,
            r#"<path d="M{} {}Q{} {} {} {}" stroke="{BROW}" stroke-width="2.1" fill="none"/>"#,
            n(inner),
            n(li),
            n((inner + outer) / 2.0),
            n((li + lo) / 2.0 + arch),
            n(outer),
            n(lo)
        );
    }
    // Tidy short hair: a round crown, a neat fringe of pointed locks and
    // short side locks over the ears.
    let _ = write!(
        s,
        r#"<path d="M64 84C61 52 78 38 100 38C122 38 139 52 136 84C135.6 88 134.6 91 133 94.5C131 89.5 129 85 127.5 80Q126 74 122 69Q120 73.5 114 75.5Q112 68 107.5 64Q105 71 99.5 75Q97.5 67.5 93 64.5Q90 71.5 85 74.5Q83 69.5 80 67Q76.5 73 74 80C72 85 70.2 89.5 67 94.5C65.4 91 64.4 88 64 84Z" fill="url(#khg)" stroke="{OL}" stroke-width="2.2"/>"#
    );
    let _ = write!(
        s,
        r#"<path d="M107 64.5Q105 56 100 50M93 65Q92 57 89 51M121.5 68Q120 60 115 54M80 67.5Q80 61 82 56M132.5 92Q131.5 85 128.5 78M67.5 92Q68.5 85 71.5 78" stroke="{HAIR_SH}" stroke-width="1.2" fill="none"/><path d="M98 39Q97 31 103 28Q100 33 104 38" fill="url(#khg)" stroke="{OL}" stroke-width="1.8"/><path d="M77 50Q87 43.5 96 43M104 43Q114 43.5 123 50" stroke="{HAIR_HI}" stroke-width="2.6" fill="none"/>"#
    );
    // The tenugui headband: cream cotton with indigo stripes, knotted at
    // the side.
    let _ = write!(
        s,
        r#"<path d="M63.6 54.5Q100 65.5 136.4 54.5L136.8 61Q100 72.5 63.2 61Z" fill="{INDIGO}" stroke="{OL}" stroke-width="2"/><path d="M66 58Q100 68.8 134 58" stroke="{CREAM}" stroke-width="1.3" stroke-dasharray="1.2 3.2" fill="none"/><path d="M76 56.4Q88 60 99 60.4" stroke="{INDIGO_HI}" stroke-width="1.4" fill="none"/><path d="M130.5 55.5L139 52.5L140.5 61.5L131.5 63.5Z" fill="{INDIGO_HI}" stroke="{OL}" stroke-width="1.7"/><path d="M134 55L135.4 61.8" stroke="{CREAM}" stroke-width="1" stroke-dasharray="1.2 2.6"/>"#
    );
    if let Extra::Spill(t) = r.extra {
        let dy = 3.0 * t;
        let _ = write!(
            s,
            r#"<g transform="translate(139 {})"><path d="M0 -6C3 -1.5 4.4 1.5 2.6 4C1 6 -1 6 -2.6 4C-4.4 1.5 -3 -1.5 0 -6Z" fill="{SWEAT}" stroke="{OL}" stroke-width="1.3"/><ellipse cx="-.9" cy="1.6" rx=".9" ry="1.5" fill="white"/></g>"#,
            n(74.0 + dy)
        );
    }
}

fn eye(s: &mut String, r: &Rig, cx: f32, m: f32) {
    let cy = 91.0;
    let closed = |s: &mut String| {
        let _ = write!(
            s,
            r#"<path d="M{} {}Q{} {} {} {}" stroke="{OL}" stroke-width="2.3" fill="none"/>"#,
            n(cx - m * 7.0),
            n(cy + 1.5),
            n(cx),
            n(cy + 5.5),
            n(cx + m * 7.5),
            n(cy + 1.5)
        );
    };
    match r.eyes {
        Eyes::Closed => closed(s),
        Eyes::Happy => {
            let _ = write!(
                s,
                r#"<path d="M{} {}Q{} {} {} {}" stroke="{OL}" stroke-width="2.4" fill="none"/>"#,
                n(cx - m * 7.0),
                n(cy + 3.5),
                n(cx),
                n(cy - 4.5),
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
            let (ry, irx, iry) = if wide {
                (10.0, 5.4, 7.0)
            } else {
                (9.4, 6.0, 7.8)
            };
            let ix = cx + r.look.0;
            let iy = cy + 1.0 + r.look.1;
            let id = if m < 0.0 { "kel" } else { "ker" };
            let _ = write!(
                s,
                r#"<clipPath id="{id}"><ellipse cx="{}" cy="{}" rx="7.6" ry="{ry}"/></clipPath><ellipse cx="{}" cy="{}" rx="7.6" ry="{ry}" fill="white"/><g clip-path="url(#{id})"><ellipse cx="{}" cy="{}" rx="{irx}" ry="{iry}" fill="url(#kir)" stroke="{OL}" stroke-width=".9"/><ellipse cx="{}" cy="{}" rx="2.6" ry="3.7" fill="{OL}"/><path d="{}" fill="white"/><circle cx="{}" cy="{}" r="1.1" fill="white" opacity=".9"/>"#,
                n(cx),
                n(cy),
                n(cx),
                n(cy),
                n(ix),
                n(iy),
                n(ix),
                n(iy + 0.4),
                sparkle_d((ix - 2.1, iy - 2.8), 3.0),
                n(ix + 2.3),
                n(iy + 3.2)
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
                // A strong, straight upper lash line: calm boy's eyes.
                let _ = write!(
                    s,
                    r#"<path d="M{} {}Q{} {} {} {}L{} {}" stroke="{OL}" stroke-width="2.8" fill="none"/>"#,
                    n(cx - m * 7.8),
                    n(yl + 2.0),
                    n(cx - m * 0.5),
                    n(yl - 2.4),
                    n(cx + m * 8.2),
                    n(yl + 0.4),
                    n(cx + m * 9.0),
                    n(yl + 0.6)
                );
                let _ = write!(
                    s,
                    r##"<path d="M{} {}Q{} {} {} {}" stroke="#6E5560" stroke-width="1.1" fill="none"/>"##,
                    n(cx - m * 3.0),
                    n(cy + ry - 0.2),
                    n(cx + m * 1.0),
                    n(cy + ry + 0.6),
                    n(cx + m * 5.0),
                    n(cy + ry - 1.4)
                );
            }
        }
    }
}

fn mouth(s: &mut String, m: Mouth) {
    match m {
        Mouth::Smile => {
            let _ = write!(
                s,
                r#"<path d="M96.8 105.2Q100 107.8 103.2 105.2" stroke="{OL}" stroke-width="1.6" fill="none"/>"#
            );
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
            let _ = write!(
                s,
                r#"<ellipse cx="100" cy="106" rx="{}" ry="{}" fill="{MOUTH}" stroke="{OL}" stroke-width="1.3"/>"#,
                n(r * 0.85),
                n(r)
            );
        }
        Mouth::Hmm => {
            let _ = write!(
                s,
                r#"<path d="M97 106Q99 104.8 103 106.2" stroke="{OL}" stroke-width="1.6" fill="none"/>"#
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

/// A small tea leaf at `c`, turned `rot` degrees.
fn leaf(s: &mut String, c: P, r: f32, rot: f32, op: f32) {
    let _ = write!(
        s,
        r#"<g transform="translate({}) rotate({}) scale({})" opacity="{op:.2}"><path d="M-5 0Q0 -4.6 5 0Q0 4.6 -5 0Z" fill="{LEAF}" stroke="{OL}" stroke-width="1.3"/><path d="M-4 0H3.6" stroke="{LEAF_HI}" stroke-width=".9"/></g>"#,
        xy(c),
        n(rot),
        n(r)
    );
}

fn extras(s: &mut String, r: &Rig, tray: Option<&Tray>) {
    match r.extra {
        Extra::None => {}
        Extra::Listen(t) => {
            // Sound arcs ringing out by his ear.
            let ear = head_point(r, (60.0, 88.0));
            for k in 0..3 {
                let u = (t * 2.0 + k as f32 / 3.0) % 1.0;
                let rad = 5.0 + 12.0 * u;
                let op = (PI * u).sin();
                let a = add(ear, dir(150.0), rad);
                let b = add(ear, dir(210.0), rad);
                let _ = write!(
                    s,
                    r#"<path d="M{}A{} {} 0 0 0 {}" stroke="{OL}" stroke-width="2" fill="none" opacity="{op:.2}"/>"#,
                    xy(a),
                    n(rad),
                    n(rad),
                    xy(b)
                );
            }
        }
        Extra::Leaves(t) => {
            // Tea leaves drifting round over his head as the tea steeps.
            let c = (152.0, 52.0);
            let lift = 1.2 * wave(t, 1.0, 0.0);
            let _ = write!(
                s,
                r##"<circle cx="{}" cy="{}" r="2.2" fill="white" stroke="#2B1730" stroke-width="1.2"/><circle cx="{}" cy="{}" r="3.2" fill="white" stroke="#2B1730" stroke-width="1.3"/><ellipse cx="{}" cy="{}" rx="19" ry="12" fill="white" stroke="#2B1730" stroke-width="1.8"/><ellipse cx="{}" cy="{}" rx="13" ry="6" fill="#E9F0D2"/>"##,
                n(133.0),
                n(72.0 + lift),
                n(140.0),
                n(64.0 + lift),
                n(c.0),
                n(c.1 + lift),
                n(c.0),
                n(c.1 + lift + 2.0)
            );
            for i in 0..3 {
                let a = TAU * (t + i as f32 / 3.0);
                let p = (c.0 + 10.0 * a.cos(), c.1 + lift + 1.0 + 4.0 * a.sin());
                leaf(s, p, 0.75, a.to_degrees() * 0.5 + 30.0, 1.0);
            }
        }
        Extra::Zz(t) => {
            for k in 0..3 {
                let u = (t + k as f32 / 3.0) % 1.0;
                let x = 140.0 + u * 16.0 + 3.0 * (TAU * u).sin();
                let y = 140.0 - u * 50.0;
                let z = 4.0 + u * 4.0;
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
                    r#"<g opacity="{op:.2}"><path d="{d}" stroke="{OL}" stroke-width="{}" fill="none"/><path d="{d}" stroke="{CREAM}" stroke-width="{}" fill="none"/></g>"#,
                    n(2.0 + z * 0.35),
                    n(z * 0.28)
                );
            }
        }
        Extra::Spill(t) => {
            // Tea splashing out of the toppling cup and dripping down.
            if let Some(tr) = tray
                && let Some(&(x, Item::Tipped(a))) = tr.items.first()
            {
                let lip = tray_point(tr, add((x, 1.0), turn((5.0, -15.0), a), 1.0));
                for k in 0..4 {
                    let u = (t + k as f32 / 4.0) % 1.0;
                    let vx = [7.0, 12.0, 4.0, 10.0][k];
                    let p = (lip.0 + vx * u, lip.1 - 8.0 * u + 60.0 * u * u);
                    let op = (PI * u).sin().powf(0.5);
                    let rr = [2.6, 2.0, 1.6, 2.2][k];
                    let _ = write!(
                        s,
                        r##"<path d="M{}c{} {} {} {} 0 {}c{} 0 {} {} 0 {}Z" fill="#D3DE8C" stroke="{OL}" stroke-width="1.1" opacity="{op:.2}"/>"##,
                        xy((p.0, p.1 - rr * 1.6)),
                        n(rr * 0.6),
                        n(rr),
                        n(rr),
                        n(rr * 1.6),
                        n(rr * 2.6),
                        n(-rr * 1.4),
                        n(-rr * 1.4),
                        n(-rr * 1.0),
                        n(-rr * 2.6)
                    );
                }
                steam(s, lip, 0.8, t, 0.7);
            }
            // Little stress marks at his temple.
            let m = head_point(r, (58.0, 66.0));
            let _ = write!(
                s,
                r#"<path d="M{}l-6-4M{}h-7M{}l-6 3" stroke="{OL}" stroke-width="1.8"/>"#,
                xy(m),
                xy((m.0 - 2.0, m.1 + 8.0)),
                xy((m.0, m.1 + 16.0))
            );
        }
    }
}

// ---------------------------------------------------------------- the shelf

/// Size of the tea-shelf panel `notes_svg` draws (its viewBox).
pub const NOTES_W: f32 = 240.0;
pub const NOTES_H: f32 = 160.0;
/// Frames in one steam loop of the shelf (2 s at `FPS`).
pub const NOTES_FRAMES: usize = 24;
/// Cups on one tray: one stretch of the meeting.
const GROUP: usize = 5;
/// Where trays may go: the wall under the beam, inside a margin.
const AREA: (f32, f32, f32, f32) = (10.0, 20.0, 230.0, 154.0);
/// At scale 1: room per cup on a tray, a tray's end margin, the gap
/// between trays, and the height of one shelf row (steam, cup, tray,
/// plank).
const SLOT: f32 = 22.0;
const TRAY_PAD: f32 = 5.0;
const TRAY_GAP: f32 = 10.0;
const ROW_H: f32 = 42.0;
const TRAY_H: f32 = 4.5;
const PLANK_H: f32 = 5.0;
const MAX_SCALE: f32 = 1.8;

/// Where the cups, trays and shelves go for `count` notes.
struct Shelf {
    /// Each cup's foot, in note order.
    cups: Vec<P>,
    /// Each tray: left edge, top, width.
    trays: Vec<(f32, f32, f32)>,
    /// Each shelf plank's top.
    planks: Vec<f32>,
    /// The scale everything is drawn at.
    k: f32,
}

/// Lays the trays out in shelf rows, picking the number of rows that lets
/// the cups be drawn largest. Deterministic: the same notes always make
/// the same shelf.
fn layout(count: usize) -> Shelf {
    let trays = count.div_ceil(GROUP);
    let (x0, y0, x1, y1) = AREA;
    let (w, h) = (x1 - x0, y1 - y0);
    let tray_w = |cups: usize| cups as f32 * SLOT + 2.0 * TRAY_PAD;
    let mut best = (1usize, 0.0f32);
    for rows in 1..=trays.max(1) {
        let cols = trays.div_ceil(rows).max(1);
        let need = cols as f32 * tray_w(GROUP.min(count.max(1))) + (cols - 1) as f32 * TRAY_GAP;
        let k = (w / need).min(h / (rows as f32 * ROW_H)).min(MAX_SCALE);
        if k > best.1 + 0.001 {
            best = (rows, k);
        }
    }
    let (rows, k) = best;
    let cols = trays.div_ceil(rows).max(1);
    // Rows no taller than they need (with some air), centred on the wall.
    let row_h = (h / rows as f32).min(ROW_H * k * 1.2);
    let y0 = y0 + (h - rows as f32 * row_h) / 2.0;
    let mut shelf = Shelf {
        cups: Vec::with_capacity(count),
        trays: Vec::new(),
        planks: Vec::new(),
        k,
    };
    for row in 0..rows {
        let plank = y0 + (row + 1) as f32 * row_h - PLANK_H * k;
        shelf.planks.push(plank);
        let first = row * cols;
        let last = ((row + 1) * cols).min(trays);
        if first >= last {
            continue;
        }
        let sizes: Vec<usize> = (first..last)
            .map(|g| (count - g * GROUP).min(GROUP))
            .collect();
        let total: f32 = sizes.iter().map(|&c| tray_w(c) * k).sum::<f32>()
            + (sizes.len() - 1) as f32 * TRAY_GAP * k;
        let mut x = NOTES_W / 2.0 - total / 2.0;
        let top = plank - TRAY_H * k;
        for size in sizes {
            shelf.trays.push((x, top, tray_w(size) * k));
            for j in 0..size {
                shelf
                    .cups
                    .push((x + (TRAY_PAD + (j as f32 + 0.5) * SLOT) * k, top + 1.0 * k));
            }
            x += (tray_w(size) + TRAY_GAP) * k;
        }
    }
    shelf
}

/// Kaito's tea shelf: one cup per note, in order, five to a lacquered tray,
/// the trays set out on shelf planks (more rows as the meeting goes on).
/// Decision cups steam, iced glasses glint with frost; the newest cup is
/// set down again every loop with a little puff. A standalone SVG document
/// of `NOTES_W` × `NOTES_H`, looping over `NOTES_FRAMES`.
pub fn notes_svg(notes: &[PointKind], frame: usize) -> String {
    let f = frame % NOTES_FRAMES;
    let t = f as f32 / NOTES_FRAMES as f32;
    let (w, h) = (NOTES_W, NOTES_H);
    let mut s = String::with_capacity(6144 + notes.len() * 1800);
    let _ = write!(
        s,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}"><defs><linearGradient id="kwall" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#F7ECD8"/><stop offset="1" stop-color="#EBD7B6"/></linearGradient><clipPath id="kpanel"><rect width="{w}" height="{h}" rx="14"/></clipPath></defs><g clip-path="url(#kpanel)" stroke-linecap="round" stroke-linejoin="round"><rect width="{w}" height="{h}" fill="url(#kwall)"/>"##
    );
    // Shoji lattice on the wall under a dark wooden beam.
    let mut d = String::new();
    for i in 0..6 {
        let _ = write!(d, "M{} 12V{}", n(20.0 + i as f32 * 40.0), n(h));
    }
    for i in 0..6 {
        let _ = write!(d, "M0 {}H{}", n(36.0 + i as f32 * 26.0), n(w));
    }
    let _ = write!(
        s,
        r##"<path d="{d}" stroke="#D9C09A" stroke-width="1.2" opacity=".5"/><rect width="{w}" height="12" fill="#6B4A3A"/><path d="M0 9H{w}" stroke="#86604C" stroke-width="1.2"/><path d="M0 12H{w}" stroke="#2B1730" stroke-width="1.6"/>"##
    );
    let shelf = layout(notes.len());
    let k = shelf.k;
    // Shelf planks.
    for &py in &shelf.planks {
        let ph = PLANK_H * k;
        let _ = write!(
            s,
            r#"<rect x="-2" y="{}" width="{}" height="{}" fill="{WOOD}" stroke="{OL}" stroke-width="{}"/><path d="M0 {}H{}" stroke="{WOOD_HI}" stroke-width="{}"/><rect x="0" y="{}" width="{}" height="{}" fill="{WOOD_SH}" opacity=".35"/>"#,
            n(py),
            n(w + 4.0),
            n(ph),
            n((1.5 * k).max(0.8)),
            n(py + ph * 0.3),
            n(w),
            n((0.9 * k).max(0.5)),
            n(py + ph),
            n(w),
            n((2.5 * k).max(1.0))
        );
        // Two brackets holding it up.
        let b = (6.0 * k).clamp(3.0, 9.0);
        for x in [26.0, w - 26.0] {
            let _ = write!(
                s,
                r#"<path d="M{}h{}l-{} {}Z" fill="{WOOD_SH}" stroke="{OL}" stroke-width="{}"/>"#,
                xy((x - b / 2.0, py + ph)),
                n(b),
                n(b * 0.6),
                n(b),
                n((1.2 * k).max(0.7))
            );
        }
    }
    if notes.is_empty() {
        // Nothing noted yet: the kettle on the counter, waiting.
        kettle(&mut s, (120.0, shelf.planks[0] - 0.5), t);
    }
    // Trays.
    for &(x, top, tw) in &shelf.trays {
        let th = TRAY_H * k;
        let _ = write!(
            s,
            r#"<path d="M{}L{}L{}L{}Z" fill="{TRAY}" stroke="{OL}" stroke-width="{}"/><path d="M{}H{}" stroke="{TRAY_HI}" stroke-width="{}"/>"#,
            xy((x, top - 0.6 * k)),
            xy((x + tw, top - 0.6 * k)),
            xy((x + tw - 1.5 * k, top + th)),
            xy((x + 1.5 * k, top + th)),
            n((1.5 * k).max(0.7)),
            xy((x + 2.0 * k, top + 0.8 * k)),
            n(x + tw - 2.0 * k),
            n((0.9 * k).max(0.4))
        );
    }
    // Cups, oldest first; the newest is set down with a puff every loop.
    let newest = notes.len().saturating_sub(1);
    for (i, (&kind, &c)) in notes.iter().zip(&shelf.cups).enumerate() {
        let mut base = c;
        if i == newest {
            let drop = match f {
                0 => 10.0,
                1 => 5.0,
                2 => 1.5,
                _ => 0.0,
            };
            base.1 -= drop * k;
            if (2..10).contains(&f) {
                let u = (f - 2) as f32 / 8.0;
                puff(&mut s, (c.0, c.1 - 1.0 * k), k, u, kind);
            }
        }
        cup(&mut s, kind, base, k, 0.0);
        let ph = (i * 7 % 24) as f32 / 24.0;
        match kind {
            PointKind::Decision => {
                steam(&mut s, (base.0, base.1 - 15.5 * k), k, (t + ph) % 1.0, 0.85)
            }
            PointKind::Question => frost(&mut s, base, k, (t + ph) % 1.0),
            _ => {}
        }
    }
    s.push_str("</g></svg>");
    s
}

/// The little cloud a cup kicks up as it is set down, `u` (0..1) through
/// it: steam for a hot cup, frost for an iced one.
fn puff(s: &mut String, c: P, k: f32, u: f32, kind: PointKind) {
    let op = (1.0 - u) * 0.9;
    let (fill, rim) = match kind {
        PointKind::Question => ("#EAF6FB", ICE),
        _ => ("white", "#C9B9AE"),
    };
    for i in 0..6 {
        let side = if i % 2 == 0 { -1.0 } else { 1.0 };
        let j = (i / 2) as f32;
        let p = (
            c.0 + side * (8.0 + 6.0 * u + 3.0 * j) * k,
            c.1 - (1.0 + 5.0 * j + 4.0 * u) * k,
        );
        let r = (2.6 - 0.5 * j + 1.2 * u) * k;
        let _ = write!(
            s,
            r#"<circle cx="{}" cy="{}" r="{}" fill="{fill}" stroke="{rim}" stroke-width="{}" opacity="{op:.2}"/>"#,
            n(p.0),
            n(p.1),
            n(r),
            n(0.8 * k)
        );
    }
}

/// Frames of `note_pop_svg`, played once (not looped).
pub const POP_FRAMES: usize = 12;
/// Size of the `note_pop_svg` square (its viewBox).
pub const POP_SIZE: f32 = 48.0;

/// A new cup set down on a little wooden coaster: it drops in, lands with
/// a squash and a puff (steam for a hot cup, frost for an iced one) and
/// settles. A `POP_SIZE` square; frames past the last show the last.
pub fn note_pop_svg(kind: PointKind, frame: usize) -> String {
    let f = frame.min(POP_FRAMES - 1);
    let u = f as f32 / (POP_FRAMES - 1) as f32;
    let drop = match f {
        0 => 12.0,
        1 => 7.0,
        2 => 2.5,
        _ => 0.0,
    };
    let squash = match f {
        3 => 0.86,
        4 => 1.06,
        _ => 1.0,
    };
    let k = 1.25;
    let foot = (24.0, 42.0);
    let mut s = String::with_capacity(4096);
    let _ = write!(
        s,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {w}" width="{w}" height="{w}"><g stroke-linecap="round" stroke-linejoin="round">"#,
        w = POP_SIZE
    );
    let _ = write!(
        s,
        r#"<ellipse cx="24" cy="43" rx="14" ry="3.6" fill="{WOOD}" stroke="{OL}" stroke-width="1.5"/><path d="M13 42.2Q24 45.4 35 42.2" stroke="{WOOD_HI}" stroke-width="1" fill="none"/>"#
    );
    if f >= 3 {
        let p = ((f - 3) as f32 / 7.0).min(1.0);
        if p < 1.0 {
            puff(&mut s, (foot.0, foot.1 - 1.0), 1.1, p, kind);
        }
    }
    let _ = write!(
        s,
        r#"<g transform="translate(0 {}) translate(24 42) scale({} {}) translate(-24 -42)">"#,
        n(-drop),
        n(1.0 + (1.0 - squash) * 0.6),
        n(squash)
    );
    cup(&mut s, kind, foot, k, 0.0);
    s.push_str("</g>");
    let t = u * 0.5;
    match kind {
        PointKind::Decision if f >= 3 => steam(&mut s, (foot.0, foot.1 - 15.5 * k), 0.85, t, 1.0),
        PointKind::Question if f >= 3 => frost(&mut s, foot, k, t + 0.2),
        _ => {}
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
                assert!(svg.len() < 30_000, "{pose:?} {f}: {} bytes", svg.len());
            }
        }
    }

    #[test]
    fn frame_size_matches_the_others() {
        assert_eq!((FRAME_W, FRAME_H, FPS), (200.0, 320.0, 12));
    }

    #[test]
    fn poses_loop() {
        for pose in Pose::ALL {
            let count = frame_count(pose);
            assert_eq!(frame_svg(pose, 0), frame_svg(pose, count), "{pose:?}");
            assert_ne!(
                frame_svg(pose, 0),
                frame_svg(pose, count / 2),
                "{pose:?} doesn't move"
            );
        }
    }

    fn mixed(count: usize) -> Vec<PointKind> {
        (0..count)
            .map(|i| PointKind::ALL[(i * 7 + i / 3) % PointKind::ALL.len()])
            .collect()
    }

    #[test]
    fn the_shelf_draws_any_number_of_notes() {
        for count in [0, 1, 2, 5, 6, 20, 47, 120] {
            let notes = mixed(count);
            for f in 0..NOTES_FRAMES {
                let svg = notes_svg(&notes, f);
                assert!(svg.starts_with("<svg") && svg.ends_with("</svg>"));
                assert!(!svg.contains("<text") && !svg.contains("NaN") && !svg.contains("filter"));
            }
            assert_eq!(notes_svg(&notes, 0), notes_svg(&notes, NOTES_FRAMES));
        }
    }

    #[test]
    fn cups_stay_on_the_shelf_and_apart() {
        for count in [1, 5, 20, 47, 120] {
            let shelf = layout(count);
            assert_eq!(shelf.cups.len(), count);
            assert_eq!(shelf.trays.len(), count.div_ceil(GROUP));
            let k = shelf.k;
            assert!(k > 0.3, "{count}: cups too small ({k})");
            for (i, a) in shelf.cups.iter().enumerate() {
                // The widest cup (the bowl) and the tallest (a glass with
                // its straw, or a steaming yunomi) fit in the panel.
                assert!(
                    a.0 - 10.0 * k > 0.0 && a.0 + 10.0 * k < NOTES_W,
                    "{count}: cup {i} at {a:?}"
                );
                assert!(
                    a.1 - 30.0 * k > 12.0 && a.1 < NOTES_H,
                    "{count}: cup {i} at {a:?}"
                );
                for b in &shelf.cups[..i] {
                    let apart = (a.0 - b.0).abs() >= 20.0 * k || (a.1 - b.1).abs() >= 30.0 * k;
                    assert!(apart, "{count}: cups {a:?} and {b:?} overlap");
                }
            }
        }
        let one = layout(1);
        assert!((one.cups[0].0 - NOTES_W / 2.0).abs() < 1.0);
    }

    #[test]
    fn a_cup_pops_for_every_kind() {
        for kind in PointKind::ALL {
            let last = note_pop_svg(kind, POP_FRAMES - 1);
            assert!(last.starts_with("<svg") && last.ends_with("</svg>"));
            assert_eq!(last, note_pop_svg(kind, POP_FRAMES + 5));
            for f in 0..POP_FRAMES {
                let svg = note_pop_svg(kind, f);
                assert!(svg.starts_with("<svg") && !svg.contains("<text") && !svg.contains("NaN"));
            }
            assert_ne!(note_pop_svg(kind, 0), last);
        }
    }
}
