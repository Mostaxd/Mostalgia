//! Writes every companion frame (and each cast member's notes panel) as SVG,
//! straight from the app's artwork code, for the website's sprites.
//! Run: `cargo run --release -- <out-dir>`, then `node ../raster.js` and `python3 ../encode.py`.
#![allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PointKind { Decision, Action, Question, Fact, Point }
impl PointKind { pub const ALL: [PointKind; 5] = [PointKind::Decision, PointKind::Action, PointKind::Question, PointKind::Fact, PointKind::Point]; }
#[path = "../../../../crates/mostalgia-app/src/companion/art.rs"] mod mori;
#[path = "../../../../crates/mostalgia-app/src/companion/cast/hoshi.rs"] mod hoshi;
#[path = "../../../../crates/mostalgia-app/src/companion/cast/jin.rs"] mod jin;
#[path = "../../../../crates/mostalgia-app/src/companion/cast/kaito.rs"] mod kaito;
#[path = "../../../../crates/mostalgia-app/src/companion/cast/tsumugi.rs"] mod tsumugi;
fn notes(count: usize) -> Vec<PointKind> { (0..count).map(|i| PointKind::ALL[(i * 7 + i / 3) % 4]).collect() }
macro_rules! dump { ($m:ident, $out:expr) => {{
  let dir = $out.join(stringify!($m)); std::fs::create_dir_all(&dir).unwrap();
  for pose in $m::Pose::ALL { let name = format!("{pose:?}").to_lowercase();
    for f in 0..$m::frame_count(pose) { std::fs::write(dir.join(format!("{name}-{f:02}.svg")), $m::frame_svg(pose, f)).unwrap(); } }
}}; }
macro_rules! dn { ($m:ident, $out:expr) => {{ let dir = $out.join(stringify!($m));
  for f in 0..$m::NOTES_FRAMES { std::fs::write(dir.join(format!("notes-{f:02}.svg")), $m::notes_svg(&notes(5), f)).unwrap(); } }}; }
fn main() { let out = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
  dump!(mori, out); dump!(hoshi, out); dump!(jin, out); dump!(kaito, out); dump!(tsumugi, out); dn!(hoshi,out); dn!(jin,out); dn!(kaito,out); dn!(tsumugi,out); }
