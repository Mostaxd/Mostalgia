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
}
