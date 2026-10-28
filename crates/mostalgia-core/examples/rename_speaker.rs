//! Renames a speaker in one meeting the way the app does, and remembers
//! the voice under the new name:
//! `cargo run --release -p mostalgia-core --example rename_speaker -- <meeting id> <current> <new name>`
//! Back up mostalgia.db and voices.json first.

use mostalgia_core::{paths, store::Store, voices};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(id), Some(current), Some(name)) = (args.next(), args.next(), args.next()) else {
        anyhow::bail!("usage: rename_speaker <meeting id> <current> <new name>");
    };
    let id: i64 = id.parse()?;
    let store = Store::open(&paths::database())?;
    let lines = store.rename_speaker(id, &current, &name)?;
    println!("renamed {lines} lines: {current} -> {name}");
    match voices::learn_from_meeting(id, &current, &name)? {
        voices::Learned::Voice { secs } => println!("voice learned from {secs:.0} s"),
        other => println!("voice not learned: {other:?}"),
    }
    Ok(())
}
