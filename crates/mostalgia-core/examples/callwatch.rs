//! Prints call start/end events as mostalgia sees them (for checking
//! detection): `cargo run -p mostalgia-core --example callwatch`.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

fn main() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let _watch = mostalgia_core::callwatch::spawn(tx, Arc::new(AtomicBool::new(false)));
    let t0 = std::time::Instant::now();
    for ev in rx {
        println!("{:7.2}s {ev:?}", t0.elapsed().as_secs_f64());
    }
}
