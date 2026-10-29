//! "Dokumente": the documents drive. Files dropped here or picked with
//! "Add" (Markdown, text, PDF, Word, OpenDocument, HTML) are copied into
//! the data directory and indexed in the background; answers in "Ask",
//! the companion's chat and her live answer suggestions can cite them.
//! The reader shows a document's text as it was indexed, with a cited
//! page or section highlighted. Documents can be grouped in collections
//! (say, one per project), each switched on or off for answers as a whole;
//! a document is moved by dragging it onto a collection or from the reader.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, StyledExt, h_flex, switch::Switch, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mostalgia_core::docs::extract::Section;
use mostalgia_core::docs::{self, Collection, DocHit, Document, Kind, Within};
use mostalgia_core::i18n::{self, tr, trf};

use crate::ui;

/// What the background worker reports.
enum Progress {
    /// A file was added (or was there already) and is being indexed.
    Added { id: i64, new: bool },
    /// Indexing a document finished (with an error message on failure).
    Indexed { id: i64, error: Option<String> },
    /// A file couldn't be added at all.
    Failed { file: String, error: String },
    /// The queue is empty.
    Idle,
}

enum Job {
    /// A file to add, into a collection if given.
    Add(PathBuf, Option<i64>),
    Reindex(i64),
}

/// A document being dragged onto a collection.
#[derive(Clone)]
struct DraggedDoc {
    id: i64,
    title: String,
}

impl Render for DraggedDoc {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .gap_1p5()
            .px_2()
            .py_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .text_sm()
            .shadow_md()
            .child(Icon::new(IconName::FileText).xsmall())
            .child(self.title.clone())
    }
}

/// Naming a collection: a new one (`target` `None`) or a rename.
struct NameEdit {
    target: Option<i64>,
    /// A new collection: put this document into it.
    assign: Option<i64>,
    input: Entity<InputState>,
    _subscription: Subscription,
}

pub enum DocumentsEvent {
    /// Ask questions about these documents (opens "Fragen").
    Ask(Within),
}

impl EventEmitter<DocumentsEvent> for DocumentsView {}

pub struct DocumentsView {
    docs: Vec<Document>,
    collections: Vec<Collection>,
    /// Folded groups in the list (`None`: documents in no collection).
    folded: HashSet<Option<i64>>,
    name_edit: Option<NameEdit>,
    selected: Option<i64>,
    /// A collection picked in the list (instead of a document).
    selected_collection: Option<i64>,
    /// The selected document's text (`None` while it is read), and the
    /// location to highlight.
    sections: Option<Vec<Section>>,
    /// Documents' text read before (until they are read in again).
    section_cache: HashMap<i64, Vec<Section>>,
    highlight: Option<u32>,
    /// Scroll the reader to the highlighted location at the next render.
    scroll_to_highlight: bool,
    reader_scroll: ScrollHandle,
    search: Entity<InputState>,
    query: String,
    hits: Vec<DocHit>,
    /// Documents being indexed now, and files waiting.
    busy: HashSet<i64>,
    pending: usize,
    jobs: Option<Sender<Job>>,
    progress: Option<Receiver<Progress>>,
    /// Problems with the last files added (shown until dismissed).
    errors: Vec<String>,
    /// Notices like "already in the drive".
    notices: Vec<String>,
    /// Delete was clicked once (a second click deletes).
    confirm_delete: Option<i64>,
    lang_seen: Option<i18n::Lang>,
    _subscriptions: Vec<Subscription>,
}

impl DocumentsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Dokumente durchsuchen")));
        let sub = cx.subscribe_in(&search, window, |this, input, event: &InputEvent, _, cx| {
            if let InputEvent::Change = event {
                this.query = input.read(cx).value().to_string();
                this.run_search(cx);
            }
        });
        let mut this = Self {
            docs: Vec::new(),
            collections: Vec::new(),
            folded: HashSet::new(),
            name_edit: None,
            selected: None,
            selected_collection: None,
            sections: None,
            section_cache: HashMap::new(),
            highlight: None,
            scroll_to_highlight: false,
            reader_scroll: ScrollHandle::new(),
            search,
            query: String::new(),
            hits: Vec::new(),
            busy: HashSet::new(),
            pending: 0,
            jobs: None,
            progress: None,
            errors: Vec::new(),
            notices: Vec::new(),
            confirm_delete: None,
            lang_seen: None,
            _subscriptions: vec![sub],
        };
        this.reload(cx);
        this
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        match docs::list().and_then(|d| Ok((d, docs::collections()?))) {
            Ok((list, collections)) => (self.docs, self.collections) = (list, collections),
            Err(e) => self.errors.push(format!("{e:#}")),
        }
        if self.selected.is_some_and(|id| !self.docs.iter().any(|d| d.id == id)) {
            self.selected = None;
            self.sections = None;
        }
        if self.selected_collection.is_some_and(|id| !self.collections.iter().any(|k| k.id == id)) {
            self.selected_collection = None;
        }
        cx.notify();
    }

    /// Opens a document in the reader, at a page or section if given.
    pub fn open(&mut self, id: i64, loc: Option<u32>, cx: &mut Context<Self>) {
        let same = self.selected == Some(id) && self.sections.is_some();
        self.selected = Some(id);
        self.selected_collection = None;
        self.highlight = loc;
        self.scroll_to_highlight = loc.is_some();
        self.confirm_delete = None;
        if loc.is_none() && !same {
            self.reader_scroll.set_offset(point(px(0.), px(0.)));
        }
        if let Some(sections) = self.section_cache.get(&id) {
            self.sections = Some(sections.clone());
        } else if !same {
            // Read from the file again (a big PDF takes a moment).
            self.sections = None;
            cx.spawn(async move |this, cx| {
                let sections = cx.background_spawn(async move { docs::sections(id).unwrap_or_default() }).await;
                this.update(cx, |this, cx| {
                    this.section_cache.insert(id, sections.clone());
                    if this.selected == Some(id) {
                        this.sections = Some(sections);
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    /// Shows a collection (its summary and "ask") instead of a document.
    fn select_collection(&mut self, id: i64, cx: &mut Context<Self>) {
        self.selected_collection = Some(id);
        self.selected = None;
        self.sections = None;
        self.confirm_delete = None;
        cx.notify();
    }

    /// What "Fragen" offers first: the picked collection, or the selected
    /// document's collection and then the document.
    pub fn ask_offers(&self) -> Vec<Within> {
        if let Some(k) = self.selected_collection {
            return vec![Within::Collection(k)];
        }
        let Some(d) = self.selected.and_then(|id| self.docs.iter().find(|d| d.id == id)) else { return Vec::new() };
        let collection = d.collection_id.filter(|k| self.collections.iter().any(|c| c.id == *k));
        collection.map(Within::Collection).into_iter().chain([Within::Document(d.id)]).collect()
    }

    fn run_search(&mut self, cx: &mut Context<Self>) {
        let q = self.query.trim();
        self.hits = if q.is_empty() { Vec::new() } else { docs::search_drive(q, 20).unwrap_or_default() };
        cx.notify();
    }

    /// The file picker; the files go into `collection` if given.
    fn pick_files(&mut self, collection: Option<i64>, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some(tr("Hinzufügen").into()) });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                this.update(cx, |this, cx| this.add_paths(paths, collection, cx)).ok();
            }
        })
        .detach();
    }

    /// Queues files (and the supported files in dropped folders), to go
    /// into `collection` if given.
    pub fn add_paths(&mut self, paths: Vec<PathBuf>, collection: Option<i64>, cx: &mut Context<Self>) {
        let mut files = Vec::new();
        let mut skipped = Vec::new();
        let mut stack = paths;
        while let Some(p) = stack.pop() {
            if p.is_dir() {
                if let Ok(entries) = std::fs::read_dir(&p) {
                    stack.extend(entries.flatten().map(|e| e.path()).filter(|p| p.is_dir() || Kind::from_path(p).is_some()));
                }
            } else if Kind::from_path(&p).is_some() {
                files.push(p);
            } else {
                skipped.push(p.file_name().map_or(p.display().to_string(), |n| n.to_string_lossy().to_string()));
            }
        }
        if !skipped.is_empty() {
            self.errors.push(trf("Nicht unterstützt (übersprungen): {}", &[&skipped.join(", ")]));
        }
        files.sort();
        for f in files {
            self.queue(Job::Add(f, collection), cx);
        }
        cx.notify();
    }

    fn reindex(&mut self, id: i64, cx: &mut Context<Self>) {
        self.busy.insert(id);
        self.queue(Job::Reindex(id), cx);
    }

    /// Hands a job to the worker thread (started on first use; it ends
    /// when the queue runs dry).
    fn queue(&mut self, job: Job, cx: &mut Context<Self>) {
        self.pending += 1;
        let job = match &self.jobs {
            Some(tx) => match tx.send(job) {
                Ok(()) => return,
                Err(crossbeam_channel::SendError(job)) => job,
            },
            None => job,
        };
        let (jobs_tx, jobs_rx) = crossbeam_channel::unbounded::<Job>();
        let (tx, rx) = crossbeam_channel::unbounded();
        let _ = jobs_tx.send(job);
        std::thread::spawn(move || {
            while let Ok(job) = jobs_rx.recv_timeout(Duration::from_millis(300)) {
                let id = match job {
                    Job::Add(path, collection) => match docs::add(&path) {
                        Ok((id, new)) => {
                            if collection.is_some() {
                                let _ = docs::set_collection(id, collection);
                            }
                            let _ = tx.send(Progress::Added { id, new });
                            if !new {
                                continue;
                            }
                            id
                        }
                        Err(e) => {
                            let file = path.file_name().map_or(path.display().to_string(), |n| n.to_string_lossy().to_string());
                            let _ = tx.send(Progress::Failed { file, error: format!("{e:#}") });
                            continue;
                        }
                    },
                    Job::Reindex(id) => id,
                };
                let error = docs::index(id).err().map(|e| format!("{e:#}"));
                let _ = tx.send(Progress::Indexed { id, error });
            }
            let _ = tx.send(Progress::Idle);
        });
        self.jobs = Some(jobs_tx);
        self.progress = Some(rx);
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(120)).await;
                let keep = this.update(cx, |this, cx| this.poll(cx)).unwrap_or(false);
                if !keep {
                    break;
                }
            }
        })
        .detach();
    }

    /// Takes the worker's progress; returns whether to keep polling.
    fn poll(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(rx) = &self.progress else { return false };
        let received: Vec<Progress> = rx.try_iter().collect();
        let mut changed = false;
        let mut idle = false;
        for p in received {
            changed = true;
            match p {
                Progress::Added { id, new } => {
                    if new {
                        self.busy.insert(id);
                    } else {
                        self.pending = self.pending.saturating_sub(1);
                        let title = docs::get(id).ok().flatten().map_or(String::new(), |d| d.title);
                        self.notices.push(trf("„{}“ ist schon in deinen Dokumenten", &[&title]));
                    }
                    if self.selected.is_none() {
                        self.selected = Some(id);
                    }
                }
                Progress::Indexed { id, error } => {
                    self.pending = self.pending.saturating_sub(1);
                    self.busy.remove(&id);
                    if let Some(e) = error {
                        let title = docs::get(id).ok().flatten().map_or(String::new(), |d| d.title);
                        self.errors.push(format!("{title}: {e}"));
                    }
                    self.section_cache.remove(&id);
                    if self.selected == Some(id) {
                        let loc = self.highlight;
                        self.sections = None;
                        self.open(id, loc, cx);
                    }
                }
                Progress::Failed { file, error } => {
                    self.pending = self.pending.saturating_sub(1);
                    self.errors.push(format!("{file}: {error}"));
                }
                Progress::Idle => idle = true,
            }
        }
        if changed {
            self.reload(cx);
            if !self.query.trim().is_empty() {
                self.run_search(cx);
            }
        }
        if idle {
            self.jobs = None;
            self.progress = None;
            self.busy.clear();
            self.pending = 0;
            cx.notify();
            return false;
        }
        true
    }

    /// Switches a document on or off for answers.
    fn set_active(&mut self, id: i64, active: bool, cx: &mut Context<Self>) {
        if let Err(e) = docs::set_active(id, active) {
            self.errors.push(format!("{e:#}"));
        }
        self.reload(cx);
    }

    /// Switches a whole collection on or off for answers.
    fn set_collection_active(&mut self, id: i64, active: bool, cx: &mut Context<Self>) {
        if let Err(e) = docs::set_collection_active(id, active) {
            self.errors.push(format!("{e:#}"));
        }
        self.reload(cx);
    }

    /// Moves a document into a collection (`None`: out of any).
    fn move_doc(&mut self, id: i64, collection: Option<i64>, cx: &mut Context<Self>) {
        if let Err(e) = docs::set_collection(id, collection) {
            self.errors.push(format!("{e:#}"));
        }
        self.folded.remove(&collection);
        self.reload(cx);
    }

    fn delete_collection(&mut self, id: i64, cx: &mut Context<Self>) {
        if let Err(e) = docs::delete_collection(id) {
            self.errors.push(format!("{e:#}"));
        }
        self.folded.remove(&Some(id));
        self.reload(cx);
    }

    /// Starts naming a new collection (`target` `None`, then `assign` goes
    /// into it) or renaming one, in place in the list.
    fn begin_name_edit(&mut self, target: Option<i64>, assign: Option<i64>, window: &mut Window, cx: &mut Context<Self>) {
        let name = target.and_then(|id| self.collections.iter().find(|k| k.id == id)).map_or(String::new(), |k| k.name.clone());
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(tr("Name der Sammlung")));
        input.update(cx, |s, cx| s.set_value(name, window, cx));
        input.read(cx).focus_handle(cx).focus(window, cx);
        let sub = cx.subscribe_in(&input, window, |this, input, event: &InputEvent, _, cx| match event {
            InputEvent::PressEnter { .. } => {
                let name = input.read(cx).value().trim().to_string();
                this.commit_name(&name, cx);
            }
            InputEvent::Blur => {
                this.name_edit = None;
                cx.notify();
            }
            _ => {}
        });
        self.name_edit = Some(NameEdit { target, assign, input, _subscription: sub });
        cx.notify();
    }

    fn commit_name(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(edit) = self.name_edit.take() else { return };
        if !name.is_empty() {
            let done = match edit.target {
                Some(id) => docs::rename_collection(id, name),
                None => docs::create_collection(name).and_then(|k| {
                    self.folded.remove(&Some(k));
                    edit.assign.map_or(Ok(()), |doc| docs::set_collection(doc, Some(k)))
                }),
            };
            if let Err(e) = done {
                self.errors.push(format!("{e:#}"));
            }
        }
        self.reload(cx);
    }

    /// The reader's "in collection" button: moves the document.
    fn collection_menu(&self, doc: &Document, cx: &mut Context<Self>) -> impl IntoElement {
        let current = doc.collection_id.and_then(|id| self.collections.iter().find(|k| k.id == id));
        let label = current.map_or_else(|| tr("Keine Sammlung").to_string(), |k| k.name.clone());
        let choices: Vec<(Option<i64>, String)> =
            self.collections.iter().map(|k| (Some(k.id), k.name.clone())).chain([(None, tr("Keine Sammlung").to_string())]).collect();
        let (id, now) = (doc.id, current.map(|k| k.id));
        let weak = cx.entity().downgrade();
        Button::new("doc-collection")
            .small()
            .ghost()
            .icon(IconName::FolderInput)
            .label(label)
            .tooltip(tr("In eine Sammlung verschieben"))
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.scrollable(true).max_h(px(360.));
                for (k, name) in &choices {
                    let (k, weak) = (*k, weak.clone());
                    menu = menu.item(PopupMenuItem::new(name.clone()).checked(k == now).on_click(move |_, _, cx| {
                        weak.update(cx, |this, cx| this.move_doc(id, k, cx)).ok();
                    }));
                }
                let weak = weak.clone();
                menu.separator().item(PopupMenuItem::new(tr("Neue Sammlung…")).icon(IconName::FolderPlus).on_click(move |_, window, cx| {
                    weak.update(cx, |this, cx| this.begin_name_edit(None, Some(id), window, cx)).ok();
                }))
            })
    }

    fn active_switch(&self, key: &str, d: &Document, cx: &mut Context<Self>) -> Switch {
        let id = d.id;
        Switch::new(SharedString::from(format!("{key}-{id}")))
            .small()
            .checked(d.active)
            .tooltip(if d.active { tr("Wird für Antworten genutzt – klicken zum Ausschalten") } else { tr("Ausgeschaltet – Antworten nutzen dieses Dokument nicht") })
            .on_change(cx.listener(move |this, on: &bool, _, cx| this.set_active(id, *on, cx)))
    }

    fn delete(&mut self, id: i64, cx: &mut Context<Self>) {
        if self.confirm_delete != Some(id) {
            self.confirm_delete = Some(id);
            cx.notify();
            return;
        }
        self.confirm_delete = None;
        if let Err(e) = docs::delete(id) {
            self.errors.push(format!("{e:#}"));
        }
        if self.selected == Some(id) {
            self.selected = None;
            self.sections = None;
        }
        self.reload(cx);
        self.run_search(cx);
    }

    fn kind_badge(kind: Kind, cx: &App) -> Div {
        let theme = cx.theme();
        let color = match kind {
            Kind::Pdf => theme.danger,
            Kind::Docx | Kind::Odt => theme.info,
            Kind::Markdown => theme.success,
            Kind::Html => theme.warning,
            Kind::Text => theme.muted_foreground,
        };
        div()
            .flex_none()
            .w(px(38.))
            .py_0p5()
            .rounded(theme.radius)
            .bg(color.opacity(0.14))
            .text_color(color)
            .text_size(px(10.))
            .font_semibold()
            .text_center()
            .child(kind.id().to_uppercase())
    }

    /// "PDF · 12 pages · English · 34 chunks" or the state while indexing.
    fn meta(&self, d: &Document) -> String {
        if self.busy.contains(&d.id) || d.indexed_at.is_none() && d.error.is_none() {
            return tr("Wird gelesen …").to_string();
        }
        let mut parts = Vec::new();
        if let Some(p) = d.pages {
            parts.push(if p == 1 { tr("1 Seite").to_string() } else { trf("{} Seiten", &[&p]) });
        }
        if !d.language.is_empty() {
            parts.push(d.language.to_uppercase());
        }
        parts.push(if d.chunks == 1 { tr("1 Abschnitt").to_string() } else { trf("{} Abschnitte", &[&d.chunks]) });
        parts.push(size(d.size_bytes));
        if d.chunks > 0 && !d.embedded {
            // Without the embedding model only keyword search finds it.
            parts.push(tr("nur Stichwortsuche").to_string());
        }
        parts.join(" · ")
    }

    fn render_list(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let mut items: Vec<AnyElement> = Vec::new();
        if !self.query.trim().is_empty() {
            items.push(div().px_2().pt_1().text_xs().font_semibold().text_color(theme.muted_foreground).child(trf("{} TREFFER", &[&self.hits.len()])).into_any_element());
            for (i, h) in self.hits.iter().enumerate() {
                let (id, loc) = (h.document_id, h.loc);
                let heading = if h.heading.is_empty() { String::new() } else { format!(" · {}", h.heading) };
                let snippet: String = h.text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(180).collect();
                items.push(
                    v_flex()
                        .id(("doc-hit", i))
                        .px_2()
                        .py_1p5()
                        .gap_0p5()
                        .rounded(theme.radius)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.muted))
                        .when(self.selected == Some(id) && self.highlight == Some(loc), |el| el.bg(theme.accent))
                        .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(format!("{} · {}{heading}", h.title, docs::loc_label(h.kind, h.loc))))
                        .child(ui::bidi_text(snippet, true).text_sm().line_height(relative(1.35)))
                        .on_click(cx.listener(move |this, _, _, cx| this.open(id, Some(loc), cx)))
                        .into_any_element(),
                );
            }
            return v_flex().gap_0p5().children(items).into_any_element();
        }
        // Grouped by collection (by name), then the documents in none.
        let collections = self.collections.clone();
        let known: HashSet<i64> = collections.iter().map(|k| k.id).collect();
        if self.name_edit.as_ref().is_some_and(|e| e.target.is_none()) {
            items.push(self.name_row(cx));
        }
        for k in &collections {
            let docs: Vec<&Document> = self.docs.iter().filter(|d| d.collection_id == Some(k.id)).collect();
            items.push(self.render_group(Some(k), &docs, cx));
        }
        let loose: Vec<&Document> = self.docs.iter().filter(|d| d.collection_id.is_none_or(|k| !known.contains(&k))).collect();
        if collections.is_empty() {
            items.extend(loose.iter().map(|d| self.doc_row(d, cx)));
        } else if !loose.is_empty() {
            items.push(self.render_group(None, &loose, cx));
        }
        v_flex().gap_0p5().children(items).into_any_element()
    }

    /// The row where a new collection is named (or one renamed).
    fn name_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(edit) = &self.name_edit else { return div().into_any_element() };
        h_flex()
            .flex_1()
            .min_w_0()
            .gap_2()
            .when(edit.target.is_none(), |el| el.px_2().py_1().child(Icon::new(IconName::FolderPlus).small().text_color(theme.primary)))
            .child(div().flex_1().child(Input::new(&edit.input).small()))
            .into_any_element()
    }

    /// A collection (`None`: the documents in none) with its documents;
    /// documents and files can be dropped on it.
    fn render_group(&self, k: Option<&Collection>, docs: &[&Document], cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let key = k.map(|k| k.id);
        let folded = self.folded.contains(&key);
        let renaming = k.is_some() && self.name_edit.as_ref().is_some_and(|e| e.target == key);
        let on = k.is_none_or(|k| k.active);
        let name = k.map_or_else(|| tr("Ohne Sammlung").to_string(), |k| k.name.clone());
        let count = if docs.len() == 1 { tr("1 Dokument").to_string() } else { trf("{} Dokumente", &[&docs.len()]) };
        let gid = key.map_or(0, |id| id as usize + 1);
        let header = h_flex()
            .id(("doc-group", gid))
            .px_2()
            .py_1()
            .gap_1p5()
            .rounded(theme.radius)
            .cursor_pointer()
            .hover(|s| s.bg(theme.muted))
            .when(k.is_some() && self.selected_collection == key, |el| el.bg(theme.accent))
            .child(
                div()
                    .id(("doc-group-fold", gid))
                    .p_0p5()
                    .rounded(theme.radius)
                    .hover(|s| s.bg(theme.muted))
                    .child(Icon::new(if folded { IconName::ChevronRight } else { IconName::ChevronDown }).xsmall().text_color(theme.muted_foreground))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        if !this.folded.remove(&key) {
                            this.folded.insert(key);
                        }
                        cx.notify();
                    })),
            )
            .child(Icon::new(if k.is_some() { IconName::Folder } else { IconName::FileText }).small().text_color(if on && k.is_some() { theme.primary } else { theme.muted_foreground }))
            .when(renaming, |el| el.child(self.name_row(cx)))
            .when(!renaming, |el| {
                el.child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1p5()
                        .when(!on, |el| el.opacity(0.55))
                        .child(div().text_sm().font_semibold().truncate().child(name.clone()))
                        .child(div().flex_none().text_xs().text_color(theme.muted_foreground).child(count)),
                )
            })
            .when_some(k.filter(|_| !renaming), |el, k| {
                let id = k.id;
                let weak = cx.entity().downgrade();
                el.child(
                    Button::new(("doc-collection-menu", id as usize)).xsmall().ghost().icon(IconName::Ellipsis).dropdown_menu(move |menu, _, _| {
                        let (rename, add, delete) = (weak.clone(), weak.clone(), weak.clone());
                        let ask = weak.clone();
                        menu.item(PopupMenuItem::new(tr("Fragen zu dieser Sammlung")).icon(IconName::Sparkles).on_click(move |_, _, cx| {
                            ask.update(cx, |_, cx| cx.emit(DocumentsEvent::Ask(Within::Collection(id)))).ok();
                        }))
                        .item(PopupMenuItem::new(tr("Umbenennen")).icon(IconName::Pencil).on_click(move |_, window, cx| {
                            rename.update(cx, |this, cx| this.begin_name_edit(Some(id), None, window, cx)).ok();
                        }))
                        .item(PopupMenuItem::new(tr("Dateien hinzufügen…")).icon(IconName::Upload).on_click(move |_, _, cx| {
                            add.update(cx, |this, cx| this.pick_files(Some(id), cx)).ok();
                        }))
                        .separator()
                        .item(PopupMenuItem::new(tr("Sammlung auflösen (Dokumente bleiben)")).icon(IconName::Trash).on_click(move |_, _, cx| {
                            delete.update(cx, |this, cx| this.delete_collection(id, cx)).ok();
                        }))
                    }),
                )
                .child(
                    Switch::new(("doc-collection-active", id as usize))
                        .small()
                        .checked(k.active)
                        .tooltip(if k.active {
                            tr("Diese Sammlung wird für Antworten genutzt – klicken zum Ausschalten")
                        } else {
                            tr("Ausgeschaltet – Antworten nutzen keine Dokumente dieser Sammlung")
                        })
                        .on_change(cx.listener(move |this, on: &bool, _, cx| this.set_collection_active(id, *on, cx))),
                )
            })
            .on_click(cx.listener(move |this, _, _, cx| match key {
                // A collection: show it (and "ask about it").
                Some(id) => this.select_collection(id, cx),
                None => {
                    if !this.folded.remove(&key) {
                        this.folded.insert(key);
                    }
                    cx.notify();
                }
            }));
        v_flex()
            .id(("doc-group-drop", gid))
            .gap_0p5()
            .rounded(theme.radius)
            .drag_over::<DraggedDoc>(|style, _, _, cx| style.bg(cx.theme().primary.opacity(0.1)))
            .drag_over::<ExternalPaths>(|style, _, _, cx| style.bg(cx.theme().primary.opacity(0.1)))
            .on_drop(cx.listener(move |this, d: &DraggedDoc, _, cx| this.move_doc(d.id, key, cx)))
            .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| this.add_paths(paths.paths().to_vec(), key, cx)))
            .child(header)
            .when(!folded, |el| {
                el.when(docs.is_empty(), |el| {
                    el.child(div().pl(px(30.)).py_1().text_xs().text_color(theme.muted_foreground).child(tr("Leer – Dokumente hierher ziehen")))
                })
                .children(docs.iter().map(|d| div().pl_3().child(self.doc_row(d, cx))))
            })
            .into_any_element()
    }

    /// A document in the list.
    fn doc_row(&self, d: &Document, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let id = d.id;
        let failed = d.error.is_some() && !self.busy.contains(&id);
        let meta = if failed { d.error.clone().unwrap_or_default() } else { self.meta(d) };
        let busy = self.busy.contains(&id) || d.indexed_at.is_none() && d.error.is_none();
        let meta = match () {
            _ if failed || d.used => meta,
            _ if !d.active => format!("{} · {meta}", tr("Aus")),
            _ => format!("{} · {meta}", tr("Sammlung aus")),
        };
        let switch = self.active_switch("doc-active", d, cx);
        h_flex()
            .id(("doc", id as usize))
            .on_drag(DraggedDoc { id, title: d.title.clone() }, |d, _, _, cx| cx.new(|_| d.clone()))
            .px_2()
            .py_1p5()
            .gap_2()
            .items_start()
            .rounded(theme.radius)
            .cursor_pointer()
            .hover(|s| s.bg(theme.muted))
            .when(self.selected == Some(id), |el| el.bg(theme.accent))
            .child(Self::kind_badge(d.kind, cx).mt(px(2.)).when(!d.used, |el| el.opacity(0.5)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .when(!d.used, |el| el.opacity(0.55))
                    .child(div().text_sm().font_medium().truncate().child(d.title.clone()))
                    .child(
                            h_flex()
                            .gap_1()
                            .text_xs()
                            .text_color(if failed { theme.danger } else { theme.muted_foreground })
                            .when(busy, |el| el.child(gpui_kit::component::spinner::Spinner::new().xsmall()))
                            .child(div().truncate().child(meta)),
                    ),
            )
            .child(div().flex_none().mt(px(2.)).child(switch))
            .on_click(cx.listener(move |this, _, _, cx| this.open(id, None, cx)))
            .into_any_element()
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .px_8()
            .child(Icon::new(IconName::Library).large().text_color(theme.primary))
            .child(div().text_xl().font_semibold().child(tr("Deine Dokumente")))
            .child(
                div()
                    .max_w(px(520.))
                    .text_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(tr("Lege Handbücher, Spezifikationen, Notizen oder Berichte hier ab – Markdown, Text, PDF, Word, OpenDocument oder HTML, auf Deutsch, Englisch oder in anderen Sprachen. Sie werden lokal gelesen und durchsuchbar gemacht. Antworten in „Fragen“, im Chat und als Live-Antwortvorschläge können sie dann zitieren.")),
            )
            .child(Button::new("docs-add-empty").primary().icon(IconName::Upload).label(tr("Dateien hinzufügen")).on_click(cx.listener(|this, _, _, cx| this.pick_files(None, cx))))
            .child(div().text_xs().text_color(theme.muted_foreground).child(tr("…oder Dateien und Ordner hierher ziehen")))
            .into_any_element()
    }

    /// The reader's place when a collection is picked: what it holds and
    /// "ask about it".
    fn render_collection(&self, k: &Collection, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let id = k.id;
        let titles: Vec<String> = self.docs.iter().filter(|d| d.collection_id == Some(id)).map(|d| d.title.clone()).collect();
        let count = if titles.len() == 1 { tr("1 Dokument").to_string() } else { trf("{} Dokumente", &[&titles.len()]) };
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .px_8()
            .child(Icon::new(IconName::Folder).large().text_color(if k.active { theme.primary } else { theme.muted_foreground }))
            .child(div().text_xl().font_semibold().child(k.name.clone()))
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(format!("{count} · {}", if k.active { tr("Für Antworten") } else { tr("Sammlung aus") })),
            )
            .child(
                v_flex()
                    .max_w(px(420.))
                    .gap_0p5()
                    .text_sm()
                    .children(titles.into_iter().take(12).map(|t| h_flex().gap_1p5().child(Icon::new(IconName::FileText).xsmall().text_color(theme.muted_foreground)).child(div().truncate().child(t)))),
            )
            .child(
                Button::new("doc-collection-ask")
                    .primary()
                    .icon(IconName::Sparkles)
                    .label(tr("Fragen zu dieser Sammlung"))
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(DocumentsEvent::Ask(Within::Collection(id))))),
            )
            .into_any_element()
    }

    fn render_reader(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        if let Some(k) = self.selected_collection.and_then(|id| self.collections.iter().find(|k| k.id == id)) {
            return self.render_collection(&k.clone(), cx);
        }
        let Some(doc) = self.selected.and_then(|id| self.docs.iter().find(|d| d.id == id).cloned()) else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(Icon::new(IconName::BookOpen).large())
                .child(tr("Wähle ein Dokument, um es zu lesen."))
                .into_any_element();
        };
        let id = doc.id;
        let busy = self.busy.contains(&id);
        let confirming = self.confirm_delete == Some(id);
        let header = v_flex()
            .px_6()
            .py_3()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .child(Self::kind_badge(doc.kind, cx))
                    .child(div().flex_1().min_w_0().text_lg().font_semibold().truncate().child(doc.title.clone()))
                    .child(
                        h_flex()
                            .gap_1p5()
                            .mr_2()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(match () {
                                _ if doc.used => tr("Für Antworten"),
                                _ if !doc.active => tr("Aus"),
                                _ => tr("Sammlung aus"),
                            })
                            .child(self.active_switch("doc-active-reader", &doc, cx)),
                    )
                    .child(self.collection_menu(&doc, cx))
                    .child(
                        Button::new("doc-ask")
                            .small()
                            .ghost()
                            .icon(IconName::Sparkles)
                            .tooltip(tr("Fragen zu diesem Dokument"))
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(DocumentsEvent::Ask(Within::Document(id))))),
                    )
                    .child(
                        Button::new("doc-open-file")
                            .small()
                            .ghost()
                            .icon(IconName::ExternalLink)
                            .tooltip(tr("Mit der Standard-App öffnen"))
                            .on_click({
                                let path = doc.path.clone();
                                move |_, _, cx| cx.open_with_system(&path)
                            }),
                    )
                    .child(
                        Button::new("doc-reindex")
                            .small()
                            .ghost()
                            .icon(IconName::RefreshCw)
                            .loading(busy)
                            .tooltip(tr("Neu einlesen"))
                            .on_click(cx.listener(move |this, _, _, cx| this.reindex(id, cx))),
                    )
                    .child(
                        Button::new("doc-delete")
                            .small()
                            .when(confirming, |b| b.danger().label(tr("Wirklich entfernen?")))
                            .when(!confirming, |b| b.ghost())
                            .icon(IconName::Trash)
                            .tooltip(tr("Aus den Dokumenten entfernen (die Originaldatei bleibt)"))
                            .on_click(cx.listener(move |this, _, _, cx| this.delete(id, cx))),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("{} · {} · {}", doc.file_name, self.meta(&doc), trf("hinzugefügt {}", &[&i18n::date(&doc.added_at)]))),
            )
            .when_some(doc.error.clone().filter(|_| !busy), |el, e| el.child(div().text_xs().text_color(theme.danger).child(e)));

        // The text by page / section, the cited one highlighted; Markdown
        // rendered as such.
        let mut blocks: Vec<AnyElement> = Vec::new();
        let mut highlight_ix = None;
        let mut last_heading = String::new();
        let markdown = doc.kind == Kind::Markdown;
        let loading = self.sections.is_none();
        for (ix, sec) in self.sections.iter().flatten().enumerate() {
            let lit = self.highlight == Some(sec.loc);
            if lit && highlight_ix.is_none() {
                highlight_ix = Some(blocks.len());
            }
            let label = doc.kind.paged().then(|| docs::loc_label(doc.kind, sec.loc));
            // The heading path's last part, sized by its depth.
            let heading = (sec.heading != last_heading && !sec.heading.is_empty()).then(|| {
                let depth = sec.heading.split(" › ").count();
                let title = sec.heading.rsplit(" › ").next().unwrap_or_default().to_string();
                let el = div().font_semibold().pt_1().child(title);
                match depth {
                    1 => el.text_xl(),
                    2 => el.text_lg(),
                    _ => el.text_base(),
                }
            });
            last_heading = sec.heading.clone();
            let body = if markdown && !ui::is_rtl(&sec.text) {
                div().text_sm().child(TextView::markdown(("doc-md", ix), sec.text.clone()).selectable(true)).into_any_element()
            } else if markdown {
                ui::rtl_markdown(&sec.text, cx).text_sm().into_any_element()
            } else {
                v_flex()
                    .gap_2()
                    .children(sec.text.split("\n\n").filter(|p| !p.trim().is_empty()).map(|p| ui::bidi_text(p.replace('\n', " "), true).text_sm().line_height(relative(1.55))))
                    .into_any_element()
            };
            blocks.push(
                v_flex()
                    .gap_1()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius)
                    .when(lit, |el| el.bg(theme.warning.opacity(0.12)).border_l_2().border_color(theme.warning))
                    .when_some(label, |el, l| el.child(div().text_xs().font_semibold().text_color(theme.muted_foreground).child(l)))
                    .when_some(heading, |el, h| el.child(h))
                    .child(body)
                    .into_any_element(),
            );
        }
        if loading {
            blocks.push(h_flex().p_4().gap_2().text_sm().text_color(theme.muted_foreground).child(gpui_kit::component::spinner::Spinner::new().small()).child(tr("Wird gelesen …")).into_any_element());
        }
        if blocks.is_empty() && !busy && !loading {
            blocks.push(div().p_4().text_sm().text_color(theme.muted_foreground).child(tr("Kein Text gefunden.")).into_any_element());
        }
        if !loading
            && std::mem::take(&mut self.scroll_to_highlight)
            && let Some(ix) = highlight_ix
        {
            self.reader_scroll.scroll_to_top_of_item(ix);
        }
        v_flex()
            .size_full()
            .child(header)
            .child(
                div().flex_1().min_h_0().child(
                    // Each block stays a direct child (scroll_to_top_of_item),
                    // in a centered column.
                    v_flex()
                        .id("doc-reader")
                        .size_full()
                        .track_scroll(&self.reader_scroll)
                        .overflow_y_scroll()
                        .items_center()
                        .px_4()
                        .py_3()
                        .gap_1()
                        .children(blocks.into_iter().map(|b| div().w_full().max_w(px(860.)).child(b))),
                ),
            )
            .into_any_element()
    }
}

fn size(bytes: u64) -> String {
    match bytes {
        b if b >= 1024 * 1024 => format!("{:.1} MB", b as f64 / 1024. / 1024.),
        b if b >= 1024 => format!("{} KB", b / 1024),
        b => format!("{b} B"),
    }
}


impl Render for DocumentsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.lang_seen != Some(i18n::lang()) {
            self.lang_seen = Some(i18n::lang());
            self.search.update(cx, |s, cx| s.set_placeholder(tr("Dokumente durchsuchen"), window, cx));
        }
        let theme = cx.theme().clone();
        let empty = self.docs.is_empty() && self.pending == 0;
        let status = (self.pending > 0).then(|| trf("{} Datei(en) werden gelesen …", &[&self.pending]));
        let messages: Vec<(bool, String)> = self.errors.iter().map(|e| (true, e.clone())).chain(self.notices.iter().map(|n| (false, n.clone()))).collect();
        let body = if empty {
            self.render_empty(cx)
        } else {
            let list = self.render_list(cx);
            let reader = self.render_reader(cx);
            h_flex()
                .size_full()
                .child(
                    v_flex()
                        .w(px(340.))
                        .h_full()
                        .flex_none()
                        .border_r_1()
                        .border_color(theme.border)
                        .child(
                            h_flex()
                                .p_3()
                                .gap_1()
                                .child(div().flex_1().child(Input::new(&self.search).small().prefix(Icon::new(IconName::Search).small().text_color(theme.muted_foreground)).cleanable(true)))
                                .child(
                                    Button::new("docs-new-collection")
                                        .small()
                                        .ghost()
                                        .icon(IconName::FolderPlus)
                                        .tooltip(tr("Neue Sammlung"))
                                        .on_click(cx.listener(|this, _, window, cx| this.begin_name_edit(None, None, window, cx))),
                                ),
                        )
                        .child(div().flex_1().min_h_0().child(v_flex().id("doc-list").size_full().px_2().pb_2().overflow_y_scroll().child(list))),
                )
                .child(div().flex_1().min_w_0().h_full().child(reader))
                .into_any_element()
        };
        v_flex()
            .id("documents")
            .size_full()
            .drag_over::<ExternalPaths>(|style, _, _, cx| style.bg(cx.theme().primary.opacity(0.06)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.add_paths(paths.paths().to_vec(), None, cx)))
            .child(
                h_flex()
                    .px_8()
                    .py_4()
                    .gap_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().text_xl().font_semibold().child(tr("Dokumente")))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(tr("Lokal gespeichert · nur gefundene Abschnitte gehen an das Sprachmodell")))
                    .child(div().flex_1())
                    .when_some(status, |el, s| {
                        el.child(h_flex().gap_2().text_xs().text_color(theme.muted_foreground).child(gpui_kit::component::spinner::Spinner::new().xsmall()).child(s))
                    })
                    .when(!empty, |el| {
                        el.child(Button::new("docs-add").small().primary().icon(IconName::Upload).label(tr("Hinzufügen")).on_click(cx.listener(|this, _, _, cx| this.pick_files(None, cx))))
                    }),
            )
            .when(!messages.is_empty(), |el| {
                el.child(
                    h_flex()
                        .px_8()
                        .py_2()
                        .gap_2()
                        .items_start()
                        .border_b_1()
                        .border_color(theme.border)
                        .bg(theme.muted.opacity(0.5))
                        .child(v_flex().flex_1().gap_0p5().text_xs().children(messages.into_iter().map(|(err, m)| {
                            div().text_color(if err { theme.danger } else { theme.muted_foreground }).child(m)
                        })))
                        .child(Button::new("docs-messages-close").xsmall().ghost().icon(IconName::X).on_click(cx.listener(|this, _, _, cx| {
                            this.errors.clear();
                            this.notices.clear();
                            cx.notify();
                        }))),
                )
            })
            .child(div().flex_1().min_h_0().child(body))
    }
}
