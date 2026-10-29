//! Exporting a meeting: Markdown, plain text, SRT subtitles, Word (.docx).

use std::path::Path;

use anyhow::{Context, Result};
use docx_rs::{Docx, Paragraph, Run};

use crate::chunker::format_ts;
use crate::i18n::{self, tr, trf};
use crate::store::Meeting;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Markdown,
    Text,
    Srt,
    Docx,
}

impl Format {
    pub const ALL: [Format; 4] = [Format::Markdown, Format::Text, Format::Srt, Format::Docx];

    pub fn extension(self) -> &'static str {
        match self {
            Format::Markdown => "md",
            Format::Text => "txt",
            Format::Srt => "srt",
            Format::Docx => "docx",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Markdown => "Markdown",
            Format::Text => tr("Text"),
            Format::Srt => tr("Untertitel (SRT)"),
            Format::Docx => "Word (.docx)",
        }
    }

    pub fn from_extension(ext: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.extension().eq_ignore_ascii_case(ext))
    }
}

/// Which parts of a meeting an export holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parts {
    pub summary: bool,
    /// The companion's notes.
    pub notes: bool,
    /// The chat with the companion.
    pub chat: bool,
    pub transcript: bool,
}

impl Parts {
    /// Everything: summary, notes, chat and transcript.
    pub const ALL: Parts = Parts { summary: true, notes: true, chat: true, transcript: true };
    pub const NOTES: Parts = Parts { summary: false, notes: true, chat: false, transcript: false };
    pub const SUMMARY: Parts = Parts { summary: true, notes: false, chat: false, transcript: false };
    /// Summary and notes, what goes into the documents drive.
    pub const SUMMARY_NOTES: Parts = Parts { summary: true, notes: true, chat: false, transcript: false };

    /// A short id for actions and file names ("all", "notes", "summary",
    /// "summary-notes").
    pub fn id(self) -> &'static str {
        match self {
            Parts::NOTES => "notes",
            Parts::SUMMARY => "summary",
            Parts::SUMMARY_NOTES => "summary-notes",
            _ => "all",
        }
    }

    pub fn parse(id: &str) -> Parts {
        [Parts::NOTES, Parts::SUMMARY, Parts::SUMMARY_NOTES].into_iter().find(|p| p.id() == id).unwrap_or(Parts::ALL)
    }

    /// The formats that fit: subtitles only with the transcript.
    pub fn formats(self) -> Vec<Format> {
        Format::ALL.into_iter().filter(|f| *f != Format::Srt || self.transcript).collect()
    }
}

/// A file name for the export: date plus a slug of the title.
pub fn suggested_name(m: &Meeting, format: Format) -> String {
    suggested_name_of(m, Parts::ALL, format)
}

/// A file name for an export of `parts`: "2026-10-29-Weekly-notes.md".
pub fn suggested_name_of(m: &Meeting, parts: Parts, format: Format) -> String {
    let slug: String = m
        .title
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let suffix = match parts {
        Parts::ALL => String::new(),
        p => format!("-{}", p.id()),
    };
    format!("{}-{}{suffix}.{}", m.started_at.format("%Y-%m-%d"), slug, format.extension())
}

pub fn write(m: &Meeting, format: Format, path: &Path) -> Result<()> {
    write_parts(m, Parts::ALL, format, path)
}

/// Writes only `parts` of the meeting (SRT is always the transcript).
pub fn write_parts(m: &Meeting, parts: Parts, format: Format, path: &Path) -> Result<()> {
    let result = match format {
        Format::Markdown => std::fs::write(path, markdown_parts(m, parts)).map_err(Into::into),
        Format::Text => std::fs::write(path, text_parts(m, parts)).map_err(Into::into),
        Format::Srt => std::fs::write(path, srt(m)).map_err(Into::into),
        Format::Docx => docx_parts(m, parts, path),
    };
    result.with_context(|| format!("writing {}", path.display()))
}

fn header(m: &Meeting) -> String {
    let mins = (m.duration_secs / 60.0).round();
    let length = if mins < 1.0 { tr("< 1 min").to_string() } else { trf("{} min", &[&mins]) };
    format!("{} · {length}", i18n::date_time(&m.started_at))
}

/// The summary without its own "# Title" line (the export has one).
fn summary_body(m: &Meeting) -> Option<&str> {
    let md = m.summary.as_ref()?.markdown.trim_start();
    Some(match md.strip_prefix("# ") {
        Some(rest) => rest.split_once('\n').map_or("", |(_, b)| b).trim_start(),
        None => md,
    })
}

/// "[02:03] Decision: Freeze marketing spend" for each of Mori's notes.
fn note_lines(m: &Meeting) -> Vec<String> {
    m.notes
        .iter()
        .map(|n| {
            let kind = crate::companion::PointKind::parse(&n.kind).map_or(n.kind.as_str(), |k| k.label());
            format!("[{}] {kind}: {}", format_ts(n.at_secs), n.text)
        })
        .collect()
}

/// (question, answer) of the chat with Mori, citations as "(0:27)".
fn chat_turns(m: &Meeting) -> Vec<(String, String)> {
    m.chat.iter().map(|t| (t.question.clone(), crate::ask::cite::parse(&t.answer).plain().trim().to_string())).collect()
}

pub fn markdown(m: &Meeting) -> String {
    markdown_parts(m, Parts::ALL)
}

pub fn markdown_parts(m: &Meeting, parts: Parts) -> String {
    let mut out = format!("# {}\n\n_{}_\n\n", m.title, header(m));
    if let Some(body) = summary_body(m).filter(|_| parts.summary) {
        out.push_str(body.trim_end());
        out.push_str("\n\n");
    }
    if parts.notes && !m.notes.is_empty() {
        out.push_str(&format!("## {}\n\n", &trf("Notizen von {}", &[&companion_of(m)])));
        for line in note_lines(m) {
            out.push_str(&format!("- {line}\n"));
        }
        out.push('\n');
    }
    if parts.chat && !m.chat.is_empty() {
        out.push_str(&format!("## {}\n\n", &trf("Chat mit {}", &[&companion_of(m)])));
        for (q, a) in chat_turns(m) {
            out.push_str(&format!("**{q}**\n\n{a}\n\n"));
        }
    }
    if parts.transcript {
        out.push_str(&format!("## {}\n\n", tr("Transkript")));
        for s in &m.segments {
            out.push_str(&format!("**[{}] {}:** {}\n\n", format_ts(s.start_secs), i18n::speaker(s.speaker_label()), s.text));
        }
    }
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

pub fn text(m: &Meeting) -> String {
    text_parts(m, Parts::ALL)
}

pub fn text_parts(m: &Meeting, parts: Parts) -> String {
    let mut out = format!("{}\n{}\n\n", m.title, header(m));
    if let Some(body) = summary_body(m).filter(|_| parts.summary) {
        for line in body.lines() {
            let line = line.trim_start_matches('#').trim().replace("**", "");
            out.push_str(&line);
            out.push('\n');
        }
        out.push('\n');
    }
    if parts.notes && !m.notes.is_empty() {
        out.push_str(&format!("{}\n", &trf("Notizen von {}", &[&companion_of(m)])));
        for line in note_lines(m) {
            out.push_str(&format!("- {line}\n"));
        }
        out.push('\n');
    }
    if parts.chat && !m.chat.is_empty() {
        out.push_str(&format!("{}\n", &trf("Chat mit {}", &[&companion_of(m)])));
        for (q, a) in chat_turns(m) {
            out.push_str(&format!("> {q}\n{a}\n\n"));
        }
    }
    if parts.transcript {
        out.push_str(&format!("{}\n\n", tr("Transkript")));
        for s in &m.segments {
            out.push_str(&format!("[{}] {}: {}\n", format_ts(s.start_secs), i18n::speaker(s.speaker_label()), s.text));
        }
    }
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

pub fn srt(m: &Meeting) -> String {
    let ts = |secs: f64| {
        let ms = (secs.max(0.0) * 1000.0).round() as u64;
        format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, (ms / 60_000) % 60, (ms / 1000) % 60, ms % 1000)
    };
    let mut out = String::new();
    for (i, s) in m.segments.iter().enumerate() {
        let end = s.end_secs.max(s.start_secs + 0.5);
        out.push_str(&format!("{}\n{} --> {}\n{}: {}\n\n", i + 1, ts(s.start_secs), ts(end), i18n::speaker(s.speaker_label()), s.text));
    }
    out
}

/// A run list for a line with `**bold**` spans.
fn runs(line: &str, size: usize) -> Vec<Run> {
    line.split("**")
        .enumerate()
        .filter(|(_, part)| !part.is_empty())
        .map(|(i, part)| {
            let run = Run::new().add_text(part).size(size);
            if i % 2 == 1 { run.bold() } else { run }
        })
        .collect()
}

fn para(runs: Vec<Run>) -> Paragraph {
    runs.into_iter().fold(Paragraph::new(), |p, r| p.add_run(r))
}

pub fn docx(m: &Meeting, path: &Path) -> Result<()> {
    docx_parts(m, Parts::ALL, path)
}

pub fn docx_parts(m: &Meeting, parts: Parts, path: &Path) -> Result<()> {
    let mut doc = Docx::new()
        .add_paragraph(Paragraph::new().add_run(Run::new().add_text(&m.title).bold().size(40)))
        .add_paragraph(Paragraph::new().add_run(Run::new().add_text(header(m)).color("666666").size(20)));
    if let Some(body) = summary_body(m).filter(|_| parts.summary) {
        for line in body.lines().map(str::trim_end) {
            doc = doc.add_paragraph(if let Some(h) = line.strip_prefix("## ") {
                Paragraph::new().add_run(Run::new().add_text(h).bold().size(28))
            } else if let Some(item) = line.strip_prefix("- [ ] ") {
                para([vec![Run::new().add_text("☐ ").size(22)], runs(item, 22)].concat())
            } else if let Some(item) = line.strip_prefix("- ") {
                para([vec![Run::new().add_text("• ").size(22)], runs(item, 22)].concat())
            } else {
                para(runs(line, 22))
            });
        }
    }
    if parts.notes && !m.notes.is_empty() {
        doc = doc.add_paragraph(Paragraph::new().add_run(Run::new().add_text(&trf("Notizen von {}", &[&companion_of(m)])).bold().size(28)));
        for line in note_lines(m) {
            doc = doc.add_paragraph(para(vec![Run::new().add_text("• ").size(22), Run::new().add_text(line).size(22)]));
        }
    }
    if parts.chat && !m.chat.is_empty() {
        doc = doc.add_paragraph(Paragraph::new().add_run(Run::new().add_text(&trf("Chat mit {}", &[&companion_of(m)])).bold().size(28)));
        for (q, a) in chat_turns(m) {
            doc = doc.add_paragraph(Paragraph::new().add_run(Run::new().add_text(q).bold().size(22)));
            for line in a.lines().filter(|l| !l.trim().is_empty()) {
                doc = doc.add_paragraph(para(runs(line, 22)));
            }
        }
    }
    if parts.transcript {
        doc = doc.add_paragraph(Paragraph::new().add_run(Run::new().add_text(tr("Transkript")).bold().size(28)));
        for s in &m.segments {
            doc = doc.add_paragraph(
                Paragraph::new()
                    .add_run(Run::new().add_text(format!("[{}] ", format_ts(s.start_secs))).color("888888").size(20))
                    .add_run(Run::new().add_text(format!("{}: ", i18n::speaker(s.speaker_label()))).bold().size(22))
                    .add_run(Run::new().add_text(&s.text).size(22)),
            );
        }
    }
    let file = std::fs::File::create(path)?;
    doc.build().pack(file)?;
    Ok(())
}

/// The companion who took the meeting's notes and chat.
fn companion_of(m: &Meeting) -> &'static str {
    crate::companion::Character::parse(&m.companion).unwrap_or(crate::companion::Character::Mori).name()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Source;
    use crate::store::{Note, StoredSegment, Summary};
    use chrono::{Local, TimeZone};

    fn meeting() -> Meeting {
        Meeting {
            id: 1,
            title: "Weekly sync".into(),
            started_at: Local.with_ymd_and_hms(2026, 10, 29, 9, 0, 0).unwrap(),
            duration_secs: 600.0,
            language: "en".into(),
            dir: Default::default(),
            segments: vec![StoredSegment { id: 1, source: Source::System, speaker: None, start_secs: 3.0, end_secs: 5.0, text: "Let's ship in March.".into() }],
            bookmarks: Vec::new(),
            notes: vec![Note {
                id: 1,
                kind: "decision".into(),
                text: "Ship in March".into(),
                at_secs: 4.0,
                created_at: Local::now(),
                pinned: false,
                origin: "live".into(),
            }],
            chat: Vec::new(),
            summary: Some(Summary { provider: "p".into(), model: "m".into(), effort: None, created_at: Local::now(), markdown: "# Weekly sync\n\n## Kernpunkte\n- Launch date".into() }),
            transcript_engine: None,
            transcript_error: None,
            companion: "mori".into(),
        }
    }

    #[test]
    fn exports_only_the_parts_asked_for() {
        let m = meeting();
        let all = markdown(&m);
        assert!(all.contains("Launch date") && all.contains("Ship in March") && all.contains("Let's ship in March."));
        let notes = markdown_parts(&m, Parts::NOTES);
        assert!(notes.starts_with("# Weekly sync\n") && notes.contains("Ship in March"));
        assert!(!notes.contains("Launch date") && !notes.contains("Let's ship"));
        let summary = text_parts(&m, Parts::SUMMARY_NOTES);
        assert!(summary.contains("Launch date") && summary.contains("Ship in March") && !summary.contains("Let's ship"));
        assert_eq!(suggested_name_of(&m, Parts::NOTES, Format::Markdown), "2026-10-29-Weekly-sync-notes.md");
        assert_eq!(suggested_name(&m, Format::Docx), "2026-10-29-Weekly-sync.docx");
        assert_eq!(Parts::parse(Parts::NOTES.id()), Parts::NOTES);
        assert!(!Parts::NOTES.formats().contains(&Format::Srt));
    }
}
