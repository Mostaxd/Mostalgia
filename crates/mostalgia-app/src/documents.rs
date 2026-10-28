//! "Dokumente": the documents drive. Files dropped here or picked with
//! "Add" (Markdown, text, PDF, Word, OpenDocument, HTML) are copied into
//! the data directory and indexed in the background; answers in "Ask",
//! the companion's chat and her live answer suggestions can cite them.
//! The reader shows a document's text as it was indexed, with a cited
//! page or section highlighted.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use mostalgia_core::docs::{self, Chunk, DocHit, Document, Kind};
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
    Add(PathBuf),
    Reindex(i64),
}

pub struct DocumentsView {
    docs: Vec<Document>,
    selected: Option<i64>,
    /// The selected document's text, and the location to highlight.
    chunks: Vec<Chunk>,
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
            selected: None,
            chunks: Vec::new(),
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

    fn reload(&mut self, cx: &mut Context<Self>) {
        match docs::list() {
            Ok(list) => self.docs = list,
            Err(e) => self.errors.push(format!("{e:#}")),
        }
        if self.selected.is_some_and(|id| !self.docs.iter().any(|d| d.id == id)) {
            self.selected = None;
            self.chunks.clear();
        }
        cx.notify();
    }

    /// Opens a document in the reader, at a page or section if given.
    pub fn open(&mut self, id: i64, loc: Option<u32>, cx: &mut Context<Self>) {
        self.selected = Some(id);
        self.highlight = loc;
        self.scroll_to_highlight = loc.is_some();
        self.confirm_delete = None;
        self.chunks = docs::chunks(id).unwrap_or_default();
        if loc.is_none() {
            self.reader_scroll.set_offset(point(px(0.), px(0.)));
        }
        cx.notify();
    }

    fn run_search(&mut self, cx: &mut Context<Self>) {
        let q = self.query.trim();
        self.hits = if q.is_empty() { Vec::new() } else { docs::search(q, None, 20).unwrap_or_default() };
        cx.notify();
    }

    /// The file picker.
    fn pick_files(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some(tr("Hinzufügen").into()) });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                this.update(cx, |this, cx| this.add_paths(paths, cx)).ok();
            }
        })
        .detach();
    }

    /// Queues files (and the supported files in dropped folders).
    pub fn add_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
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
            self.queue(Job::Add(f), cx);
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
                    Job::Add(path) => match docs::add(&path) {
                        Ok((id, new)) => {
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
                    if self.selected == Some(id) {
                        let loc = self.highlight;
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
            self.chunks.clear();
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
            parts.push(trf("{} Seiten", &[&p]));
        }
        if !d.language.is_empty() {
            parts.push(d.language.to_uppercase());
        }
        parts.push(trf("{} Abschnitte", &[&d.chunks]));
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
        for d in &self.docs {
            let id = d.id;
            let failed = d.error.is_some() && !self.busy.contains(&id);
            let meta = if failed { d.error.clone().unwrap_or_default() } else { self.meta(d) };
            let busy = self.busy.contains(&id) || d.indexed_at.is_none() && d.error.is_none();
            items.push(
                h_flex()
                    .id(("doc", id as usize))
                    .px_2()
                    .py_1p5()
                    .gap_2()
                    .items_start()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.muted))
                    .when(self.selected == Some(id), |el| el.bg(theme.accent))
                    .child(Self::kind_badge(d.kind, cx).mt(px(2.)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
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
                    .on_click(cx.listener(move |this, _, _, cx| this.open(id, None, cx)))
                    .into_any_element(),
            );
        }
        v_flex().gap_0p5().children(items).into_any_element()
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
            .child(Button::new("docs-add-empty").primary().icon(IconName::Upload).label(tr("Dateien hinzufügen")).on_click(cx.listener(|this, _, _, cx| this.pick_files(cx))))
            .child(div().text_xs().text_color(theme.muted_foreground).child(tr("…oder Dateien und Ordner hierher ziehen")))
            .into_any_element()
    }

    fn render_reader(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
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

        // The text grouped by page / section, the cited one highlighted.
        let mut blocks: Vec<AnyElement> = Vec::new();
        let mut highlight_ix = None;
        let mut last: Option<(u32, String)> = None;
        for c in &self.chunks {
            let lit = self.highlight == Some(c.loc);
            if lit && highlight_ix.is_none() {
                highlight_ix = Some(blocks.len());
            }
            let new_loc = last.as_ref().is_none_or(|(l, _)| *l != c.loc);
            let new_heading = last.as_ref().is_none_or(|(_, h)| *h != c.heading);
            let label = (new_loc && doc.kind.paged()).then(|| docs::loc_label(doc.kind, c.loc));
            let heading = (new_heading && !c.heading.is_empty()).then(|| c.heading.clone());
            last = Some((c.loc, c.heading.clone()));
            blocks.push(
                v_flex()
                    .gap_1()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius)
                    .when(lit, |el| el.bg(theme.warning.opacity(0.12)).border_l_2().border_color(theme.warning))
                    .when_some(label, |el, l| el.child(div().text_xs().font_semibold().text_color(theme.muted_foreground).child(l)))
                    .when_some(heading, |el, h| el.child(div().text_sm().font_semibold().child(h)))
                    .children(c.text.split("\n\n").filter(|p| !p.trim().is_empty()).map(|p| ui::bidi_text(p.replace('\n', " "), true).text_sm().line_height(relative(1.55))))
                    .into_any_element(),
            );
        }
        if blocks.is_empty() && !busy {
            blocks.push(div().p_4().text_sm().text_color(theme.muted_foreground).child(tr("Kein Text gefunden.")).into_any_element());
        }
        if std::mem::take(&mut self.scroll_to_highlight)
            && let Some(ix) = highlight_ix
        {
            self.reader_scroll.scroll_to_top_of_item(ix);
        }
        v_flex()
            .size_full()
            .child(header)
            .child(
                div().flex_1().min_h_0().child(
                    v_flex()
                        .id("doc-reader")
                        .size_full()
                        .track_scroll(&self.reader_scroll)
                        .overflow_y_scroll()
                        .px_4()
                        .py_3()
                        .gap_1()
                        .children(blocks),
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
                        .child(div().p_3().child(Input::new(&self.search).small().prefix(Icon::new(IconName::Search).small().text_color(theme.muted_foreground)).cleanable(true)))
                        .child(div().flex_1().min_h_0().child(v_flex().id("doc-list").size_full().px_2().pb_2().overflow_y_scroll().child(list))),
                )
                .child(div().flex_1().min_w_0().h_full().child(reader))
                .into_any_element()
        };
        v_flex()
            .id("documents")
            .size_full()
            .drag_over::<ExternalPaths>(|style, _, _, cx| style.bg(cx.theme().primary.opacity(0.06)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.add_paths(paths.paths().to_vec(), cx)))
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
                        el.child(Button::new("docs-add").small().primary().icon(IconName::Upload).label(tr("Hinzufügen")).on_click(cx.listener(|this, _, _, cx| this.pick_files(cx))))
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
