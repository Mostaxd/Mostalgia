//! English for user-visible texts from mostalgia-core: speaker labels,
//! device names, export headings, summary and ask errors/statuses.

pub const EN: &[(&str, &str)] = &[
    // Speaker labels (stored in German, shown translated)
    ("Ich", "Me"),
    ("Andere", "Others"),
    ("Sprecher {}", "Speaker {}"),
    // Device names
    ("Laptop-Mikrofon", "Laptop microphone"),
    ("Headset-Mikrofon (Klinke)", "Headset microphone (jack)"),
    ("Laptop-Lautsprecher", "Laptop speakers"),
    ("Kopfhörer (Klinke)", "Headphones (jack)"),
    ("Grafikkarte HDMI{}", "Graphics card HDMI{}"),
    ("Browser-Anruf ({})", "Browser call ({})"),
    // Export
    ("Transkript", "Transcript"),
    ("Text", "Text"),
    ("Untertitel (SRT)", "Subtitles (SRT)"),
    // Summaries
    ("Der Anbieter hat keine Modelle geliefert", "The provider returned no models"),
    ("Die Besprechung hat kein Transkript zum Zusammenfassen", "The meeting has no transcript to summarize"),
    ("Das Modell hat eine leere Zusammenfassung geliefert", "The model returned an empty summary"),
    ("Claude hat die Zusammenfassung abgelehnt (refusal); nichts gespeichert", "Claude declined to summarize (refusal); nothing was saved"),
    ("Die Zusammenfassung wurde abgeschnitten (max_tokens)", "The summary was cut off (max_tokens)"),
    ("Die Zusammenfassung wurde abgeschnitten (length)", "The summary was cut off (length)"),
    ("DeepSeek hat die Zusammenfassung abgelehnt; nichts gespeichert", "DeepSeek declined to summarize; nothing was saved"),
    ("Schlüsselbund nicht erreichbar", "Could not open the system keyring"),
    ("API-Schlüssel konnte nicht aus dem Schlüsselbund gelesen werden", "Could not read the API key from the system keyring"),
    ("API-Schlüssel konnte nicht im Schlüsselbund gespeichert werden", "Could not save the API key to the system keyring"),
    // Provider errors
    ("Claude-API nicht erreichbar", "Could not reach the Claude API"),
    ("Claude-API-Fehler ({}): {}", "Claude API error ({}): {}"),
    ("Claude-API-Fehler: {}", "Claude API error: {}"),
    ("Antwort der Claude-API konnte nicht gelesen werden", "Could not read the Claude API response"),
    ("DeepSeek-API nicht erreichbar", "Could not reach the DeepSeek API"),
    ("DeepSeek-API-Fehler ({}): {}", "DeepSeek API error ({}): {}"),
    ("Antwort der DeepSeek-API konnte nicht gelesen werden", "Could not read the DeepSeek API response"),
    ("unbekannt", "unknown"),
    // Ask your meetings
    ("Suche: {} ({} Treffer, {} ms)", "Searching: {} ({} results, {} ms)"),
    ("Lese Transkript: {}", "Reading transcript: {}"),
    ("Besprechung nicht gefunden", "Meeting not found"),
    ("Besprechung #{}", "Meeting #{}"),
    ("unbekanntes Werkzeug {}", "unknown tool {}"),
    ("Claude hat die Frage abgelehnt", "Claude declined the question"),
    ("keine Antwort nach {} Suchschritten", "no answer after {} search steps"),
];
