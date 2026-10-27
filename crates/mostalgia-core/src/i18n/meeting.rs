//! English for the meeting view (German original → English).

pub const EN: &[(&str, &str)] = &[
    // Header
    ("Titel ändern", "Rename"),
    ("Speichern", "Save"),
    ("Abbrechen", "Cancel"),
    ("{} Sprecher", "{} speakers"),
    ("Exportieren", "Export"),
    ("Wiedergabe stoppen", "Stop playback"),
    ("Abspielen", "Play"),
    ("Wirklich löschen", "Delete for good"),
    ("Aus der Bibliothek entfernen (Audiodateien bleiben erhalten)", "Remove from the library (audio files are kept)"),
    ("Markdown", "Markdown"),
    ("Text", "Text"),
    ("Untertitel (SRT)", "Subtitles (SRT)"),
    ("Word (.docx)", "Word (.docx)"),
    // Tabs
    ("Zusammenfassung", "Summary"),
    ("Transkript · {}", "Transcript · {}"),
    ("Audio", "Audio"),
    // Summary
    ("Die Besprechung wird zusammengefasst … (bei hoher Denktiefe bis zu einigen Minuten)", "Summarizing the meeting … (up to a few minutes at high thinking levels)"),
    ("Erstellt von {} · {}{} am {}", "Created by {} · {}{} on {}"),
    (" · Denktiefe {}", " · thinking {}"),
    ("Neu erstellen", "Regenerate"),
    ("Noch keine Zusammenfassung", "No summary yet"),
    ("Kernpunkte, Entscheidungen und Aufgaben mit Verantwortlichen, erstellt vom gewählten Sprachmodell. Es wird nur der Text gesendet, nie Audio.", "Key points, decisions and action items with owners, written by the selected language model. Only the text is sent, never audio."),
    ("Zusammenfassung erstellen", "Create summary"),
    ("Kein {}-API-Schlüssel — in den Einstellungen hinterlegen.", "No {} API key — add one in Settings."),
    ("Zusammenfassung fehlgeschlagen: {}", "Summary failed: {}"),
    // Transcript
    ("In dieser Aufnahme wurde keine Sprache erkannt.", "No speech was detected in this recording."),
    ("„{}“ umbenennen in", "Rename “{}” to"),
    ("Sprecher umbenennen", "Rename speaker"),
    ("Text korrigieren", "Correct text"),
    // Audio
    ("Auf die Wellenform klicken, um ab dieser Stelle abzuspielen.", "Click the waveform to play from that point."),
    // Notices and errors
    ("Wiedergabe: {}", "Playback: {}"),
    ("Dateiauswahl: {}", "File picker: {}"),
    ("Exportiert: {}", "Exported: {}"),
    ("Besprechung nicht gefunden", "Meeting not found"),
    ("Besprechung nicht gefunden.", "Meeting not found."),
];
