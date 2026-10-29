//! Replays a timestamped transcript in real time through Mori's live-notes
//! logic (trigger, context, request, card staggering) against the
//! configured notes connection, and reports how late each note shows up.
//! Makes real requests (costs tokens).
//!
//!     cargo run --release -p mostalgia-core --example notes_replay -- <transcript> [live|60|90|120]
//!
//! Transcript lines: "[mm:ss.s-mm:ss.s] Speaker: text". A line counts as
//! finalized `FINALIZE_DELAY` after it ends (pause detection plus
//! transcription). SPEED=<x> runs faster than real time (the model's
//! latency is not scaled, so keep it at 1 for latency numbers). START=<s>
//! begins there, as if everything before had been looked at already.
//! MEMORY_PROBE=<s> makes one meeting-memory update for the lines up to then.
//! GAP=<s> sets the least time between checks (as it happens), FOCUS=<text>
//! the focus hint, STYLE=default|modern the note style (default: the
//! configured ones).

use std::sync::mpsc;
use std::time::Instant;

use mostalgia_core::companion::{self, ContextLine, NotesClock, NotesInput, Point, PointsReply};
use mostalgia_core::config::NotesMode;

/// Pause detection (~0.5 s) plus local transcription of the chunk.
const FINALIZE_DELAY: f64 = 1.2;
/// Cards appear at least this far apart (as in the app).
const STAGGER: f64 = 3.5;
const CONTEXT_CHARS: usize = 24_000;

struct Line {
    start: f64,
    end: f64,
    speaker: String,
    text: String,
}

fn ts(s: &str) -> f64 {
    let (m, s) = s.split_once(':').unwrap();
    m.parse::<f64>().unwrap() * 60. + s.parse::<f64>().unwrap()
}

struct Run {
    started: f64,
    /// Index of the newest finalized line when it started.
    trigger: usize,
    chars: usize,
    stable: usize,
    lines: usize,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("transcript path");
    let mode = match args.next().as_deref().unwrap_or("live") {
        "live" => NotesMode::Live,
        s => NotesMode::Every(s.parse().expect("live or seconds")),
    };
    let speed: f64 = std::env::var("SPEED").ok().and_then(|s| s.parse().ok()).unwrap_or(1.);
    let lines: Vec<Line> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter_map(|l| {
            let (stamp, rest) = l.strip_prefix('[')?.split_once("] ")?;
            let (a, b) = stamp.split_once('-')?;
            let (speaker, text) = rest.split_once(": ")?;
            Some(Line { start: ts(a), end: ts(b), speaker: speaker.into(), text: text.into() })
        })
        .collect();
    let config = mostalgia_core::config::Config::load();
    let req = config.notes_request().unwrap();
    // GAP=<s> overrides the configured gap, FOCUS=<text> the focus hint.
    let gap: f64 = std::env::var("GAP").ok().and_then(|s| s.parse().ok()).unwrap_or(config.notes_gap() as f64);
    let focus = std::env::var("FOCUS").unwrap_or_else(|_| config.companion_notes_focus.clone());
    let style = std::env::var("STYLE").ok().and_then(|s| companion::NoteStyle::parse(&s)).unwrap_or_else(|| config.notes_style());
    println!("# {} lines, {:.0} s, mode {mode:?}, style {}, focus {focus:?}, {} · {}", lines.len(), lines.last().unwrap().end, style.id(), req.connection.name, req.model);
    let finalized_at = |i: usize| lines[i].end + FINALIZE_DELAY;
    let context = |i: usize| ContextLine { at_secs: lines[i].start, speaker: lines[i].speaker.clone(), text: lines[i].text.clone(), interim: false };

    // MEMORY_PROBE=<s>: one meeting-memory update for the lines up to then, and stop.
    if let Some(until) = std::env::var("MEMORY_PROBE").ok().and_then(|s| s.parse::<f64>().ok()) {
        let fold: Vec<ContextLine> = (0..lines.len()).filter(|&i| lines[i].start < until).map(context).collect();
        let t = Instant::now();
        let r = companion::meeting_memory(&companion::memory_message(None, &fold, &[]), &focus);
        println!("memory of {} lines in {:.2}s: {r:?}", fold.len(), t.elapsed().as_secs_f32());
        return;
    }
    let start: f64 = std::env::var("START").ok().and_then(|s| s.parse().ok()).unwrap_or(0.);
    let t0 = Instant::now();
    let now = || start + t0.elapsed().as_secs_f64() * speed;
    let (tx, rx) = mpsc::channel::<(usize, Result<PointsReply, String>, f64)>();
    let (mtx, mrx) = mpsc::channel::<(f64, Result<String, String>, f64)>();
    let mut notes: Vec<Point> = Vec::new();
    let mut run: Option<Run> = None;
    let mut runs = 0;
    let mut last_start: Option<f64> = None;
    let mut seen = (0..lines.len()).take_while(|&i| finalized_at(i) <= start).count(); // finalized lines already looked at
    let mut new_after = if seen > 0 { lines[seen - 1].start } else { -1. };
    let mut memory: Option<String> = None;
    let mut memory_until = 0.;
    let mut memory_busy = false;
    let mut last_release: Option<f64> = None;
    let mut latencies_line = Vec::new();
    let mut latencies_trigger = Vec::new();
    let mut request_secs = Vec::new();
    let mut tokens = (0u64, 0u64, 0u64);
    let mut empty_runs = 0;
    let end = lines.last().unwrap().end + 20.;
    loop {
        let t = now();
        let done = (0..lines.len()).take_while(|&i| finalized_at(i) <= t).count();
        // A finished request: its cards, staggered.
        if let Ok((n, result, finished)) = rx.try_recv() {
            let r = run.take().unwrap();
            let took = finished - r.started;
            request_secs.push(took);
            match result {
                Ok(reply) => {
                    if let Some(u) = reply.usage {
                        tokens.0 += u.input;
                        tokens.1 += u.output;
                        tokens.2 += u.cached;
                    }
                    let usage = reply.usage.map_or("-".into(), |u| format!("{} in ({} cached) / {} out", u.input, u.cached, u.output));
                    println!(
                        "run #{n} @{:6.1}s: {} lines, {} chars ({} stable), took {took:.2}s (first token {:.2}s), {usage} → {} notes ({} repeated, {} over limit)",
                        r.started,
                        r.lines,
                        r.chars,
                        r.stable,
                        reply.first_token_secs.unwrap_or(f64::NAN),
                        reply.points.len(),
                        reply.duplicates,
                        reply.over_limit
                    );
                    if reply.points.is_empty() {
                        empty_runs += 1;
                    }
                    let gap = if matches!(mode, NotesMode::Live) { STAGGER } else { 0. };
                    let shown = companion::stagger(finished, last_release, reply.points.len(), gap);
                    for (p, at) in reply.points.iter().zip(&shown) {
                        // The line the note comes from: the last one starting by its time.
                        let line = lines.iter().rposition(|l| l.start <= p.at_secs + 1.).unwrap_or(0);
                        let from_line = at - finalized_at(line);
                        let from_trigger = at - finalized_at(r.trigger);
                        latencies_line.push(from_line);
                        latencies_trigger.push(from_trigger);
                        println!(
                            "   [{:?}] {} (line {} @{:.0}s) shown @{at:.1}s: {from_line:.1}s after its line was finalized, {from_trigger:.1}s after the trigger line",
                            p.kind, p.text, line, lines[line].start
                        );
                    }
                    last_release = shown.last().copied().or(last_release);
                    notes.extend(reply.points);
                }
                Err(e) => println!("run #{n}: FAILED after {took:.2}s: {e}"),
            }
        }
        if let Ok((until, result, took)) = mrx.try_recv() {
            memory_busy = false;
            match result {
                Ok(m) => {
                    println!("memory up to {until:.0}s ({took:.2}s): {m}");
                    memory = Some(m);
                    memory_until = until;
                }
                Err(e) => println!("memory FAILED: {e}"),
            }
        }
        let verbatim: Vec<ContextLine> = (0..done).filter(|&i| lines[i].start >= memory_until).map(context).collect();
        if !memory_busy
            && let Some(cut) = companion::memory_due(&verbatim)
        {
            memory_busy = true;
            let fold: Vec<ContextLine> = verbatim.iter().filter(|l| l.at_secs < cut).cloned().collect();
            let msg = companion::memory_message(memory.as_deref(), &fold, &notes);
            let (mtx, started, focus) = (mtx.clone(), t, focus.clone());
            std::thread::spawn(move || {
                let r = companion::meeting_memory(&msg, &focus).map(|(m, _)| m).map_err(|e| format!("{e:#}"));
                let _ = mtx.send((cut, r, start + t0.elapsed().as_secs_f64() * speed - started));
            });
        }
        let new: Vec<usize> = (seen..done).collect();
        let clock = NotesClock {
            in_flight: run.is_some(),
            since_last: last_start.map(|s| t - s),
            new_lines: new.len(),
            new_words: new.iter().map(|&i| companion::word_count(&lines[i].text)).sum(),
            new_speech: new.iter().map(|&i| lines[i].end - lines[i].start).sum(),
            waiting: new.first().map_or(0., |&i| t - finalized_at(i)),
        };
        if companion::notes_due(mode, gap, &clock) {
            runs += 1;
            let input = NotesInput { memory: memory.as_deref(), lines: &verbatim, new_after, notes: &notes };
            let (msg, stable) = companion::notes_message(&input, CONTEXT_CHARS);
            run = Some(Run { started: t, trigger: done - 1, chars: msg.len(), stable, lines: verbatim.len() });
            last_start = Some(t);
            seen = done;
            new_after = lines[done - 1].start;
            let (tx, previous, n, focus) = (tx.clone(), notes.clone(), runs, focus.clone());
            std::thread::spawn(move || {
                let r = companion::live_points(&msg, stable, &previous, mode, style, &focus).map_err(|e| format!("{e:#}"));
                let _ = tx.send((n, r, start + t0.elapsed().as_secs_f64() * speed));
            });
        }
        if t > end && run.is_none() && !memory_busy {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let pct = |v: &mut Vec<f64>, p: f64| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        if v.is_empty() { f64::NAN } else { v[((v.len() - 1) as f64 * p).round() as usize] }
    };
    let minutes = (lines.last().unwrap().end - start) / 60.;
    println!("\n# summary ({mode:?}, {minutes:.1} min of talk)");
    println!("requests: {runs} ({:.1} per 10 min), empty: {empty_runs}", runs as f64 / minutes * 10.);
    println!("notes: {} ({:.1} per 10 min)", notes.len(), notes.len() as f64 / minutes * 10.);
    println!(
        "request time: median {:.2}s, p90 {:.2}s",
        pct(&mut request_secs.clone(), 0.5),
        pct(&mut request_secs, 0.9)
    );
    println!("note latency from its line finalized: median {:.1}s, p90 {:.1}s", pct(&mut latencies_line.clone(), 0.5), pct(&mut latencies_line, 0.9));
    println!("note latency from the trigger line: median {:.1}s, p90 {:.1}s", pct(&mut latencies_trigger.clone(), 0.5), pct(&mut latencies_trigger, 0.9));
    println!("tokens: {} in ({} cached), {} out; per request {:.0} in / {:.0} out", tokens.0, tokens.2, tokens.1, tokens.0 as f64 / runs.max(1) as f64, tokens.1 as f64 / runs.max(1) as f64);
}
