//! Teams transcripts (WebVTT with `<v Name>` voice tags), lined up with a
//! Mostalgia transcript to suggest who our unnamed speakers were.
//!
//! The two transcripts don't start at the same moment (the recording
//! starts before or after Teams' transcription), so the offset between
//! them is found from the text: runs of three words both transcripts heard
//! give candidate offsets, and the most common one wins.

use std::collections::HashMap;

/// One cue of a Teams transcript.
#[derive(Clone, Debug, PartialEq)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub speaker: Option<String>,
    pub text: String,
}

/// The cues of a WebVTT file. Cues without a voice tag keep the previous
/// cue's speaker only when they have none (Teams tags every cue).
pub fn parse(vtt: &str) -> Vec<Cue> {
    let mut out = Vec::new();
    let mut lines = vtt.lines().peekable();
    while let Some(line) = lines.next() {
        let Some((start, end)) = line.split_once("-->").and_then(|(a, b)| Some((timestamp(a)?, timestamp(b.split_whitespace().next()?)?))) else {
            continue;
        };
        let mut text_lines = Vec::new();
        while let Some(l) = lines.peek() {
            if l.trim().is_empty() {
                break;
            }
            text_lines.push(lines.next().unwrap_or_default());
        }
        let raw = text_lines.join(" ");
        let (speaker, text) = voice(&raw);
        if !text.is_empty() {
            out.push(Cue { start, end, speaker, text });
        }
    }
    out
}

/// `00:01:02.500`, `1:02.500` or `0:01:02.5` in seconds.
fn timestamp(s: &str) -> Option<f64> {
    let s = s.trim().replace(',', ".");
    let parts: Vec<&str> = s.split(':').collect();
    let secs: f64 = parts.last()?.parse().ok()?;
    let mut total = secs;
    let mut mult = 60.0;
    for p in parts.iter().rev().skip(1) {
        total += p.parse::<f64>().ok()? * mult;
        mult *= 60.0;
    }
    Some(total)
}

/// `<v Anna Lehmann>Hello</v>` → (Some("Anna Lehmann"), "Hello"); other tags removed.
fn voice(raw: &str) -> (Option<String>, String) {
    let mut speaker = None;
    if let Some(rest) = raw.trim_start().strip_prefix("<v")
        && let Some(end) = rest.find('>')
    {
        let inner = rest[..end].trim();
        // `<v.loud Anna>`: classes before the name.
        let name = if inner.starts_with('.') { inner.split_once(' ').map_or("", |(_, n)| n) } else { inner };
        let name = name.trim();
        if !name.is_empty() {
            speaker = Some(name.to_string());
        }
    }
    let mut text = String::new();
    let mut in_tag = false;
    for c in raw.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    (speaker, text.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// A line of our own transcript, as the matcher needs it.
#[derive(Clone, Debug)]
pub struct Line<'a> {
    pub start: f64,
    pub end: f64,
    pub speaker: &'a str,
    pub text: &'a str,
}

/// A name suggested for one of our speaker labels.
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    /// Our label ("Andere", "Sprecher 2").
    pub label: String,
    /// The Teams speaker it lines up with.
    pub name: String,
    /// Seconds the two overlap.
    pub seconds: f64,
    /// Share of the label's overlapped time that is this name (0–1).
    pub share: f64,
}

/// Lowercase words, letters and digits only.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// Seconds to add to a Teams time to get ours, from shared word runs;
/// `fallback` when the transcripts share too little.
pub fn find_offset(ours: &[Line], cues: &[Cue], fallback: Option<f64>) -> Option<f64> {
    for n in [3usize, 2] {
        let mut shingles: HashMap<Vec<String>, Vec<f64>> = HashMap::new();
        for c in cues {
            let w = words(&c.text);
            for s in w.windows(n) {
                shingles.entry(s.to_vec()).or_default().push(c.start);
            }
        }
        // Common phrases ("ja das ist") say nothing about the offset.
        shingles.retain(|_, starts| starts.len() <= 3);
        let mut bins: HashMap<i64, usize> = HashMap::new();
        for l in ours {
            let w = words(l.text);
            for s in w.windows(n) {
                for start in shingles.get(s).into_iter().flatten() {
                    *bins.entry(((l.start - start) / 2.0).round() as i64).or_default() += 1;
                }
            }
        }
        // A bin and its neighbours (offsets straddle bin edges).
        let best = bins.keys().map(|b| (*b, (b - 1..=b + 1).map(|k| bins.get(&k).copied().unwrap_or(0)).sum::<usize>())).max_by_key(|(b, n)| (*n, -b.abs()));
        if let Some((bin, votes)) = best
            && votes >= 3
        {
            return Some(bin as f64 * 2.0);
        }
    }
    fallback
}

/// Names for our generic speaker labels: the Teams speaker each one
/// overlaps most with, when that's clear (most of its time, at least a few
/// seconds). `me` (the signed-in user) is never suggested for the
/// others' track; "Ich" isn't renamed.
pub fn suggest(ours: &[Line], cues: &[Cue], offset: f64, me: &str) -> Vec<Suggestion> {
    let mut overlap: HashMap<&str, HashMap<&str, f64>> = HashMap::new();
    for l in ours {
        if l.speaker == "Ich" || !crate::voices::is_generic_label(l.speaker) { // i18n: not UI
            continue;
        }
        for c in cues {
            let Some(name) = c.speaker.as_deref() else { continue };
            if !me.is_empty() && name.eq_ignore_ascii_case(me) {
                continue;
            }
            let o = (l.end.min(c.end + offset) - l.start.max(c.start + offset)).max(0.0);
            if o > 0.0 {
                *overlap.entry(l.speaker).or_default().entry(name).or_default() += o;
            }
        }
    }
    let mut out: Vec<Suggestion> = overlap
        .into_iter()
        .filter_map(|(label, names)| {
            let total: f64 = names.values().sum();
            let (name, secs) = names.into_iter().max_by(|a, b| a.1.total_cmp(&b.1))?;
            let share = secs / total;
            (secs >= MIN_SECONDS && share >= MIN_SHARE).then(|| Suggestion { label: label.into(), name: name.into(), seconds: secs, share })
        })
        .collect();
    // One name per label, and one label per name (the larger overlap keeps it).
    out.sort_by(|a, b| b.seconds.total_cmp(&a.seconds));
    let mut taken: Vec<String> = Vec::new();
    out.retain(|s| {
        if taken.contains(&s.name) {
            return false;
        }
        taken.push(s.name.clone());
        true
    });
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

const MIN_SECONDS: f64 = 5.0;
const MIN_SHARE: f64 = 0.6;

/// All speaker names in a transcript, in order of first appearance.
pub fn speakers(cues: &[Cue]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in cues {
        if let Some(s) = &c.speaker
            && !out.contains(s)
        {
            out.push(s.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const VTT: &str = "WEBVTT\n\n\
1b2c3d4e-0001/12-0\n00:00:01.000 --> 00:00:06.000\n<v Anna Lehmann>Guten Morgen, wir starten mit dem PayFlow Rollout heute.</v>\n\n\
1b2c3d4e-0002/7-0\n00:00:07.000 --> 00:00:14.000\n<v Jerome Markus>Die Migration der Kundendaten ist fast fertig, es fehlen nur noch zwei Tabellen.</v>\n\n\
00:00:15.000 --> 00:00:22.500\n<v Anna Lehmann>Gut, dann machen wir am Freitag den Go-live mit dem ganzen Team.</v>\n\n\
0:00:23.000 --> 0:00:30.000\n<v Mosta>Ich kümmere mich um das Angebot und schicke es morgen raus.</v>\n";

    #[test]
    fn parses_teams_vtt() {
        let cues = parse(VTT);
        assert_eq!(cues.len(), 4);
        assert_eq!(cues[0].speaker.as_deref(), Some("Anna Lehmann"));
        assert_eq!(cues[0].start, 1.0);
        assert_eq!(cues[2].end, 22.5);
        assert_eq!(cues[3].start, 23.0);
        assert!(cues[1].text.starts_with("Die Migration"));
        assert_eq!(speakers(&cues), ["Anna Lehmann", "Jerome Markus", "Mosta"]);
    }

    #[test]
    fn voice_tags_with_classes() {
        assert_eq!(voice("<v.loud Anna>Hi <b>there</b></v>"), (Some("Anna".into()), "Hi there".into()));
        assert_eq!(voice("no tag"), (None, "no tag".into()));
    }

    /// Our transcript started 40 s before Teams' and heard slightly
    /// different words.
    fn ours() -> Vec<Line<'static>> {
        vec![
            Line { start: 41.2, end: 46.0, speaker: "Sprecher 1", text: "Guten Morgen wir starten mit dem Pay Flow Rollout heute" },
            Line { start: 47.1, end: 54.0, speaker: "Sprecher 2", text: "die Migration der Kundendaten ist fast fertig es fehlen nur noch zwei Tabellen" },
            Line { start: 55.0, end: 62.4, speaker: "Sprecher 1", text: "gut dann machen wir am Freitag den Go live mit dem ganzen Team" },
            Line { start: 63.0, end: 70.0, speaker: "Ich", text: "ich kümmere mich um das Angebot und schicke es morgen raus" },
        ]
    }

    #[test]
    fn finds_the_offset_from_shared_words() {
        let cues = parse(VTT);
        let off = find_offset(&ours(), &cues, None).unwrap();
        assert!((off - 40.0).abs() <= 2.0, "{off}");
        assert_eq!(find_offset(&ours()[..0], &cues, Some(12.0)), Some(12.0));
    }

    #[test]
    fn suggests_names_for_generic_labels() {
        let cues = parse(VTT);
        let s = suggest(&ours(), &cues, 40.0, "Mosta");
        assert_eq!(s.len(), 2, "{s:?}");
        assert_eq!((s[0].label.as_str(), s[0].name.as_str()), ("Sprecher 1", "Anna Lehmann"));
        assert_eq!((s[1].label.as_str(), s[1].name.as_str()), ("Sprecher 2", "Jerome Markus"));
        assert!(s[0].share > 0.9);
    }

    #[test]
    fn no_suggestion_when_mixed() {
        let cues = parse(VTT);
        let lines = vec![Line { start: 41.0, end: 54.0, speaker: "Andere", text: "" }];
        // Anna and Jerome both overlap a lot: no clear winner.
        assert!(suggest(&lines, &cues, 40.0, "").is_empty());
    }
}
