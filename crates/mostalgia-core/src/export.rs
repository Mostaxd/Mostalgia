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

/// A file name for the export: date plus a slug of the title.
pub fn suggested_name(m: &Meeting, format: Format) -> String {
    let slug: String = m
        .title
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    format!("{}-{}.{}", m.started_at.format("%Y-%m-%d"), slug, format.extension())
}

pub fn write(m: &Meeting, format: Format, path: &Path) -> Result<()> {
    let result = match format {
        Format::Markdown => std::fs::write(path, markdown(m)).map_err(Into::into),
        Format::Text => std::fs::write(path, text(m)).map_err(Into::into),
        Format::Srt => std::fs::write(path, srt(m)).map_err(Into::into),
        Format::Docx => docx(m, path),
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
    let mut out = format!("# {}\n\n_{}_\n\n", m.title, header(m));
    if let Some(body) = summary_body(m) {
        out.push_str(body.trim_end());
        out.push_str("\n\n");
    }
    if !m.notes.is_empty() {
        out.push_str(&format!("## {}\n\n", tr("Notizen von Mori")));
        for line in note_lines(m) {
            out.push_str(&format!("- {line}\n"));
        }
        out.push('\n');
    }
    if !m.chat.is_empty() {
        out.push_str(&format!("## {}\n\n", tr("Chat mit Mori")));
        for (q, a) in chat_turns(m) {
            out.push_str(&format!("**{q}**\n\n{a}\n\n"));
        }
    }
    out.push_str(&format!("## {}\n\n", tr("Transkript")));
    for s in &m.segments {
        out.push_str(&format!("**[{}] {}:** {}\n\n", format_ts(s.start_secs), i18n::speaker(s.speaker_label()), s.text));
    }
    out
}

pub fn text(m: &Meeting) -> String {
    let mut out = format!("{}\n{}\n\n", m.title, header(m));
    if let Some(body) = summary_body(m) {
        for line in body.lines() {
            let line = line.trim_start_matches('#').trim().replace("**", "");
            out.push_str(&line);
            out.push('\n');
        }
        out.push('\n');
    }
    if !m.notes.is_empty() {
        out.push_str(&format!("{}\n", tr("Notizen von Mori")));
        for line in note_lines(m) {
            out.push_str(&format!("- {line}\n"));
        }
        out.push('\n');
    }
    if !m.chat.is_empty() {
        out.push_str(&format!("{}\n", tr("Chat mit Mori")));
        for (q, a) in chat_turns(m) {
            out.push_str(&format!("> {q}\n{a}\n\n"));
        }
    }
    out.push_str(&format!("{}\n\n", tr("Transkript")));
    for s in &m.segments {
        out.push_str(&format!("[{}] {}: {}\n", format_ts(s.start_secs), i18n::speaker(s.speaker_label()), s.text));
    }
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
    let mut doc = Docx::new()
        .add_paragraph(Paragraph::new().add_run(Run::new().add_text(&m.title).bold().size(40)))
        .add_paragraph(Paragraph::new().add_run(Run::new().add_text(header(m)).color("666666").size(20)));
    if let Some(body) = summary_body(m) {
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
    if !m.notes.is_empty() {
        doc = doc.add_paragraph(Paragraph::new().add_run(Run::new().add_text(tr("Notizen von Mori")).bold().size(28)));
        for line in note_lines(m) {
            doc = doc.add_paragraph(para(vec![Run::new().add_text("• ").size(22), Run::new().add_text(line).size(22)]));
        }
    }
    if !m.chat.is_empty() {
        doc = doc.add_paragraph(Paragraph::new().add_run(Run::new().add_text(tr("Chat mit Mori")).bold().size(28)));
        for (q, a) in chat_turns(m) {
            doc = doc.add_paragraph(Paragraph::new().add_run(Run::new().add_text(q).bold().size(22)));
            for line in a.lines().filter(|l| !l.trim().is_empty()) {
                doc = doc.add_paragraph(para(runs(line, 22)));
            }
        }
    }
    doc = doc.add_paragraph(Paragraph::new().add_run(Run::new().add_text(tr("Transkript")).bold().size(28)));
    for s in &m.segments {
        doc = doc.add_paragraph(
            Paragraph::new()
                .add_run(Run::new().add_text(format!("[{}] ", format_ts(s.start_secs))).color("888888").size(20))
                .add_run(Run::new().add_text(format!("{}: ", i18n::speaker(s.speaker_label()))).bold().size(22))
                .add_run(Run::new().add_text(&s.text).size(22)),
        );
    }
    let file = std::fs::File::create(path)?;
    doc.build().pack(file)?;
    Ok(())
}
