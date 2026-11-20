//! Shows the voice print a rename would learn for a speaker of a saved
//! meeting (read-only): which pieces are left out as someone else's, how
//! many seconds it uses, and how alike it is to each saved voice:
//! `cargo run --release -p mostalgia-core --example voice_probe -- <meeting id> <speaker>`

use mostalgia_core::voices::{self, Voices};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(id), Some(name)) = (args.next(), args.next()) else {
        anyhow::bail!("usage: voice_probe <meeting id> <speaker>");
    };
    match voices::probe_print(id.parse()?, &name)? {
        None => println!("{name} has no lines on the system track"),
        Some((secs, None)) => println!("{name}: only {secs:.1} s of clean speech, no print"),
        Some((secs, Some(print))) => {
            println!("{name}: print from {secs:.1} s of clean speech");
            for (known, sim) in Voices::load().similarities(&print) {
                println!("  {known:<16} {sim:.3}");
            }
        }
    }
    Ok(())
}
