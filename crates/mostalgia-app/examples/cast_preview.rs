//! Writes every animation frame of the alternative companions (Tsumugi and
//! Hoshi) and their notes artwork as SVG, for previewing without the app.
//! Run with:
//!
//!     cargo run -p mostalgia-app --example cast_preview -- <out-dir>
//!
//! Files are named `<character>/<pose>-<frame>.svg` and
//! `<character>/notes-<count>-<frame>.svg`; rasterize them with
//! `resvg --zoom 2 in.svg out.png`.

use std::path::Path;

use mostalgia_app::companion::cast::{PointKind, hoshi, tsumugi};

/// A mixed run of notes, as a meeting would produce them.
fn notes(count: usize) -> Vec<PointKind> {
    (0..count).map(|i| PointKind::ALL[(i * 7 + i / 3) % 4]).collect()
}

macro_rules! write_character {
    ($m:ident, $out:expr) => {{
        let dir = $out.join(stringify!($m));
        std::fs::create_dir_all(&dir).expect("create output dir");
        for pose in $m::Pose::ALL {
            let name = format!("{pose:?}").to_lowercase();
            for f in 0..$m::frame_count(pose) {
                std::fs::write(dir.join(format!("{name}-{f:02}.svg")), $m::frame_svg(pose, f)).expect("write frame");
            }
            println!("{}/{name}: {} frames", stringify!($m), $m::frame_count(pose));
        }
        for count in [0, 1, 5, 20] {
            for f in 0..$m::NOTES_FRAMES {
                std::fs::write(dir.join(format!("notes-{count:02}-{f:02}.svg")), $m::notes_svg(&notes(count), f)).expect("write notes");
            }
        }
    }};
}

fn main() {
    let out = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "cast-frames".into()));
    let out: &Path = &out;
    write_character!(tsumugi, out);
    write_character!(hoshi, out);
    println!("written to {}", out.display());
}
