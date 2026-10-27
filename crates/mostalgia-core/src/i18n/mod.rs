//! UI language. Strings are written in German in the code and looked up in
//! per-area English tables (`tr("Neue Aufnahme")` → "New recording"), so a
//! missing translation falls back to German instead of breaking. Another
//! language (e.g. Arabic) is another set of tables plus a `Lang` variant.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

use chrono::{DateTime, Local};

mod app;
mod cli;
mod core;
mod meeting;
mod recording;
mod settings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    De,
    En,
}

static LANG: AtomicU8 = AtomicU8::new(0);

/// The language for a config value: "de", "en", or "system" (from $LANG /
/// $LC_ALL / $LC_MESSAGES: German if it starts with "de", else English).
pub fn resolve(setting: &str) -> Lang {
    match setting {
        "de" => Lang::De,
        "en" => Lang::En,
        _ => {
            let env = ["LC_ALL", "LC_MESSAGES", "LANG"]
                .iter()
                .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
                .unwrap_or_default();
            if env.starts_with("de") { Lang::De } else { Lang::En }
        }
    }
}

pub fn set_lang(lang: Lang) {
    LANG.store(lang as u8, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    if LANG.load(Ordering::Relaxed) == Lang::En as u8 { Lang::En } else { Lang::De }
}

fn english() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| {
        [app::EN, cli::EN, core::EN, meeting::EN, recording::EN, settings::EN]
            .into_iter()
            .flatten()
            .copied()
            .collect()
    })
}

/// Translates a German UI string into the current language.
pub fn tr(de: &'static str) -> &'static str {
    match lang() {
        Lang::De => de,
        Lang::En => english().get(de).copied().unwrap_or(de),
    }
}

/// Translates a template with `{}` placeholders, then fills them in order:
/// `trf("{} Zeilen", &[&n])`.
pub fn trf(de: &'static str, args: &[&dyn std::fmt::Display]) -> String {
    let mut out = String::new();
    let mut args = args.iter();
    let mut parts = tr(de).split("{}").peekable();
    while let Some(part) = parts.next() {
        out.push_str(part);
        if parts.peek().is_some() {
            if let Some(a) = args.next() {
                out.push_str(&a.to_string());
            }
        }
    }
    out
}

/// How a stored speaker label is shown: "Ich", "Andere" and "Sprecher N"
/// are data (German in the database) and get translated; names people gave
/// stay as they are.
pub fn speaker(label: &str) -> String {
    match label {
        "Ich" => tr("Ich").into(),
        "Andere" => tr("Andere").into(),
        other => match other.strip_prefix("Sprecher ").and_then(|n| n.parse::<usize>().ok()) {
            Some(n) => trf("Sprecher {}", &[&n]),
            None => other.to_string(),
        },
    }
}

/// "27.10.2026" / "Oct 27, 2026".
pub fn date(dt: &DateTime<Local>) -> String {
    match lang() {
        Lang::De => dt.format("%d.%m.%Y").to_string(),
        Lang::En => dt.format("%b %-d, %Y").to_string(),
    }
}

/// "27.10.2026, 19:54" / "Oct 27, 2026, 19:54".
pub fn date_time(dt: &DateTime<Local>) -> String {
    format!("{}, {}", date(dt), dt.format("%H:%M"))
}

/// "27.10." / "Oct 27" — for compact lists.
pub fn short_date(dt: &DateTime<Local>) -> String {
    match lang() {
        Lang::De => dt.format("%d.%m.").to_string(),
        Lang::En => dt.format("%b %-d").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_and_fills() {
        set_lang(Lang::En);
        assert_eq!(tr("__nicht übersetzt__"), "__nicht übersetzt__");
        assert_eq!(trf("{} von {}", &[&1, &2]), "1 von 2");
        set_lang(Lang::De);
        assert_eq!(trf("{} von {}", &[&1, &2]), "1 von 2");
    }

    /// Every German string literal in the workspace's sources must have an
    /// English translation, so nothing German shows up with the UI in
    /// English. Lines marked `// i18n: not UI` are exempt (stored labels,
    /// hallucination filters, stop words), as are tests and search keywords.
    #[test]
    fn every_german_string_has_english() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut files = Vec::new();
        let mut dirs = vec![root];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|n| n != "target" && n != "i18n") {
                        dirs.push(path);
                    }
                } else if path.extension().is_some_and(|e| e == "rs") && path.components().any(|c| c.as_os_str() == "src") {
                    files.push(path);
                }
            }
        }
        assert!(files.len() > 20, "found only {} source files", files.len());
        let table = english();
        let mut missing = Vec::new();
        for file in files {
            let source = std::fs::read_to_string(&file).unwrap();
            // Tests come last in a file; their literals are fixtures.
            let source = source.split("#[cfg(test)]").next().unwrap_or_default();
            let mut exempt_list = false;
            let mut after_tr = false;
            for (n, line) in source.lines().enumerate() {
                let code = line.trim_start();
                if exempt_list {
                    exempt_list = !code.starts_with("];");
                    continue;
                }
                if line.contains("i18n: not UI") {
                    // A marked list (`… = &[ // i18n: not UI`) is exempt up to its `];`.
                    exempt_list = line.contains("&[ //");
                    continue;
                }
                if code.starts_with("//") || code.starts_with("keywords:") {
                    continue;
                }
                let literals = string_literals(line);
                // Literals passed to tr()/trf() must be keys, German-looking or not.
                let mut keys: Vec<&String> = Vec::new();
                for call in ["tr(", "trf("] {
                    let mut rest = line;
                    while let Some(at) = rest.find(&format!("{call}\"")) {
                        let args = &rest[at + call.len()..];
                        // `tr(` itself, not `push_str(` or `mime_str(`.
                        let own_call = !rest[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_');
                        if let Some(lit) = string_literals(args).into_iter().next().filter(|_| own_call) {
                            keys.extend(literals.iter().find(|l| **l == lit));
                        }
                        rest = &args[1..];
                    }
                }
                if after_tr {
                    keys.extend(literals.first());
                }
                after_tr = ["tr(", "trf("].iter().any(|call| {
                    code.strip_suffix(call).is_some_and(|before| !before.ends_with(|c: char| c.is_alphanumeric() || c == '_'))
                });
                for literal in &literals {
                    let must = keys.contains(&literal) || looks_german(literal);
                    if must && !table.contains_key(literal.as_str()) {
                        missing.push(format!("{}:{}: {literal:?}", file.display(), n + 1));
                    }
                }
            }
        }
        assert!(missing.is_empty(), "German strings without English translation:\n{}", missing.join("\n"));
    }

    /// The contents of the plain "…" literals on a line (escapes decoded for \" \\ \n).
    fn string_literals(line: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut chars = line.chars().peekable();
        let mut prev = ' ';
        while let Some(c) = chars.next() {
            // Skip char literals like '"' and the rest after a line comment.
            if c == '\'' && chars.peek() == Some(&'"') {
                chars.next();
                prev = '"';
                continue;
            }
            if c == '/' && chars.peek() == Some(&'/') && prev != ':' {
                break;
            }
            if c == '"' {
                let mut lit = String::new();
                while let Some(d) = chars.next() {
                    match d {
                        '\\' => match chars.next() {
                            Some('n') => lit.push('\n'),
                            Some(e) => lit.push(e),
                            None => {}
                        },
                        '"' => break,
                        _ => lit.push(d),
                    }
                }
                out.push(lit);
            }
            prev = c;
        }
        out
    }

    fn looks_german(s: &str) -> bool {
        const WORDS: &[&str] = &[
            "nicht", "und", "der", "die", "das", "mit", "ist", "kein", "keine", "wird", "werden", "oder", "auf", "bei", "von",
            "nur", "noch", "ein", "eine", "Fehler", "Besprechung", "Aufnahme", "Sprecher", "Zusammenfassung", "Einstellungen",
            "Verbindung", "Schlüssel", "Datei", "Speichern", "Abbrechen", "Löschen", "Neue", "Lokal", "Fragen", "Transkript",
        ];
        s.chars().any(|c| "äöüÄÖÜß".contains(c)) || s.split(|c: char| !c.is_alphabetic()).any(|w| WORDS.contains(&w))
    }
}
