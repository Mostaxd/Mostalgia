//! Exporting or importing the whole library (see `mostalgia_core::library`)
//! on a thread of its own: it keeps running when Settings is closed, the
//! Settings card shows how far it is, and the app redraws the library
//! when an import has added meetings. Quitting cancels it and waits.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use mostalgia_core::i18n::{tr, trf};
use mostalgia_core::library::{self, ExportOptions, ImportOptions, Places};
use mostalgia_core::paths;
use mostalgia_core::store::Store;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Exporting,
    Importing,
    /// After an import: the new meetings and documents are made searchable.
    Indexing,
}

/// The running (or last finished) job, as shown.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub phase: Phase,
    pub done: usize,
    pub total: usize,
    /// Set once it ended: what it did, or why it failed.
    pub outcome: Option<Result<String, String>>,
}

impl Snapshot {
    pub fn running(&self) -> bool {
        self.outcome.is_none()
    }

    pub fn fraction(&self) -> Option<f32> {
        (self.total > 0 && self.phase != Phase::Indexing).then(|| self.done as f32 / self.total as f32)
    }
}

struct Job {
    snapshot: Snapshot,
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

static JOB: Mutex<Option<Job>> = Mutex::new(None);
/// Bumped on every change, so views redraw only then.
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Bumped when an import added something (the library list reloads).
static IMPORTED: AtomicU64 = AtomicU64::new(0);

fn job() -> std::sync::MutexGuard<'static, Option<Job>> {
    JOB.lock().unwrap_or_else(|e| e.into_inner())
}

fn update(f: impl FnOnce(&mut Snapshot)) {
    if let Some(j) = job().as_mut() {
        f(&mut j.snapshot);
        GENERATION.fetch_add(1, Ordering::SeqCst);
    }
}

pub fn generation() -> u64 {
    GENERATION.load(Ordering::SeqCst)
}

pub fn imported() -> u64 {
    IMPORTED.load(Ordering::SeqCst)
}

pub fn current() -> Option<Snapshot> {
    job().as_ref().map(|j| j.snapshot.clone())
}

pub fn running() -> bool {
    current().is_some_and(|s| s.running())
}

/// Stops the running job (an export leaves no file; an import keeps the
/// meetings it finished).
pub fn cancel() {
    if let Some(j) = job().as_ref() {
        j.cancel.store(true, Ordering::Relaxed);
    }
}

/// Forgets a finished job's outcome.
pub fn dismiss() {
    let mut job = job();
    if job.as_ref().is_some_and(|j| !j.snapshot.running()) {
        *job = None;
        GENERATION.fetch_add(1, Ordering::SeqCst);
    }
}

/// On quit: cancels the job and waits for it (the embedding model must
/// not be torn down mid-run).
pub fn shutdown() {
    let thread = job().as_mut().and_then(|j| {
        j.cancel.store(true, Ordering::Relaxed);
        j.thread.take()
    });
    if let Some(t) = thread {
        let _ = t.join();
    }
}

fn start(phase: Phase, work: impl FnOnce(&AtomicBool) -> Result<String, String> + Send + 'static) {
    let mut slot = job();
    if slot.as_ref().is_some_and(|j| j.snapshot.running()) {
        return;
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    *slot = Some(Job { snapshot: Snapshot { phase, done: 0, total: 0, outcome: None }, cancel, thread: None });
    GENERATION.fetch_add(1, Ordering::SeqCst);
    let thread = std::thread::spawn(move || {
        let outcome = work(&flag);
        update(|s| s.outcome = Some(outcome));
    });
    if let Some(j) = slot.as_mut() {
        j.thread = Some(thread);
    }
}

fn progress(done: usize, total: usize) {
    update(|s| {
        s.done = done;
        s.total = total;
    });
}

pub fn start_export(dest: PathBuf, opts: ExportOptions) {
    start(Phase::Exporting, move |cancel| {
        let store = Store::open(&paths::database()).map_err(|e| format!("{e:#}"))?;
        let r = library::export(&store, &Places::data(), &dest, &opts, cancel, &progress).map_err(|e| format!("{e:#}"))?;
        let meetings = if r.meetings == 1 { tr("1 Besprechung").to_string() } else { trf("{} Besprechungen", &[&r.meetings]) };
        Ok(trf("{} gesichert in {} ({}).", &[&meetings, &dest.display(), &mostalgia_core::cleanup::human(r.bytes)]))
    });
}

pub fn start_import(src: PathBuf, opts: ImportOptions) {
    start(Phase::Importing, move |cancel| {
        let mut store = Store::open(&paths::database()).map_err(|e| format!("{e:#}"))?;
        let r = library::import(&mut store, &Places::data(), &src, &opts, cancel, &progress).map_err(|e| format!("{e:#}"))?;
        drop(store);
        if r.meetings_added + r.documents_added > 0 {
            IMPORTED.fetch_add(1, Ordering::SeqCst);
        }
        let mut message = trf("{} Besprechungen importiert, {} übersprungen.", &[&r.meetings_added, &r.meetings_skipped]);
        if r.audio_restored > 0 {
            message.push(' ');
            message.push_str(&trf("Audio für {} Besprechungen wiederhergestellt.", &[&r.audio_restored]));
        }
        if r.documents_added + r.documents_skipped > 0 {
            message.push(' ');
            message.push_str(&trf("{} Dokumente hinzugefügt.", &[&r.documents_added]));
        }
        if r.voices_merged > 0 {
            message.push(' ');
            message.push_str(&trf("{} Stimmen übernommen.", &[&r.voices_merged]));
        }
        if r.cancelled {
            message.push(' ');
            message.push_str(tr("Abgebrochen: der Rest wurde nicht importiert."));
            return Ok(message);
        }
        // Make what came in findable by "Ask" and in the documents.
        update(|s| s.phase = Phase::Indexing);
        if r.meetings_added > 0
            && let Err(e) = mostalgia_core::ask::index_pending_until(cancel)
        {
            eprintln!("[mostalgia] search index: {e:#}");
        }
        if r.documents_added > 0
            && let Err(e) = mostalgia_core::docs::index_pending(cancel)
        {
            eprintln!("[mostalgia] documents index: {e:#}");
        }
        Ok(message)
    });
}
