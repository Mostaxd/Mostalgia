//! Shows how the speaker separation splits a recording's system track and
//! which known voices it matches (read-only):
//! `cargo run --release -p mostalgia-core --example diarize_probe -- <session dir> [auto|N|N+]…`
//! where N is the number of people on the system track. Set
//! `MOSTALGIA_DIARIZE_THRESHOLD` to try another clustering threshold, and
//! `PROBE_DB=<copy of mostalgia.db> PROBE_MEETING=<id>` to print that
//! meeting's system lines as the separation would label them.

use mostalgia_core::diarize::{Speakers, separate};
use mostalgia_core::voices::Voices;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("session dir"));
    let modes: Vec<String> = args.collect();
    let modes = if modes.is_empty() { vec!["auto".to_string()] } else { modes };
    let samples = mostalgia_core::finalize::read_wav(&dir.join("system.wav"))?;
    let known = Voices::load();
    println!("track {:.0}s, known voices: {:?}", samples.len() as f64 / 16000.0, known.voices.iter().map(|v| &v.name).collect::<Vec<_>>());
    for mode in modes {
        let speakers = match mode.as_str() {
            "auto" => Speakers::Auto,
            m if m.ends_with('+') => Speakers::AtLeast(m.trim_end_matches('+').parse()?),
            m => Speakers::Exactly(m.parse()?),
        };
        let t = std::time::Instant::now();
        let sep = separate(&samples, speakers, &known)?;
        println!("\n== {mode} ({speakers:?}): clustering found {}, after merging {} ({:.1}s)", sep.found, sep.secs.len(), t.elapsed().as_secs_f64());
        for i in 0..sep.secs.len() {
            let sims = sep.prints[i].as_ref().map(|p| known.similarities(p)).unwrap_or_default();
            let sims: Vec<String> = sims.iter().map(|(n, s)| format!("{n} {s:.3}")).collect();
            let turns = sep.turns.iter().filter(|t| t.speaker == i as i32).count();
            println!("  speaker {i}: {:>6.1}s in {turns:>3} turns -> {:<12} | {}", sep.secs[i], sep.label(i), sims.join(", "));
        }
        if let (Ok(db), Ok(id)) = (std::env::var("PROBE_DB"), std::env::var("PROBE_MEETING")) {
            let store = mostalgia_core::store::Store::open(std::path::Path::new(&db))?;
            let meeting = store.get(id.parse()?)?.expect("meeting");
            let mut changes = std::collections::BTreeMap::<(String, String), usize>::new();
            for seg in meeting.segments.iter().filter(|s| s.source == mostalgia_core::Source::System) {
                let mut best = (0.0f64, None);
                for t in &sep.turns {
                    let overlap = (seg.end_secs.min(t.end as f64) - seg.start_secs.max(t.start as f64)).max(0.0);
                    if overlap > best.0 {
                        best = (overlap, Some(t.speaker));
                    }
                }
                let new = best.1.map(|s| sep.label(s as usize)).unwrap_or_else(|| "-".into());
                *changes.entry((seg.speaker_label().to_string(), new.clone())).or_default() += 1;
                println!("    {:>7.1} {:<12} -> {:<12} {}", seg.start_secs, seg.speaker_label(), new, seg.text.chars().take(90).collect::<String>());
            }
            for ((old, new), n) in changes {
                println!("  {old} -> {new}: {n} lines");
            }
        }
        for i in 0..sep.prints.len() {
            for j in i + 1..sep.prints.len() {
                if let (Some(a), Some(b)) = (&sep.prints[i], &sep.prints[j]) {
                    let s: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
                    println!("  speaker {i} ~ speaker {j}: {s:.3}");
                }
            }
        }
    }
    Ok(())
}
