//! English for the Microsoft Teams integration (core, CLI and app).

pub const EN: &[(&str, &str)] = &[
    // Core: sign-in
    ("Keine Client-ID für Microsoft eingetragen (Einstellungen → Microsoft Teams)", "No Microsoft client ID set (Settings → Microsoft Teams)"),
    ("Anmeldung abgebrochen", "Sign-in cancelled"),
    ("Microsoft hat die Anmeldung abgelehnt: {}", "Microsoft refused the sign-in: {}"),
    ("Die Antwort gehört nicht zu dieser Anmeldung", "The answer doesn't belong to this sign-in"),
    ("Die Anmeldung bei Microsoft ist abgelaufen. Bitte neu anmelden.", "The Microsoft sign-in has expired. Please sign in again."),
    ("Die Microsoft-Anmeldung konnte nicht im Schlüsselbund gespeichert werden", "Couldn't save the Microsoft sign-in in the keyring"),
    ("Nicht bei Microsoft angemeldet", "Not signed in to Microsoft"),
    ("Der Browser konnte nicht geöffnet werden", "Couldn't open the browser"),
    ("Die Anmeldung hat zu lange gedauert", "The sign-in took too long"),
    ("Angemeldet", "Signed in"),
    ("Anmeldung fehlgeschlagen", "Sign-in failed"),
    ("Du kannst dieses Fenster schließen und zu Mostalgia zurückkehren.", "You can close this window and go back to Mostalgia."),
    ("Microsoft hat kein Refresh-Token geschickt (fehlt offline_access?)", "Microsoft sent no refresh token (is offline_access missing?)"),
    // Core: Graph
    ("Microsoft ist nicht erreichbar", "Can't reach Microsoft"),
    ("Der Termin ist keine Teams-Besprechung", "The event isn't a Teams meeting"),
    ("Teams kennt diese Besprechung nicht (nur Geschäfts- oder Schulkonten)", "Teams doesn't know this meeting (work or school accounts only)"),
    ("Die Besprechung hat keinen Chat", "The meeting has no chat"),
    ("To Do hat die Liste nicht angelegt", "To Do didn't create the list"),
    ("Microsoft erlaubt das nicht ({}). Fehlt eine Berechtigung oder die Zustimmung des Admins?", "Microsoft doesn't allow this ({}). Is a permission or admin consent missing?"),
    ("Bei Microsoft nicht gefunden", "Not found at Microsoft"),
    ("Microsoft bremst gerade (zu viele Anfragen). Bitte gleich noch einmal versuchen.", "Microsoft is throttling (too many requests). Please try again in a moment."),
    ("Microsoft Graph antwortet mit Fehler {}", "Microsoft Graph answered with error {}"),
    // Core: calendar, chat message, brief
    ("Teams-Besprechung", "Teams meeting"),
    ("Geschrieben von Mostalgia", "Written by Mostalgia"),
    ("Teilnehmende", "Attendees"),
    ("Agenda", "Agenda"),
    ("Keine früheren Aufnahmen mit diesen Leuten.", "No earlier recordings with these people."),
    ("Letzte Male", "Last times"),
    ("Mit {}", "With {}"),
    ("Noch offen:", "Still open:"),
];
