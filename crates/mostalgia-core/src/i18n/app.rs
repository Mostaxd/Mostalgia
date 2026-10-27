//! English for the app shell: sidebar, ask view, tray, shared helpers.

pub const EN: &[(&str, &str)] = &[
    ("mostalgia beenden", "Quit mostalgia"),
    // Sidebar and title bar
    ("Neue Aufnahme", "New recording"),
    ("Aufnahme", "Recording"),
    ("Aufnahme läuft", "Recording"),
    ("Fragen", "Ask"),
    ("Frag deine Besprechungen (Strg+K)", "Ask your meetings (Ctrl+K)"),
    ("Transkripte durchsuchen", "Search transcripts"),
    ("HEUTE", "TODAY"),
    ("GESTERN", "YESTERDAY"),
    ("DIESE WOCHE", "THIS WEEK"),
    ("DIESER MONAT", "THIS MONTH"),
    ("FRÜHER", "EARLIER"),
    ("{} TREFFER", "{} RESULTS"),
    ("Noch keine Besprechungen. Starte deine erste Aufnahme.", "No meetings yet. Start your first recording."),
    ("Einstellungen", "Settings"),
    // Call banner
    ("Anruf in {} erkannt – aufnehmen?", "Call detected in {} – record it?"),
    ("Aufnahme starten", "Start recording"),
    ("Ignorieren", "Ignore"),
    // Ask view
    ("Frag deine Besprechungen", "Ask your meetings"),
    (
        "Die Suche läuft lokal über alle Transkripte und Zusammenfassungen. An das Sprachmodell gehen nur die gefundenen Abschnitte, nie Audio. Jede Aussage verweist auf die Stelle in der Aufnahme.",
        "Search runs locally over all transcripts and summaries. Only the passages it finds go to the language model, never audio. Every statement links to its moment in the recording.",
    ),
    ("Was wurde in der letzten Besprechung entschieden?", "What was decided in the last meeting?"),
    ("Welche Aufgaben sind noch offen und wer ist verantwortlich?", "Which tasks are still open, and who owns them?"),
    ("Was wurde diese Woche über das Budget gesagt?", "What was said about the budget this week?"),
    ("Fasse meine Besprechungen von heute zusammen.", "Summarize my meetings from today."),
    ("Frage zu deinen Besprechungen …", "Ask about your meetings …"),
    ("Durchsuche Besprechungen …", "Searching meetings …"),
    ("Neue Unterhaltung", "New conversation"),
    (
        "Kein {}-API-Schlüssel — in Einstellungen → KI & Zusammenfassung hinterlegen.",
        "No {} API key — add one in Settings → AI & summaries.",
    ),
    // Tray
    ("Aufnahme beenden", "Stop recording"),
    ("Fenster anzeigen", "Show window"),
    ("Beenden", "Quit"),
    // Shared helpers
    ("Mikrofon", "Microphone"),
    ("Desktop", "Desktop"),
    ("< 1 min", "< 1 min"),
    ("{} min", "{} min"),
];
