//! Text out of uploaded files: Markdown and plain text, PDF (page by
//! page), Word (.docx), HTML and OpenDocument text (.odt). Each file
//! becomes sections with a location (a PDF page, else a running section
//! number) and the heading they sit under.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::i18n::trf;

/// What kind of file a document is (stored in `documents.kind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Markdown,
    Text,
    Pdf,
    Docx,
    Odt,
    Html,
}

impl Kind {
    pub const EXTENSIONS: &[&str] = &["md", "markdown", "txt", "text", "rst", "org", "csv", "log", "pdf", "docx", "odt", "html", "htm"];

    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "md" | "markdown" => Kind::Markdown,
            "txt" | "text" | "rst" | "org" | "csv" | "log" => Kind::Text,
            "pdf" => Kind::Pdf,
            "docx" => Kind::Docx,
            "odt" => Kind::Odt,
            "html" | "htm" => Kind::Html,
            _ => return None,
        })
    }

    pub fn id(self) -> &'static str {
        match self {
            Kind::Markdown => "md",
            Kind::Text => "txt",
            Kind::Pdf => "pdf",
            Kind::Docx => "docx",
            Kind::Odt => "odt",
            Kind::Html => "html",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Kind::Markdown, Kind::Text, Kind::Pdf, Kind::Docx, Kind::Odt, Kind::Html].into_iter().find(|k| k.id() == s)
    }

    /// Whether a location is a page (PDF) rather than a section number.
    pub fn paged(self) -> bool {
        self == Kind::Pdf
    }
}

/// A piece of a document as written: its location, the heading it sits
/// under ("" if none) and its text.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub loc: u32,
    pub heading: String,
    pub text: String,
}

/// The text of a file, in sections, and its page count for PDFs.
pub fn extract(path: &Path, kind: Kind) -> Result<(Vec<Section>, Option<u32>)> {
    let bytes = std::fs::read(path).with_context(|| trf("{} konnte nicht gelesen werden", &[&path.display()]))?;
    match kind {
        Kind::Markdown => Ok((markdown(&text_of(&bytes)), None)),
        Kind::Text => Ok((plain(&text_of(&bytes)), None)),
        Kind::Html => Ok((plain(&html_text(&text_of(&bytes))), None)),
        Kind::Pdf => {
            let pages = pdf_pages(&bytes)?;
            let n = pages.len() as u32;
            let sections = pages
                .into_iter()
                .enumerate()
                .map(|(i, text)| Section { loc: i as u32 + 1, heading: String::new(), text: tidy(&text) })
                .filter(|s| !s.text.trim().is_empty())
                .collect();
            Ok((sections, Some(n)))
        }
        Kind::Docx => Ok((headed(docx_paragraphs(&bytes)?), None)),
        Kind::Odt => Ok((headed(odt_paragraphs(&bytes)?), None)),
    }
}

/// UTF-8 (lossy, without a BOM); Latin-1 when it isn't UTF-8 at all.
fn text_of(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

/// Collapses runs of spaces and blank lines that PDF extraction leaves.
fn tidy(text: &str) -> String {
    let mut out = String::new();
    let mut blank = 0;
    for line in text.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            blank += 1;
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        blank = 0;
        out.push_str(&line);
    }
    out
}

/// Markdown split at its headings; each section knows its heading path
/// ("Setup › Network").
pub fn markdown(text: &str) -> Vec<Section> {
    let mut out = Vec::new();
    let mut path: Vec<(usize, String)> = Vec::new();
    let mut body = String::new();
    let mut in_code = false;
    let flush = |path: &[(usize, String)], body: &mut String, out: &mut Vec<Section>| {
        if !body.trim().is_empty() {
            let heading = path.iter().map(|(_, h)| h.as_str()).collect::<Vec<_>>().join(" › ");
            out.push(Section { loc: out.len() as u32 + 1, heading, text: body.trim().to_string() });
        }
        body.clear();
    };
    for line in text.lines() {
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            in_code = !in_code;
        }
        let level = line.chars().take_while(|c| *c == '#').count();
        let is_heading = !in_code && (1..=6).contains(&level) && line[level..].starts_with(' ');
        if is_heading {
            flush(&path, &mut body, &mut out);
            let title = line[level..].trim().trim_end_matches('#').trim().to_string();
            path.retain(|(l, _)| *l < level);
            path.push((level, title));
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    flush(&path, &mut body, &mut out);
    out
}

/// Plain text: one section (chunking splits it further).
fn plain(text: &str) -> Vec<Section> {
    let text = text.trim();
    if text.is_empty() { Vec::new() } else { vec![Section { loc: 1, heading: String::new(), text: text.to_string() }] }
}

/// Paragraphs with their heading level (0 = body text) as sections under
/// the headings.
fn headed(paragraphs: Vec<(usize, String)>) -> Vec<Section> {
    let mut md = String::new();
    for (level, text) in paragraphs {
        if level > 0 {
            md.push_str(&format!("{} {}\n", "#".repeat(level.min(6)), text));
        } else {
            md.push_str(&text);
            md.push_str("\n\n");
        }
    }
    markdown(&md)
}

/// PDF text per page. pdf-extract panics on some malformed files: that
/// becomes an error for this document, not a crash.
fn pdf_pages(bytes: &[u8]) -> Result<Vec<String>> {
    let bytes = bytes.to_vec();
    let result = std::panic::catch_unwind(move || pdf_extract::extract_text_from_mem_by_pages(&bytes));
    match result {
        Ok(Ok(pages)) => {
            if pages.iter().all(|p| p.trim().is_empty()) {
                bail!("{}", crate::i18n::tr("Das PDF enthält keinen Text (gescannt?) – OCR wird nicht unterstützt"));
            }
            Ok(pages)
        }
        Ok(Err(e)) => bail!("{}", trf("PDF konnte nicht gelesen werden: {}", &[&e])),
        Err(_) => bail!("{}", crate::i18n::tr("PDF konnte nicht gelesen werden (beschädigt oder nicht unterstützt)")),
    }
}

/// Word heading style names (English and German Word).
const HEADING_STYLES: &[&str] = &["heading", "berschrift", "überschrift"]; // i18n: not UI

/// Paragraph texts of a .docx with heading levels from their styles
/// ("Heading1", "Überschrift1", "Title").
fn docx_paragraphs(bytes: &[u8]) -> Result<Vec<(usize, String)>> {
    let xml = zip_entry(bytes, "word/document.xml")?;
    Ok(xml_paragraphs(&xml, "w:p", "w:t", |p| {
        let style = attr_after(p, "<w:pStyle", "w:val").unwrap_or_default().to_ascii_lowercase();
        if style == "title" {
            1
        } else if let Some(n) = HEADING_STYLES.iter().find_map(|h| style.find(h).map(|i| &style[i + h.len()..])) {
            n.trim().parse().unwrap_or(1)
        } else {
            0
        }
    }))
}

/// Paragraph texts of an .odt (`text:h` are headings).
fn odt_paragraphs(bytes: &[u8]) -> Result<Vec<(usize, String)>> {
    let xml = zip_entry(bytes, "content.xml")?;
    let mut out = Vec::new();
    let mut rest = xml.as_str();
    while let Some(start) = rest.find("<text:") {
        rest = &rest[start..];
        let heading = rest.starts_with("<text:h");
        let para = heading || rest.starts_with("<text:p");
        if !para {
            rest = &rest[6..];
            continue;
        }
        let tag = if heading { "</text:h>" } else { "</text:p>" };
        let Some(end) = rest.find(tag) else { break };
        let open_end = rest.find('>').unwrap_or(0);
        let self_closing = rest[..open_end].ends_with('/');
        if self_closing {
            rest = &rest[open_end + 1..];
            continue;
        }
        let inner = &rest[open_end + 1..end];
        let level = if heading { attr_after(rest, "<text:h", "text:outline-level").and_then(|v| v.parse().ok()).unwrap_or(1) } else { 0 };
        let text = unescape(&strip_tags(inner)).trim().to_string();
        if !text.is_empty() {
            out.push((level, text));
        }
        rest = &rest[end + tag.len()..];
    }
    Ok(out)
}

/// Paragraphs `<p_tag>…</p_tag>` and the text in their `<t_tag>` runs.
fn xml_paragraphs(xml: &str, p_tag: &str, t_tag: &str, level: impl Fn(&str) -> usize) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let close_p = format!("</{p_tag}>");
    let open_t = format!("<{t_tag}");
    let close_t = format!("</{t_tag}>");
    let mut rest = xml;
    while let Some(start) = rest.find(&format!("<{p_tag}")) {
        rest = &rest[start..];
        // `<w:pPr>` etc. share the prefix: only `<w:p>` / `<w:p ` open a paragraph.
        let next = rest.as_bytes().get(p_tag.len() + 1).copied();
        if !matches!(next, Some(b'>') | Some(b' ')) {
            rest = &rest[p_tag.len() + 1..];
            continue;
        }
        let Some(end) = rest.find(&close_p) else { break };
        let p = &rest[..end];
        let mut text = String::new();
        let mut runs = p;
        while let Some(t) = runs.find(&open_t) {
            runs = &runs[t..];
            let after = runs.as_bytes().get(t_tag.len() + 1).copied();
            let Some(gt) = runs.find('>') else { break };
            if !matches!(after, Some(b'>') | Some(b' ')) || runs[..gt].ends_with('/') {
                runs = &runs[gt + 1..];
                continue;
            }
            let Some(e) = runs.find(&close_t) else { break };
            text.push_str(&unescape(&runs[gt + 1..e]));
            runs = &runs[e + close_t.len()..];
        }
        let text = text.trim().to_string();
        if !text.is_empty() {
            out.push((level(p), text));
        }
        rest = &rest[end + close_p.len()..];
    }
    out
}

/// The value of `attr="…"` in the first `tag` element of `s`.
fn attr_after<'a>(s: &'a str, tag: &str, attr: &str) -> Option<&'a str> {
    let at = s.find(tag)?;
    let el = &s[at..at + s[at..].find('>')?];
    let a = el.find(&format!("{attr}=\""))? + attr.len() + 2;
    let end = el[a..].find('"')?;
    Some(&el[a..a + end])
}

fn zip_entry(bytes: &[u8], name: &str) -> Result<String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).context(crate::i18n::tr("Datei ist kein gültiges Office-Dokument"))?;
    let mut entry = zip.by_name(name).context(crate::i18n::tr("Datei ist kein gültiges Office-Dokument"))?;
    let mut xml = String::new();
    entry.read_to_string(&mut xml)?;
    Ok(xml)
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&#39;", "'").replace("&nbsp;", " ").replace("&amp;", "&")
}

/// Readable text of an HTML page: no scripts or styles, block elements
/// on their own lines, headings as Markdown-ish lines.
fn html_text(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::new();
    let mut i = 0;
    while i < html.len() {
        let Some(lt) = html[i..].find('<').map(|p| p + i) else {
            out.push_str(&html[i..]);
            break;
        };
        out.push_str(&html[i..lt]);
        let Some(gt) = html[lt..].find('>').map(|p| p + lt) else { break };
        let tag = lower[lt + 1..gt].trim_start_matches('/').split([' ', '\n', '\t', '/']).next().unwrap_or_default().to_string();
        if (tag == "script" || tag == "style") && !lower[lt + 1..].starts_with('/') {
            let close = format!("</{tag}");
            i = lower[gt..].find(&close).map_or(html.len(), |p| gt + p);
            continue;
        }
        if ["p", "div", "br", "li", "tr", "h1", "h2", "h3", "h4", "h5", "h6", "section", "article", "table", "ul", "ol"].contains(&tag.as_str()) {
            out.push('\n');
        }
        i = gt + 1;
    }
    tidy(&unescape(&out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_sections_keep_heading_paths() {
        let s = markdown("Intro text\n# Setup\nInstall it.\n## Network\nPort 8080.\n```\n# not a heading\n```\n# Usage\nRun it.");
        assert_eq!(s.len(), 4);
        assert_eq!(s[0].heading, "");
        assert_eq!(s[2].heading, "Setup › Network");
        assert!(s[2].text.contains("# not a heading"));
        assert_eq!((s[3].loc, s[3].heading.as_str()), (4, "Usage"));
    }

    #[test]
    fn docx_xml_paragraphs() {
        let xml = r#"<w:body><w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>Budget</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Q3 is </w:t></w:r><w:r><w:t>10% &amp; more</w:t></w:r></w:p><w:p/></w:body>"#;
        let p = xml_paragraphs(xml, "w:p", "w:t", |p| if attr_after(p, "<w:pStyle", "w:val").is_some() { 2 } else { 0 });
        assert_eq!(p, vec![(2, "Budget".to_string()), (0, "Q3 is 10% & more".to_string())]);
        let s = headed(p);
        assert_eq!((s[0].heading.as_str(), s[0].text.as_str()), ("Budget", "Q3 is 10% & more"));
    }

    #[test]
    fn html_drops_scripts() {
        let t = html_text("<html><head><style>p{}</style><script>var x=1;</script></head><body><h1>Title</h1><p>One &amp; two</p><p>Three</p></body></html>");
        assert_eq!(t, "Title\n\nOne & two\n\nThree");
    }
}
