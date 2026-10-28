//! English for the documents drive and the live answer helper.

pub const EN: &[(&str, &str)] = &[
    // Core: ingest and search
    ("S. {}", "p. {}"),
    ("Dateityp wird nicht unterstützt: {}", "File type not supported: {}"),
    ("{} konnte nicht gelesen werden", "Couldn't read {}"),
    ("{} ist zu groß (höchstens {} MB)", "{} is too large (at most {} MB)"),
    ("{} konnte nicht gespeichert werden", "Couldn't save {}"),
    ("Dokument nicht gefunden", "Document not found"),
    ("Keine Textinhalte gefunden", "No text found"),
    ("Der Name darf nicht leer sein", "The name can't be empty"),
    ("Das PDF enthält keinen Text (gescannt?) – OCR wird nicht unterstützt", "The PDF has no text (scanned?) – OCR isn't supported"),
    ("PDF konnte nicht gelesen werden: {}", "Couldn't read the PDF: {}"),
    ("PDF konnte nicht gelesen werden (beschädigt oder nicht unterstützt)", "Couldn't read the PDF (damaged or not supported)"),
    ("Datei ist kein gültiges Office-Dokument", "Not a valid Office document"),
    ("Dokumente: {} ({} Treffer, {} ms)", "Documents: {} ({} hits, {} ms)"),
    // Live answer sources
    ("Frühere Besprechungen", "Past meetings"),
    ("Dokumente", "Documents"),
    ("Besprechungen und Dokumente", "Meetings and documents"),
];
