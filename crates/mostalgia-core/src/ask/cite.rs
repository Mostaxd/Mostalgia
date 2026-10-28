//! Citations in answers, turned into numbered markers plus a source list.
//!
//! Models cite as `[[m:ID@SECS]]` (library answers), `[[d:ID@LOC]]`
//! (a document's page or section) or `[[t:SECS]]` /
//! `[[t:hh:mm:ss]]` (live answers), but also slip in raw times like
//! `[00:01:00]`, `[1:02]`, ranges `[00:01:00–00:01:02]` or lists
//! `[00:01:00, 00:01:17]`. [`parse`] removes all of them from the text and
//! returns the answer as text pieces and numbered citations: the same
//! source cited twice keeps its number, and a repeated marker right after
//! itself is dropped.

/// What a citation points at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    /// A moment (or span) of the meeting being recorded / the current meeting.
    Time { secs: f64, end: Option<f64> },
    /// A moment in a saved meeting.
    Meeting { meeting_id: i64, secs: f64 },
    /// A page (PDF) or section of a document in the documents drive.
    Document { doc_id: i64, loc: u32 },
}

impl Target {
    /// Seconds where playback / scrolling should start.
    pub fn secs(&self) -> f64 {
        match *self {
            Target::Time { secs, .. } | Target::Meeting { secs, .. } => secs,
            Target::Document { .. } => 0.,
        }
    }

    /// Short time label: "0:42", "1:00–1:02", "1:02:03" (a document's
    /// location as "#4"; the app labels it "p. 4" or "§4").
    pub fn time_label(&self) -> String {
        match *self {
            Target::Time { secs, end: Some(end) } => format!("{}–{}", short_time(secs), short_time(end)),
            Target::Time { secs, .. } | Target::Meeting { secs, .. } => short_time(secs),
            Target::Document { loc, .. } => format!("#{loc}"),
        }
    }

    fn key(&self) -> (i64, i64, i64) {
        match *self {
            Target::Time { secs, end } => (-1, secs.round() as i64, end.map_or(-1, |e| e.round() as i64)),
            Target::Meeting { meeting_id, secs } => (meeting_id, secs.round() as i64, -1),
            Target::Document { doc_id, loc } => (-2 - doc_id, loc as i64, -2),
        }
    }
}

/// "0:42", "13:05", "1:02:03" (hours only from an hour on).
pub fn short_time(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// A piece of an answer.
#[derive(Clone, Debug, PartialEq)]
pub enum Mark {
    Text(String),
    /// Citation number `index` (1-based) into [`Cited::sources`].
    Cite { index: usize, target: Target },
}

/// An answer with its citations replaced by numbered markers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cited {
    pub pieces: Vec<Mark>,
    /// Distinct sources in order of first mention; source `n` is `sources[n - 1]`.
    pub sources: Vec<Target>,
}

impl Cited {
    /// The pieces split into lines (text pieces never contain '\n').
    pub fn lines(&self) -> Vec<Vec<Mark>> {
        let mut lines = vec![Vec::new()];
        for p in &self.pieces {
            match p {
                Mark::Text(t) => {
                    for (i, part) in t.split('\n').enumerate() {
                        if i > 0 {
                            lines.push(Vec::new());
                        }
                        if !part.is_empty() {
                            lines.last_mut().unwrap().push(Mark::Text(part.to_string()));
                        }
                    }
                }
                cite => lines.last_mut().unwrap().push(cite.clone()),
            }
        }
        lines
    }

    /// Plain text with times in parentheses ("… frozen (0:27).") for exports.
    pub fn plain(&self) -> String {
        let mut out = String::new();
        let mut last: Option<usize> = None;
        for p in &self.pieces {
            match p {
                Mark::Text(t) => {
                    out.push_str(t);
                    last = None;
                }
                Mark::Cite { index, target } if last != Some(*index) => {
                    out.push_str(&format!(" ({})", target.time_label()));
                    last = Some(*index);
                }
                Mark::Cite { .. } => {}
            }
        }
        out
    }
}

/// "hh:mm:ss", "h:mm:ss", "mm:ss" or "m:ss" (optionally with fractions or a
/// leading "123s "), as seconds.
fn parse_clock(s: &str) -> Option<f64> {
    let s = s.trim();
    // "[27s 00:00:27]" as the transcript tool prints it.
    let s = match s.split_once(' ') {
        Some((a, b)) if a.ends_with('s') && a[..a.len() - 1].parse::<f64>().is_ok() => b.trim(),
        _ => s,
    };
    let parts: Vec<&str> = s.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut total = 0.0;
    for (i, p) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        let ok = !p.is_empty()
            && p.len() <= if last { 6 } else { 2 }
            && p.chars().all(|c| c.is_ascii_digit() || (last && c == '.'))
            && p.chars().next().is_some_and(|c| c.is_ascii_digit());
        if !ok || (i > 0 && p.split('.').next().is_none_or(|d| d.len() != 2)) {
            return None;
        }
        let v = p.parse::<f64>().ok()?;
        if i > 0 && v >= 60.0 {
            return None;
        }
        total = total * 60.0 + v;
    }
    Some(total)
}

/// Seconds as a clock or a plain number ("42", "42.5", "42s").
fn parse_secs(s: &str) -> Option<f64> {
    let s = s.trim();
    parse_clock(s).or_else(|| {
        let n = s.strip_suffix('s').unwrap_or(s).trim();
        (!n.is_empty() && n.chars().all(|c| c.is_ascii_digit() || c == '.')).then(|| n.parse().ok()).flatten()
    })
}

const RANGE_SEPS: [&str; 4] = ["–", "—", "-", " to "];

/// One item of a time list: a time or a span.
fn parse_span(item: &str, secs: fn(&str) -> Option<f64>) -> Option<Target> {
    let item = item.trim();
    if let Some(t) = secs(item) {
        return Some(Target::Time { secs: t, end: None });
    }
    for sep in RANGE_SEPS {
        if let Some((a, b)) = item.split_once(sep) {
            let (a, b) = (secs(a)?, secs(b)?);
            return Some(Target::Time { secs: a, end: (b > a).then_some(b) });
        }
    }
    None
}

fn list(inner: &str) -> impl Iterator<Item = &str> {
    inner.split([',', ';']).map(str::trim).filter(|s| !s.is_empty())
}

/// Citations inside `[[…]]`: "m:12@27", "m:12@27, m:12@40", "t:42",
/// "t:00:01:00-00:01:02". `None` if it isn't a citation.
fn parse_double(inner: &str) -> Option<Vec<Target>> {
    let inner = inner.trim();
    let mut out = Vec::new();
    if let Some(rest) = inner.strip_prefix("m:") {
        for item in list(rest) {
            let item = item.strip_prefix("m:").unwrap_or(item);
            let (id, secs) = item.split_once('@')?;
            let secs = parse_span(secs, parse_secs)?.secs();
            out.push(Target::Meeting { meeting_id: id.trim().parse().ok()?, secs });
        }
    } else if let Some(rest) = inner.strip_prefix("d:") {
        for item in list(rest) {
            let item = item.strip_prefix("d:").unwrap_or(item);
            let (id, loc) = item.split_once('@').unwrap_or((item, "1"));
            let loc = loc.trim().trim_start_matches(['p', 'S', '§', '#', '.', ' ']).parse::<u32>().ok().or_else(|| loc.trim().parse::<f64>().ok().map(|f| f as u32))?;
            out.push(Target::Document { doc_id: id.trim().parse().ok()?, loc: loc.max(1) });
        }
    } else if let Some(rest) = inner.strip_prefix("t:") {
        for item in list(rest) {
            out.push(parse_span(item.strip_prefix("t:").unwrap_or(item), parse_secs)?);
        }
    } else {
        return None;
    }
    (!out.is_empty()).then_some(out)
}

/// Raw times inside `[…]` or `(…)`: "00:01:00", "1:00–1:02", "00:01:00, 00:01:17".
/// Only clock times count, so "[1]" or "(see 2)" stay text.
fn parse_single(inner: &str) -> Option<Vec<Target>> {
    let out: Option<Vec<Target>> = list(inner).map(|item| parse_span(item, parse_clock)).collect();
    out.filter(|v| !v.is_empty())
}

/// Splits an answer into text and numbered citations (see the module docs).
/// An unfinished marker at the end (while streaming) is left out.
pub fn parse(answer: &str) -> Cited {
    let mut cited = Cited::default();
    let mut text = String::new();
    let mut rest = answer;
    let push_cites = |cited: &mut Cited, text: &mut String, targets: Vec<Target>| {
        // The marker sits right after the word it belongs to.
        let trimmed = text.trim_end_matches([' ', '\t']).len();
        text.truncate(trimmed);
        if !text.is_empty() {
            cited.pieces.push(Mark::Text(std::mem::take(text)));
        }
        for target in targets {
            let index = match cited.sources.iter().position(|s| s.key() == target.key()) {
                Some(i) => i + 1,
                None => {
                    cited.sources.push(target);
                    cited.sources.len()
                }
            };
            if !matches!(cited.pieces.last(), Some(Mark::Cite { index: last, .. }) if *last == index) {
                cited.pieces.push(Mark::Cite { index, target: cited.sources[index - 1] });
            }
        }
    };
    while let Some(start) = rest.find(['[', '(']) {
        let (before, from) = rest.split_at(start);
        text.push_str(before);
        let double = from.starts_with("[[");
        let close = if double { "]]" } else if from.starts_with('[') { "]" } else { ")" };
        let open_len = if double { 2 } else { 1 };
        let Some(end) = from[open_len..].find(close).map(|e| e + open_len) else {
            // Unfinished at the end: hide a marker that is still streaming in.
            let tail = &from[open_len..];
            let partial = double
                || (!tail.is_empty() && tail.len() <= 24 && tail.chars().all(|c| c.is_ascii_digit() || ":.,;–—- ".contains(c)));
            if !partial || from.starts_with('(') && !tail.contains(':') {
                text.push_str(from);
            }
            rest = "";
            break;
        };
        let inner = &from[open_len..end];
        let targets = if double {
            parse_double(inner)
        } else if from.starts_with('(') {
            // In parentheses only full "hh:mm:ss" times: "(9:30–10:00)" may be a schedule.
            parse_single(inner).filter(|_| full_clocks(inner))
        } else {
            parse_single(inner)
        };
        match targets {
            Some(t) => push_cites(&mut cited, &mut text, t),
            None => text.push_str(&from[..end + close.len()]),
        }
        rest = &from[end + close.len()..];
        // "[00:01] [00:02]" or "[[t:1]], [[t:2]]": swallow the gap between markers.
        let gap = rest.len() - rest.trim_start_matches([' ', ',']).len();
        if gap > 0 && targets_follow(&rest[gap..]) {
            rest = &rest[gap..];
        }
    }
    text.push_str(rest);
    if !text.is_empty() {
        cited.pieces.push(Mark::Text(text));
    }
    cited
}

/// Whether every time in a list is a full "hh:mm:ss".
fn full_clocks(inner: &str) -> bool {
    list(inner).all(|item| {
        let times = RANGE_SEPS.iter().find_map(|sep| item.split_once(sep)).map_or(vec![item], |(a, b)| vec![a, b]);
        times.iter().all(|t| t.matches(':').count() == 2)
    })
}

/// Whether `s` starts with another citation.
fn targets_follow(s: &str) -> bool {
    if let Some(r) = s.strip_prefix("[[") {
        return r.find("]]").is_some_and(|e| parse_double(&r[..e]).is_some());
    }
    for (open, close) in [('[', ']'), ('(', ')')] {
        if let Some(r) = s.strip_prefix(open) {
            return r.find(close).is_some_and(|e| parse_single(&r[..e]).is_some() && (open == '[' || full_clocks(&r[..e])));
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secs: f64) -> Target {
        Target::Time { secs, end: None }
    }

    fn render(c: &Cited) -> String {
        c.pieces
            .iter()
            .map(|p| match p {
                Mark::Text(t) => t.clone(),
                Mark::Cite { index, .. } => format!("^{index}"),
            })
            .collect()
    }

    #[test]
    fn strips_raw_brackets() {
        let c = parse("Spend is frozen [00:00:27]. Anna sends it [0:36] and [1:02:03].");
        assert_eq!(render(&c), "Spend is frozen^1. Anna sends it^2 and^3.");
        assert_eq!(c.sources, vec![t(27.), t(36.), t(3723.)]);
        assert_eq!(c.sources[2].time_label(), "1:02:03");
        assert_eq!(c.sources[1].time_label(), "0:36");
    }

    #[test]
    fn ranges_and_lists() {
        let c = parse("Budget [00:01:00–00:01:02] and [00:01:00 - 00:01:05]; also [00:01:00, 00:01:17].");
        assert_eq!(render(&c), "Budget^1 and^2; also^3^4.");
        assert_eq!(c.sources[0], Target::Time { secs: 60., end: Some(62.) });
        assert_eq!(c.sources[0].time_label(), "1:00–1:02");
        assert_eq!(c.sources[1].time_label(), "1:00–1:05");
        assert_eq!(c.sources[2..], [t(60.), t(77.)]);
        // Parenthesized times and the transcript tool's "[27s 00:00:27]".
        let c = parse("Frozen (00:00:27) and [27s 00:00:27].");
        assert_eq!(render(&c), "Frozen^1 and^1.");
        assert_eq!(render(&parse("Stand-up (9:30–10:00) daily.")), "Stand-up (9:30–10:00) daily.");
    }

    #[test]
    fn live_and_library_markers() {
        let c = parse("Frozen [[t:27]]. Anna [[t:00:00:36]], [[t:60-62]]. Done [[t:27]].");
        assert_eq!(render(&c), "Frozen^1. Anna^2^3. Done^1.");
        assert_eq!(c.sources, vec![t(27.), t(36.), Target::Time { secs: 60., end: Some(62.) }]);

        let c = parse("Frozen [[m:1@27]]. Reason [[m:1@6, m:2@40]].\n- Agreed [[m:1@27.4]]");
        assert_eq!(render(&c), "Frozen^1. Reason^2^3.\n- Agreed^1");
        assert_eq!(c.sources[2], Target::Meeting { meeting_id: 2, secs: 40. });
        assert_eq!(c.lines().len(), 2);
        assert_eq!(c.lines()[1], vec![Mark::Text("- Agreed".into()), Mark::Cite { index: 1, target: Target::Meeting { meeting_id: 1, secs: 27. } }]);
    }

    #[test]
    fn document_markers() {
        let c = parse("VPN rotates [[d:3@12]]. Laptops [[d:3@12, d:4@1]], also [[m:1@5]] and [[d:5]].");
        assert_eq!(render(&c), "VPN rotates^1. Laptops^1^2, also^3 and^4.");
        assert_eq!(c.sources[1], Target::Document { doc_id: 4, loc: 1 });
        assert_eq!(c.sources[3], Target::Document { doc_id: 5, loc: 1 });
        assert_eq!(parse("[[d:3@p. 7]]").sources, vec![Target::Document { doc_id: 3, loc: 7 }]);
        assert_eq!(render(&parse("Bad [[d:x@1]]")), "Bad [[d:x@1]]");
    }

    #[test]
    fn dedups_adjacent_and_keeps_non_citations() {
        let c = parse("Frozen [00:00:27] [00:00:27] [[t:27]]. See [1] and (note) or [[x:1]] [10:99].");
        assert_eq!(render(&c), "Frozen^1. See [1] and (note) or [[x:1]] [10:99].");
        assert_eq!(c.sources.len(), 1);
        assert_eq!(c.plain(), "Frozen (0:27). See [1] and (note) or [[x:1]] [10:99].");
        assert!(parse("No citations here.").sources.is_empty());
    }

    #[test]
    fn hides_markers_still_streaming() {
        assert_eq!(render(&parse("Frozen [[m:1@2")), "Frozen ");
        assert_eq!(render(&parse("Frozen [00:0")), "Frozen ");
        assert_eq!(render(&parse("Options [a, b")), "Options [a, b");
        assert_eq!(render(&parse("Price (about")), "Price (about");
    }
}
