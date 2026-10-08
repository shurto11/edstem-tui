use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Instant;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::ListState;
use reqwest::blocking::Client;

use crate::api::{Course, EdClient, Lesson, Me, Module, Slide};
use crate::config::tilde;
use crate::doc::{self, dim, Doc, FileRef};
use crate::download;

/// Results sent back from worker threads.
pub enum Msg {
    Lessons {
        course_id: i64,
        result: Result<(Vec<Module>, Vec<Lesson>), String>,
    },
    Lesson {
        lesson_id: i64,
        result: Result<Lesson, String>,
    },
    Slide {
        slide_id: i64,
        result: Result<Slide, String>,
    },
    DlProgress(String),
    DlDone {
        saved: Vec<PathBuf>,
        failed: Vec<(String, String)>,
        dest: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Content,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    Course(i64),
    Module,
    Lesson(i64),
    Slide { lesson: i64, slide: i64 },
    Info,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Load {
    NotLoaded,
    Loading,
    Loaded,
}

pub struct Node {
    pub kind: NodeKind,
    pub label: String,
    pub suffix: String,
    pub style: Style,
    pub depth: usize,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub expanded: bool,
    pub load: Load,
}

#[derive(Debug, Clone, PartialEq)]
enum View {
    Hint(String),
    Course(i64),
    Module(usize),
    Lesson(i64),
    Slide { lesson: i64, slide: i64 },
}

pub enum Modal {
    None,
    Help,
    Picker {
        files: Vec<FileRef>,
        dest: PathBuf,
        selected: usize,
    },
    Progress(String),
    Result {
        title: String,
        lines: Vec<String>,
    },
}

pub struct App {
    pub running: bool,
    client: EdClient,
    http: Client,
    tx: Sender<Msg>,
    pub rx: Receiver<Msg>,
    pub user_name: String,
    web_base: Option<String>,
    pub download_root: PathBuf,

    courses: HashMap<i64, Course>,
    module_names: HashMap<i64, String>,
    lessons: HashMap<i64, Lesson>,
    lesson_course: HashMap<i64, i64>,
    lesson_detail: HashSet<i64>,
    slides: HashMap<i64, Slide>,
    slide_detail: HashSet<i64>,
    slide_loading: HashSet<i64>,

    pub nodes: Vec<Node>,
    roots: Vec<usize>,
    course_nodes: HashMap<i64, usize>,
    lesson_nodes: HashMap<i64, usize>,
    pub selected: usize,
    pub tree_state: ListState,
    pub tree_height: u16,

    pub focus: Focus,
    view: View,
    pub content_title: String,
    content: Doc,
    pub wrapped: Vec<Line<'static>>,
    wrap_width: u16,
    pub scroll: usize,
    pub content_height: u16,

    pub modal: Modal,
    pub pending_g: bool,
    status: Option<(String, Instant)>,
}

impl App {
    pub fn new(
        client: EdClient,
        me: Me,
        web_base: Option<String>,
        download_root: PathBuf,
    ) -> Result<Self, anyhow::Error> {
        let (tx, rx) = mpsc::channel();
        let mut app = App {
            running: true,
            client,
            http: download::http_client()?,
            tx,
            rx,
            user_name: me.name,
            web_base,
            download_root,
            courses: HashMap::new(),
            module_names: HashMap::new(),
            lessons: HashMap::new(),
            lesson_course: HashMap::new(),
            lesson_detail: HashSet::new(),
            slides: HashMap::new(),
            slide_detail: HashSet::new(),
            slide_loading: HashSet::new(),
            nodes: Vec::new(),
            roots: Vec::new(),
            course_nodes: HashMap::new(),
            lesson_nodes: HashMap::new(),
            selected: 0,
            tree_state: ListState::default(),
            tree_height: 0,
            focus: Focus::Tree,
            view: View::Hint(String::new()),
            content_title: String::new(),
            content: Doc::default(),
            wrapped: Vec::new(),
            wrap_width: 0,
            scroll: 0,
            content_height: 0,
            modal: Modal::None,
            pending_g: false,
            status: None,
        };

        for course in me.courses {
            let archived = course.status == "archived";
            let style = if archived {
                dim()
            } else {
                Style::default().fg(Color::LightBlue).add_modifier(Modifier::BOLD)
            };
            let suffix = if archived { " [archived]" } else { "" };
            let id = app.add_node(
                None,
                NodeKind::Course(course.id),
                course_label(&course),
                suffix.into(),
                style,
            );
            app.course_nodes.insert(course.id, id);
            app.courses.insert(course.id, course);
        }

        if let Some(&first) = app.roots.first() {
            app.selected = first;
            app.expand(first);
            app.on_select();
        } else {
            let id = app.add_node(
                None,
                NodeKind::Info,
                "(no courses)".into(),
                String::new(),
                dim(),
            );
            app.selected = id;
            app.set_view(View::Hint(
                "This Ed account is not enrolled in any course.".into(),
            ));
        }
        Ok(app)
    }

    // ---- tree structure -------------------------------------------------

    fn add_node(
        &mut self,
        parent: Option<usize>,
        kind: NodeKind,
        label: String,
        suffix: String,
        style: Style,
    ) -> usize {
        let id = self.nodes.len();
        let depth = parent.map(|p| self.nodes[p].depth + 1).unwrap_or(0);
        self.nodes.push(Node {
            kind,
            label,
            suffix,
            style,
            depth,
            parent,
            children: Vec::new(),
            expanded: false,
            load: Load::NotLoaded,
        });
        match parent {
            Some(p) => self.nodes[p].children.push(id),
            None => self.roots.push(id),
        }
        id
    }

    fn add_info(&mut self, parent: usize, text: String, style: Style) {
        self.add_node(Some(parent), NodeKind::Info, text, String::new(), style);
    }

    pub fn visible_rows(&self) -> Vec<usize> {
        fn walk(nodes: &[Node], id: usize, out: &mut Vec<usize>) {
            out.push(id);
            if nodes[id].expanded {
                for &child in &nodes[id].children {
                    walk(nodes, child, out);
                }
            }
        }
        let mut out = Vec::new();
        for &root in &self.roots {
            walk(&self.nodes, root, &mut out);
        }
        out
    }

    pub fn is_expandable(&self, id: usize) -> bool {
        match self.nodes[id].kind {
            NodeKind::Course(_) | NodeKind::Module => true,
            NodeKind::Lesson(lesson) => !self.lessons.get(&lesson).is_some_and(|l| l.locked),
            _ => false,
        }
    }

    fn select(&mut self, id: usize) {
        if self.selected != id {
            self.selected = id;
            self.on_select();
        }
    }

    /// After children are rebuilt the selected node may have been dropped from
    /// the tree: reselect the same item if it still exists, else its ancestor.
    fn fix_selection(&mut self) {
        let rows = self.visible_rows();
        if rows.contains(&self.selected) {
            return;
        }
        let kind = self.nodes[self.selected].kind.clone();
        if kind != NodeKind::Info && kind != NodeKind::Module {
            if let Some(&same) = rows.iter().find(|&&id| self.nodes[id].kind == kind) {
                self.selected = same;
                return;
            }
        }
        let mut id = self.selected;
        while let Some(parent) = self.nodes[id].parent {
            if rows.contains(&parent) {
                self.select(parent);
                return;
            }
            id = parent;
        }
        if let Some(&first) = rows.first() {
            self.select(first);
        }
    }

    // ---- navigation -----------------------------------------------------

    pub fn move_selection(&mut self, delta: isize) {
        let rows = self.visible_rows();
        let Some(pos) = rows.iter().position(|&id| id == self.selected) else {
            return;
        };
        let target = (pos as isize + delta).clamp(0, rows.len() as isize - 1) as usize;
        self.select(rows[target]);
    }

    pub fn go_top(&mut self) {
        match self.focus {
            Focus::Tree => {
                if let Some(&first) = self.visible_rows().first() {
                    self.select(first);
                }
            }
            Focus::Content => self.scroll = 0,
        }
    }

    pub fn go_bottom(&mut self) {
        match self.focus {
            Focus::Tree => {
                if let Some(&last) = self.visible_rows().last() {
                    self.select(last);
                }
            }
            Focus::Content => self.scroll = usize::MAX, // clamped while drawing
        }
    }

    pub fn half_page(&mut self, direction: isize) {
        match self.focus {
            Focus::Tree => {
                let step = (self.tree_height as isize / 2).max(1);
                self.move_selection(direction * step);
            }
            Focus::Content => {
                let step = (self.content_height as isize / 2).max(1);
                self.scroll_content(direction * step);
            }
        }
    }

    pub fn scroll_content(&mut self, delta: isize) {
        let max = self.wrapped.len().saturating_sub(self.content_height as usize);
        let current = self.scroll.min(max) as isize;
        self.scroll = (current + delta).clamp(0, max as isize) as usize;
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Tree => Focus::Content,
            Focus::Content => Focus::Tree,
        };
    }

    /// `l`: expand, step into the first child, or open a slide.
    pub fn tree_right(&mut self) {
        let id = self.selected;
        match self.nodes[id].kind {
            NodeKind::Slide { .. } => self.focus = Focus::Content,
            NodeKind::Info => {}
            _ if !self.is_expandable(id) => self.set_status("This lesson is locked"),
            _ => {
                if !self.nodes[id].expanded {
                    self.expand(id);
                } else if let Some(&child) = self.nodes[id].children.first() {
                    self.select(child);
                }
            }
        }
    }

    /// Enter: toggle a folder node, or open a slide.
    pub fn tree_enter(&mut self) {
        let id = self.selected;
        match self.nodes[id].kind {
            NodeKind::Slide { .. } => self.focus = Focus::Content,
            NodeKind::Info => {}
            _ if !self.is_expandable(id) => self.set_status("This lesson is locked"),
            _ if self.nodes[id].expanded => self.nodes[id].expanded = false,
            _ => self.expand(id),
        }
    }

    /// `h` / Esc: collapse, or go to the parent.
    pub fn tree_left(&mut self) {
        let id = self.selected;
        if self.nodes[id].expanded && !self.nodes[id].children.is_empty() {
            self.nodes[id].expanded = false;
        } else if let Some(parent) = self.nodes[id].parent {
            self.select(parent);
        }
    }

    fn expand(&mut self, id: usize) {
        match self.nodes[id].kind {
            NodeKind::Course(course) => {
                if self.nodes[id].load == Load::NotLoaded {
                    self.fetch_lessons(id, course);
                }
            }
            NodeKind::Lesson(lesson) => {
                if self.nodes[id].load == Load::NotLoaded {
                    self.fetch_lesson(id, lesson);
                }
            }
            NodeKind::Module => {}
            _ => return,
        }
        self.nodes[id].expanded = true;
    }

    /// `r`: fetch the selected item again.
    pub fn reload_selected(&mut self) {
        let id = self.selected;
        match self.nodes[id].kind {
            NodeKind::Course(course) => {
                self.fetch_lessons(id, course);
                self.nodes[id].expanded = true;
            }
            NodeKind::Lesson(lesson) if self.is_expandable(id) => {
                self.fetch_lesson(id, lesson);
                self.nodes[id].expanded = true;
            }
            NodeKind::Slide { slide, .. } => {
                self.fetch_slide(slide);
                self.refresh_view(true);
            }
            _ => return,
        }
        self.fix_selection();
    }

    // ---- loading --------------------------------------------------------

    fn fetch_lessons(&mut self, node: usize, course_id: i64) {
        self.nodes[node].children.clear();
        self.nodes[node].load = Load::Loading;
        self.add_info(node, "Loading…".into(), dim().add_modifier(Modifier::ITALIC));
        let client = self.client.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = client.lessons(course_id).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::Lessons { course_id, result });
        });
    }

    fn fetch_lesson(&mut self, node: usize, lesson_id: i64) {
        self.nodes[node].children.clear();
        self.nodes[node].load = Load::Loading;
        self.add_info(node, "Loading…".into(), dim().add_modifier(Modifier::ITALIC));
        let client = self.client.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = client.lesson(lesson_id).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::Lesson { lesson_id, result });
        });
    }

    fn fetch_slide(&mut self, slide_id: i64) {
        self.slide_detail.insert(slide_id);
        self.slide_loading.insert(slide_id);
        let client = self.client.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = client.slide(slide_id).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::Slide { slide_id, result });
        });
    }

    /// Slides listed with the lesson may come without content; fetch it once.
    fn ensure_slide_content(&mut self, slide_id: i64) {
        let empty = self
            .slides
            .get(&slide_id)
            .is_some_and(|slide| slide.content.is_empty());
        if empty && !self.slide_detail.contains(&slide_id) {
            self.fetch_slide(slide_id);
        }
    }

    pub fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Lessons { course_id, result } => self.on_lessons(course_id, result),
            Msg::Lesson { lesson_id, result } => self.on_lesson(lesson_id, result),
            Msg::Slide { slide_id, result } => {
                self.slide_loading.remove(&slide_id);
                match result {
                    Ok(slide) => {
                        self.slides.insert(slide_id, slide);
                    }
                    Err(e) => self.set_status(&format!("Slide failed to load: {e}")),
                }
                if matches!(self.view, View::Slide { slide, .. } if slide == slide_id) {
                    self.refresh_view(true);
                }
            }
            Msg::DlProgress(text) => {
                if let Modal::Progress(current) = &mut self.modal {
                    *current = text;
                }
            }
            Msg::DlDone {
                saved,
                failed,
                dest,
            } => self.modal = download_report(&saved, &failed, &dest),
        }
    }

    fn on_lessons(&mut self, course_id: i64, result: Result<(Vec<Module>, Vec<Lesson>), String>) {
        let Some(&node) = self.course_nodes.get(&course_id) else {
            return;
        };
        self.nodes[node].children.clear();
        let (modules, lessons) = match result {
            Ok(data) => data,
            Err(e) => {
                self.nodes[node].load = Load::NotLoaded;
                self.add_info(node, format!("Error: {e}"), Style::default().fg(Color::Red));
                self.fix_selection();
                return;
            }
        };
        self.nodes[node].load = Load::Loaded;

        for module in &modules {
            self.module_names.insert(module.id, module.name.clone());
        }
        for lesson in &lessons {
            self.lesson_course.insert(lesson.id, course_id);
            match self.lessons.get_mut(&lesson.id) {
                // keep slides fetched earlier; the list does not include them
                Some(existing) if self.lesson_detail.contains(&lesson.id) => {
                    let slides = std::mem::take(&mut existing.slides);
                    *existing = Lesson {
                        slides,
                        ..lesson.clone()
                    };
                }
                _ => {
                    self.lessons.insert(lesson.id, lesson.clone());
                }
            }
        }

        let known: HashSet<i64> = modules.iter().map(|m| m.id).collect();
        let loose: Vec<&Lesson> = lessons
            .iter()
            .filter(|l| !known.contains(&l.module_id))
            .collect();
        if lessons.is_empty() {
            self.add_info(node, "(no lessons)".into(), dim());
        } else if loose.len() == lessons.len() {
            for lesson in &lessons {
                self.add_lesson_node(node, lesson);
            }
        } else {
            for module in &modules {
                let members: Vec<&Lesson> =
                    lessons.iter().filter(|l| l.module_id == module.id).collect();
                if members.is_empty() {
                    continue;
                }
                let module_node = self.add_node(
                    Some(node),
                    NodeKind::Module,
                    module.name.clone(),
                    String::new(),
                    Style::default().fg(Color::Magenta),
                );
                for lesson in members {
                    self.add_lesson_node(module_node, lesson);
                }
            }
            if !loose.is_empty() {
                let module_node = self.add_node(
                    Some(node),
                    NodeKind::Module,
                    "(No module)".into(),
                    String::new(),
                    Style::default().fg(Color::Magenta),
                );
                for lesson in loose {
                    self.add_lesson_node(module_node, lesson);
                }
            }
        }
        self.fix_selection();
        if self.view == View::Course(course_id) {
            self.refresh_view(true);
        }
    }

    fn add_lesson_node(&mut self, parent: usize, lesson: &Lesson) {
        let label = if lesson.number > 0 {
            format!("{}. {}", lesson.number, lesson.title)
        } else {
            lesson.title.clone()
        };
        let (style, suffix) = if lesson.locked {
            (dim(), " [locked]".to_string())
        } else {
            (Style::default().fg(Color::Green), String::new())
        };
        let id = self.add_node(Some(parent), NodeKind::Lesson(lesson.id), label, suffix, style);
        self.lesson_nodes.insert(lesson.id, id);
        if self.lesson_detail.contains(&lesson.id) {
            self.build_slide_nodes(id, lesson.id);
        }
    }

    fn on_lesson(&mut self, lesson_id: i64, result: Result<Lesson, String>) {
        let node = self.lesson_nodes.get(&lesson_id).copied();
        match result {
            Ok(lesson) => {
                for slide in &lesson.slides {
                    // a later full fetch may already hold content the listing omits
                    let keep_old = slide.content.is_empty()
                        && self
                            .slides
                            .get(&slide.id)
                            .is_some_and(|old| !old.content.is_empty());
                    if !keep_old {
                        self.slides.insert(slide.id, slide.clone());
                    }
                }
                self.lesson_detail.insert(lesson_id);
                if lesson.course_id > 0 {
                    self.lesson_course.entry(lesson_id).or_insert(lesson.course_id);
                }
                let mut lesson = lesson;
                if let Some(old) = self.lessons.get(&lesson_id) {
                    // the detail endpoint can omit list-only fields
                    if lesson.module_id == 0 {
                        lesson.module_id = old.module_id;
                    }
                    if lesson.number == 0 {
                        lesson.number = old.number;
                    }
                }
                self.lessons.insert(lesson_id, lesson);
                if let Some(node) = node {
                    self.build_slide_nodes(node, lesson_id);
                }
            }
            Err(e) => {
                if let Some(node) = node {
                    self.nodes[node].children.clear();
                    self.nodes[node].load = Load::NotLoaded;
                    self.add_info(node, format!("Error: {e}"), Style::default().fg(Color::Red));
                }
            }
        }
        self.fix_selection();
        let showing = match self.view {
            View::Lesson(id) => id == lesson_id,
            View::Slide { lesson, .. } => lesson == lesson_id,
            _ => false,
        };
        if showing {
            self.refresh_view(true);
        }
    }

    fn build_slide_nodes(&mut self, node: usize, lesson_id: i64) {
        self.nodes[node].children.clear();
        self.nodes[node].load = Load::Loaded;
        let slides = self
            .lessons
            .get(&lesson_id)
            .map(|l| l.slides.clone())
            .unwrap_or_default();
        if slides.is_empty() {
            self.add_info(node, "(no slides)".into(), dim());
            return;
        }
        for (i, slide) in slides.iter().enumerate() {
            let title = if slide.title.trim().is_empty() {
                "Slide".to_string()
            } else {
                slide.title.clone()
            };
            let mut suffix = slide_kind_label(&slide.kind);
            if slide.is_hidden {
                suffix.push_str(" [hidden]");
            }
            self.add_node(
                Some(node),
                NodeKind::Slide {
                    lesson: lesson_id,
                    slide: slide.id,
                },
                format!("{}. {}", i + 1, title),
                suffix,
                Style::default(),
            );
        }
    }

    // ---- content pane ---------------------------------------------------

    fn on_select(&mut self) {
        let view = match self.nodes[self.selected].kind {
            NodeKind::Course(id) => View::Course(id),
            NodeKind::Module => View::Module(self.selected),
            NodeKind::Lesson(id) => View::Lesson(id),
            NodeKind::Slide { lesson, slide } => {
                self.ensure_slide_content(slide);
                View::Slide { lesson, slide }
            }
            NodeKind::Info => View::Hint(self.nodes[self.selected].label.clone()),
        };
        self.set_view(view);
    }

    fn set_view(&mut self, view: View) {
        self.view = view;
        self.refresh_view(false);
    }

    fn refresh_view(&mut self, keep_scroll: bool) {
        let (title, doc) = match self.view.clone() {
            View::Hint(text) => {
                let mut doc = Doc::default();
                doc.text(text, dim());
                (" edstem-tui ".to_string(), doc)
            }
            View::Course(id) => self.course_doc(id),
            View::Module(node) => self.module_doc(node),
            View::Lesson(id) => self.lesson_doc(id),
            View::Slide { lesson, slide } => self.slide_doc(lesson, slide),
        };
        self.content_title = title;
        self.content = doc;
        self.wrap_width = 0; // force re-wrap
        if !keep_scroll {
            self.scroll = 0;
        }
    }

    pub fn ensure_wrapped(&mut self, width: u16) {
        if self.wrap_width != width {
            self.wrapped = doc::wrap(&self.content.lines, width as usize);
            self.wrap_width = width;
        }
    }

    fn course_doc(&self, id: i64) -> (String, Doc) {
        let mut doc = Doc::default();
        let Some(course) = self.courses.get(&id) else {
            return (" Course ".into(), doc);
        };
        doc.text(course.name.clone(), heading_style());
        let term = [course.year.as_str(), course.session.as_str()]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        meta(&mut doc, "Code", &course.code);
        meta(&mut doc, "Term", &term);
        meta(&mut doc, "Status", &course.status);
        meta(&mut doc, "Role", &course.role);
        doc.blank();
        doc.text("l: show lessons   r: reload", dim());
        (format!(" {} ", course_label(course)), doc)
    }

    fn module_doc(&self, node: usize) -> (String, Doc) {
        let mut doc = Doc::default();
        let module = &self.nodes[node];
        doc.text(module.label.clone(), heading_style());
        meta(&mut doc, "Lessons", &module.children.len().to_string());
        doc.blank();
        doc.text("l: show lessons", dim());
        (format!(" {} ", module.label), doc)
    }

    fn lesson_doc(&self, id: i64) -> (String, Doc) {
        let mut doc = Doc::default();
        let Some(lesson) = self.lessons.get(&id) else {
            return (" Lesson ".into(), doc);
        };
        doc.text(lesson.title.clone(), heading_style());
        meta(&mut doc, "Type", &lesson.kind);
        meta(&mut doc, "Status", &lesson.status);
        if lesson.slide_count > 0 {
            meta(&mut doc, "Slides", &lesson.slide_count.to_string());
        }
        meta(&mut doc, "Available", &format_date(&lesson.available_at));
        meta(&mut doc, "Due", &format_date(&lesson.due_at));
        if lesson.locked {
            doc.blank();
            doc.text("This lesson is not open yet.", Style::default().fg(Color::Red));
        }
        if !lesson.outline.trim().is_empty() {
            doc.blank();
            doc.append_ed(&lesson.outline);
        }

        doc.blank();
        if self.lesson_detail.contains(&id) {
            doc.text("Slides", Style::default().add_modifier(Modifier::BOLD));
            for (i, slide) in lesson.slides.iter().enumerate() {
                doc.push(vec![
                    Span::raw(format!("  {}. {}", i + 1, slide.title)),
                    Span::styled(slide_kind_label(&slide.kind), dim()),
                ]);
            }
            doc.blank();
        } else if !lesson.locked {
            doc.text("l: load slides", dim());
        }
        if !lesson.locked {
            doc.text(
                format!(
                    "D: download every file in this lesson to {}",
                    tilde(&self.lesson_dir(id))
                ),
                dim(),
            );
        }
        (format!(" {} ", lesson.title), doc)
    }

    fn slide_doc(&self, lesson_id: i64, slide_id: i64) -> (String, Doc) {
        let mut doc = Doc::default();
        let lesson = self.lessons.get(&lesson_id);
        let Some(slide) = self.slides.get(&slide_id) else {
            return (" Slide ".into(), doc);
        };
        let lesson_title = lesson.map(|l| l.title.as_str()).unwrap_or("Lesson");
        let title = format!(" {} / {} ", lesson_title, slide.title);

        let (pos, total) = lesson
            .and_then(|l| {
                l.slides
                    .iter()
                    .position(|s| s.id == slide_id)
                    .map(|p| (p + 1, l.slides.len()))
            })
            .unwrap_or((0, 0));
        doc.text(slide.title.clone(), heading_style());
        let mut info = format!("Slide {pos}/{total}");
        if !slide.kind.is_empty() {
            info.push_str(&format!(" · {}", slide.kind));
        }
        doc.text(info, dim());
        doc.blank();

        if self.slide_loading.contains(&slide_id) {
            doc.text("Loading…", dim().add_modifier(Modifier::ITALIC));
            return (title, doc);
        }

        let body = doc::slide_body(slide);
        if body.lines.is_empty() {
            let note = match slide.kind.to_ascii_lowercase().as_str() {
                "quiz" => "(quiz slide: questions are not shown here)",
                "video" => "(video slide)",
                "code" => "(code challenge without a description)",
                _ => "(no text content)",
            };
            doc.text(note, dim());
        }
        let downloadable = body.downloadable_files().len();
        doc.lines.extend(body.lines);
        doc.files = body.files;

        doc.blank();
        if downloadable > 0 {
            doc.text(
                format!("d: download {downloadable} file(s)   D: download the whole lesson"),
                dim(),
            );
        }
        if let (Some(web), Some(course)) = (&self.web_base, self.lesson_course.get(&lesson_id)) {
            doc.text(
                format!("{web}/courses/{course}/lessons/{lesson_id}/slides/{slide_id}"),
                dim(),
            );
        }
        (title, doc)
    }

    // ---- downloads ------------------------------------------------------

    fn lesson_dir(&self, lesson_id: i64) -> PathBuf {
        let course = self
            .lesson_course
            .get(&lesson_id)
            .and_then(|id| self.courses.get(id));
        let course_dir = course
            .map(|c| if c.code.trim().is_empty() { c.name.clone() } else { c.code.clone() })
            .map(|name| download::sanitize(&name))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "course".into());
        // Lessons in different modules often share a title ("Lecture slides"),
        // so nest them under their module the way the tree does.
        let module_dir = self
            .lessons
            .get(&lesson_id)
            .and_then(|l| self.module_names.get(&l.module_id))
            .map(|name| download::sanitize(name))
            .filter(|name| !name.is_empty());
        let lesson_dir = self
            .lessons
            .get(&lesson_id)
            .map(|l| {
                if l.number > 0 {
                    format!("{:02} {}", l.number, l.title)
                } else {
                    l.title.clone()
                }
            })
            .map(|name| download::sanitize(&name))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| format!("lesson-{lesson_id}"));
        let mut dir = self.download_root.join(course_dir);
        if let Some(module_dir) = module_dir {
            dir.push(module_dir);
        }
        dir.join(lesson_dir)
    }

    /// `d`: files of the selected slide (picker when several), or a whole lesson.
    pub fn download_selected(&mut self) {
        match self.nodes[self.selected].kind {
            NodeKind::Slide { lesson, slide } => {
                if self.slide_loading.contains(&slide) {
                    self.set_status("The slide is still loading");
                    return;
                }
                let Some(data) = self.slides.get(&slide) else {
                    return;
                };
                let files = doc::slide_body(data).downloadable_files();
                let dest = self.lesson_dir(lesson);
                match files.len() {
                    0 => {
                        self.modal = Modal::Result {
                            title: "Nothing to download".into(),
                            lines: vec!["This slide has no files hosted on Ed.".into()],
                        }
                    }
                    1 => self.start_download(files, dest),
                    _ => {
                        self.modal = Modal::Picker {
                            files,
                            dest,
                            selected: 0,
                        }
                    }
                }
            }
            NodeKind::Lesson(lesson) => self.download_lesson(lesson),
            _ => self.set_status("Select a lesson or a slide to download"),
        }
    }

    /// `D`: every file in the lesson of the selected lesson or slide.
    pub fn download_lesson_of_selected(&mut self) {
        match self.nodes[self.selected].kind {
            NodeKind::Lesson(lesson) | NodeKind::Slide { lesson, .. } => {
                self.download_lesson(lesson)
            }
            _ => self.set_status("Select a lesson or a slide to download"),
        }
    }

    pub fn pick_download(&mut self, index: usize) {
        let Modal::Picker { files, dest, .. } = std::mem::replace(&mut self.modal, Modal::None)
        else {
            return;
        };
        let chosen = if index == 0 {
            files
        } else {
            files.into_iter().skip(index - 1).take(1).collect()
        };
        self.start_download(chosen, dest);
    }

    fn start_download(&mut self, files: Vec<FileRef>, dest: PathBuf) {
        self.modal = Modal::Progress(format!("Downloading {} file(s)…", files.len()));
        let http = self.http.clone();
        let tx = self.tx.clone();
        thread::spawn(move || download_all(&http, files, &dest, &tx));
    }

    fn download_lesson(&mut self, lesson_id: i64) {
        if self.lessons.get(&lesson_id).is_some_and(|l| l.locked) {
            self.set_status("This lesson is locked");
            return;
        }
        // Best-known slides; content-less ones are fetched by the worker.
        let slides: Option<Vec<Slide>> = self.lesson_detail.contains(&lesson_id).then(|| {
            self.lessons[&lesson_id]
                .slides
                .iter()
                .map(|s| self.slides.get(&s.id).cloned().unwrap_or_else(|| s.clone()))
                .collect()
        });
        let fetched = self.slide_detail.clone();
        let dest = self.lesson_dir(lesson_id);
        let client = self.client.clone();
        let http = self.http.clone();
        let tx = self.tx.clone();
        self.modal = Modal::Progress("Scanning the lesson…".into());

        thread::spawn(move || {
            let slides = match slides {
                Some(slides) => slides,
                None => match client.lesson(lesson_id) {
                    Ok(lesson) => {
                        let slides = lesson.slides.clone();
                        let _ = tx.send(Msg::Lesson {
                            lesson_id,
                            result: Ok(lesson),
                        });
                        slides
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::DlDone {
                            saved: Vec::new(),
                            failed: vec![("lesson".into(), format!("{e:#}"))],
                            dest,
                        });
                        return;
                    }
                },
            };

            let total = slides.len();
            let mut files = Vec::new();
            let mut seen = HashSet::new();
            for (i, slide) in slides.into_iter().enumerate() {
                let slide = if slide.content.is_empty() && !fetched.contains(&slide.id) {
                    let _ = tx.send(Msg::DlProgress(format!(
                        "Scanning slides {}/{}",
                        i + 1,
                        total
                    )));
                    match client.slide(slide.id) {
                        Ok(full) => {
                            let _ = tx.send(Msg::Slide {
                                slide_id: slide.id,
                                result: Ok(full.clone()),
                            });
                            full
                        }
                        Err(_) => slide,
                    }
                } else {
                    slide
                };
                for file in doc::slide_body(&slide).downloadable_files() {
                    if seen.insert(file.url.clone()) {
                        files.push(file);
                    }
                }
            }
            download_all(&http, files, &dest, &tx);
        });
    }

    // ---- status line ----------------------------------------------------

    pub fn set_status(&mut self, text: &str) {
        self.status = Some((text.to_string(), Instant::now()));
    }

    pub fn status(&self) -> Option<&str> {
        self.status
            .as_ref()
            .filter(|(_, at)| at.elapsed().as_secs() < 5)
            .map(|(text, _)| text.as_str())
    }
}

fn download_all(http: &Client, files: Vec<FileRef>, dest: &Path, tx: &Sender<Msg>) {
    let total = files.len();
    let mut saved = Vec::new();
    let mut failed = Vec::new();
    for (i, file) in files.iter().enumerate() {
        let _ = tx.send(Msg::DlProgress(format!("{}/{}  {}", i + 1, total, file.name)));
        match download::download(http, file, dest) {
            Ok(path) => saved.push(path),
            Err(e) => failed.push((file.name.clone(), format!("{e:#}"))),
        }
    }
    let _ = tx.send(Msg::DlDone {
        saved,
        failed,
        dest: dest.to_path_buf(),
    });
}

fn download_report(saved: &[PathBuf], failed: &[(String, String)], dest: &Path) -> Modal {
    const LIST_LIMIT: usize = 12;
    let mut lines = Vec::new();
    let title = if saved.is_empty() && failed.is_empty() {
        lines.push("No files hosted on Ed were found.".into());
        "Nothing to download"
    } else if failed.is_empty() {
        "Downloaded"
    } else {
        "Download finished with errors"
    };
    if !saved.is_empty() {
        lines.push(format!("{} file(s) saved to {}", saved.len(), tilde(dest)));
        for path in saved.iter().take(LIST_LIMIT) {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            lines.push(format!("  ✓ {name}"));
        }
        if saved.len() > LIST_LIMIT {
            lines.push(format!("  … and {} more", saved.len() - LIST_LIMIT));
        }
    }
    for (name, error) in failed {
        lines.push(format!("  ✗ {name}: {error}"));
    }
    Modal::Result {
        title: title.into(),
        lines,
    }
}

fn course_label(course: &Course) -> String {
    match (course.code.trim(), course.name.trim()) {
        ("", name) => name.to_string(),
        (code, "") => code.to_string(),
        (code, name) if name.starts_with(code) => name.to_string(),
        (code, name) => format!("{code} {name}"),
    }
}

fn slide_kind_label(kind: &str) -> String {
    match kind.to_ascii_lowercase().as_str() {
        "" | "document" => String::new(),
        other => format!(" [{other}]"),
    }
}

fn heading_style() -> Style {
    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
}

fn meta(doc: &mut Doc, label: &str, value: &str) {
    if value.trim().is_empty() {
        return;
    }
    doc.push(vec![
        Span::styled(format!("{label:<10}"), dim()),
        Span::raw(value.to_string()),
    ]);
}

/// "2026-03-01T10:00:00.000+11:00" -> "2026-03-01 10:00"
fn format_date(raw: &str) -> String {
    let raw = raw.trim();
    if raw.len() >= 16 && raw.is_char_boundary(16) {
        raw[..16].replace('T', " ")
    } else {
        raw.to_string()
    }
}
