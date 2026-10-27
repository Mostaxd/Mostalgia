//! Writes every animation frame of Mori (the companion) as SVG, for
//! previewing the artwork without the app. Run with:
//!
//!     cargo run -p mostalgia-app --example companion_preview -- <out-dir>
//!
//! Files are named `<pose>-<frame>.svg` (e.g. `writing-03.svg`); rasterize
//! them with `resvg --zoom 2 in.svg out.png`.

use mostalgia_app::companion::art::{frame_count, frame_svg, Pose};

fn main() {
    let out = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "companion-frames".into()));
    std::fs::create_dir_all(&out).expect("create output dir");
    let mut total = 0;
    for pose in Pose::ALL {
        let name = format!("{pose:?}").to_lowercase();
        for f in 0..frame_count(pose) {
            let svg = frame_svg(pose, f);
            total += svg.len();
            std::fs::write(out.join(format!("{name}-{f:02}.svg")), svg).expect("write frame");
        }
        println!("{name}: {} frames", frame_count(pose));
    }
    println!("{} bytes total in {}", total, out.display());
}
