//! Renders Ed's XML documents (`<document version="2.0"><paragraph>…`) into
//! styled lines for the content pane, collecting the files they link to.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use roxmltree::Node;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::api::Slide;
use crate::download::{filename_from_url, is_downloadable};

#[derive(Debug, Clone, PartialEq)]
pub struct FileRef {
    /// 1-based number shown as `[file N]` in the content pane.
    pub num: usize,
    pub name: String,
    pub url: String,
}

/// One logical line. `cont` is the prefix repeated on rows created by wrapping
/// (list indentation, callout bars, code gutters).
#[derive(Debug, Clone, Default)]
pub struct DocLine {
    pub spans: Vec<Span<'static>>,
    pub cont: Vec<Span<'static>>,
}

impl DocLine {
    fn is_blank(&self) -> bool {
        self.spans.iter().all(|span| span.content.trim().is_empty())
    }
}

#[derive(Debug, Clone, Default)]
pub struct Doc {
    pub lines: Vec<DocLine>,
    pub files: Vec<FileRef>,
}

const BLOCK_TAGS: &[&str] = &[
    "document",
    "paragraph",
    "heading",
    "list",
    "list-item",
    "pre",
    "snippet",
    "callout",
    "blockquote",
    "figure",
    "table",
    "file",
    "image",
    "video",
    "hr",
    "horizontal-rule",
];

pub fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn file_style(downloadable: bool) -> Style {
    if downloadable {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        dim()
    }
}

fn file_label(num: usize, name: &str, downloadable: bool) -> String {
    let tag = if num == 0 {
        "[file]".to_string()
    } else {
        format!("[file {num}]")
    };
    if downloadable {
        format!("{tag} {name}")
    } else {
        format!("{tag} {name} (external)")
    }
}

impl Doc {
    pub fn push(&mut self, spans: Vec<Span<'static>>) {
        self.lines.push(DocLine {
            spans,
            cont: Vec::new(),
        });
    }

    pub fn text(&mut self, text: impl Into<String>, style: Style) {
        self.push(vec![Span::styled(text.into(), style)]);
    }

    /// Adds an empty line unless the doc is empty or already ends with one.
    pub fn blank(&mut self) {
        if self.lines.last().is_some_and(|line| !line.is_blank()) {
            self.lines.push(DocLine::default());
        }
    }

    /// Registers a file and returns its number; the same URL keeps one number.
    pub fn add_file(&mut self, name: String, url: String) -> usize {
        if url.is_empty() {
            return 0;
        }
        if let Some(existing) = self.files.iter().find(|file| file.url == url) {
            return existing.num;
        }
        let num = self.files.len() + 1;
        self.files.push(FileRef { num, name, url });
        num
    }

    pub fn downloadable_files(&self) -> Vec<FileRef> {
        self.files
            .iter()
            .filter(|file| is_downloadable(&file.url))
            .cloned()
            .collect()
    }

    /// Appends Ed XML. Falls back to tag-stripped text for HTML or broken XML,
    /// and shows plain text as-is.
    pub fn append_ed(&mut self, source: &str) {
        let src = source.trim();
        if src.is_empty() {
            return;
        }
        if !src.starts_with('<') {
            self.append_plain(src);
            return;
        }

        let wrapped;
        let parsed = match roxmltree::Document::parse(src) {
            Ok(document) => Some(document),
            Err(_) => {
                wrapped = format!("<document>{src}</document>");
                roxmltree::Document::parse(&wrapped).ok()
            }
        };
        match parsed {
            Some(document) => {
                let mut renderer = Renderer {
                    doc: self,
                    list_depth: 0,
                };
                renderer.block(document.root_element());
            }
            None => self.append_plain(&strip_tags(src)),
        }
        while self.lines.last().is_some_and(DocLine::is_blank) {
            self.lines.pop();
        }
    }

    fn append_plain(&mut self, text: &str) {
        for line in text.lines() {
            let line = line.trim_end();
            if line.is_empty() {
                self.blank();
            } else {
                self.text(line.replace('\t', "    "), Style::default());
            }
        }
    }
}

/// File name used for a slide's own file (PDF slides).
pub fn slide_file_name(slide: &Slide) -> String {
    let base = if slide.title.trim().is_empty() {
        format!("slide-{}", slide.id)
    } else {
        slide.title.trim().to_string()
    };
    if slide.kind.eq_ignore_ascii_case("pdf") && !base.to_lowercase().ends_with(".pdf") {
        format!("{base}.pdf")
    } else {
        base
    }
}

/// The slide's own file (if any) followed by its rendered content.
pub fn slide_body(slide: &Slide) -> Doc {
    let mut doc = Doc::default();
    if !slide.file_url.is_empty() {
        let name = slide_file_name(slide);
        let ok = is_downloadable(&slide.file_url);
        let num = doc.add_file(name.clone(), slide.file_url.clone());
        doc.push(vec![
            Span::styled(file_label(num, &name, ok), file_style(ok)),
            Span::styled("  (slide file)", dim()),
        ]);
        doc.blank();
    }
    doc.append_ed(&slide.content);
    doc
}

struct Renderer<'d> {
    doc: &'d mut Doc,
    list_depth: usize,
}

fn is_block(node: Node) -> bool {
    node.is_element() && BLOCK_TAGS.contains(&node.tag_name().name())
}

fn all_text(node: Node) -> String {
    node.descendants()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect()
}

impl Renderer<'_> {
    fn block_children(&mut self, node: Node) {
        let mut run = Vec::new();
        for child in node.children() {
            if is_block(child) {
                self.flush_inline(&mut run);
                self.block(child);
            } else if child.is_element() || child.is_text() {
                run.push(child);
            }
        }
        self.flush_inline(&mut run);
    }

    /// Inline content found directly in a block container becomes a paragraph.
    fn flush_inline(&mut self, run: &mut Vec<Node>) {
        if run.is_empty() {
            return;
        }
        let mut builder = LineBuilder::default();
        for node in run.drain(..) {
            self.inline(node, Style::default(), &mut builder);
        }
        let lines = builder.finish();
        if lines.iter().all(|line| line.is_empty()) {
            return;
        }
        self.doc.blank();
        for line in lines {
            self.doc.push(line);
        }
        self.doc.blank();
    }

    fn block(&mut self, node: Node) {
        match node.tag_name().name() {
            "paragraph" => self.paragraph(node, Style::default()),
            "heading" => {
                let level: u8 = node
                    .attribute("level")
                    .and_then(|l| l.parse().ok())
                    .unwrap_or(1);
                let style = match level {
                    1 => Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    2 => Style::default().fg(Color::LightBlue).add_modifier(Modifier::BOLD),
                    _ => Style::default().add_modifier(Modifier::BOLD),
                };
                self.paragraph(node, style);
            }
            "list" => self.list(node),
            "pre" | "snippet" => self.code_block(&all_text(node)),
            "callout" => {
                let color = match node.attribute("type").unwrap_or("") {
                    "warning" => Color::Yellow,
                    "error" | "danger" => Color::Red,
                    "success" => Color::Green,
                    _ => Color::Blue,
                };
                self.quoted(node, Span::styled("▌ ", Style::default().fg(color)));
            }
            "blockquote" => self.quoted(node, Span::styled("│ ", dim())),
            "file" => {
                let span = self.file_span(node);
                self.doc.push(vec![span]);
            }
            "image" => {
                let src = node.attribute("src").unwrap_or("").to_string();
                self.doc.text(format!("[image] {src}"), dim());
            }
            "video" => {
                let src = node.attribute("src").unwrap_or("").to_string();
                self.doc.text(format!("[video] {src}"), dim());
            }
            "table" => self.table(node),
            "hr" | "horizontal-rule" => {
                self.doc.blank();
                self.doc.text("─".repeat(24), dim());
                self.doc.blank();
            }
            // document, figure, list-item outside a list, unknown containers
            _ => self.block_children(node),
        }
    }

    fn paragraph(&mut self, node: Node, style: Style) {
        let mut builder = LineBuilder::default();
        for child in node.children() {
            self.inline(child, style, &mut builder);
        }
        self.doc.blank();
        for line in builder.finish() {
            self.doc.push(line);
        }
        self.doc.blank();
    }

    /// Renders `f` into a separate buffer (files still go to the shared doc).
    fn sub(&mut self, f: impl FnOnce(&mut Self)) -> Vec<DocLine> {
        let saved = std::mem::take(&mut self.doc.lines);
        f(self);
        std::mem::replace(&mut self.doc.lines, saved)
    }

    fn extend_prefixed(
        &mut self,
        lines: Vec<DocLine>,
        first: Vec<Span<'static>>,
        rest: Vec<Span<'static>>,
    ) {
        for (i, line) in lines.into_iter().enumerate() {
            let mut spans = if i == 0 { first.clone() } else { rest.clone() };
            spans.extend(line.spans);
            let mut cont = rest.clone();
            cont.extend(line.cont);
            self.doc.lines.push(DocLine { spans, cont });
        }
    }

    fn list(&mut self, node: Node) {
        let ordered = matches!(node.attribute("style"), Some("number") | Some("ordered"))
            || node.attribute("type") == Some("ordered");
        self.list_depth += 1;
        let depth = self.list_depth;
        let items: Vec<Node> = node.children().filter(|c| c.is_element()).collect();

        let mut rendered = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let marker = if ordered {
                format!("{}. ", i + 1)
            } else if depth % 2 == 1 {
                "● ".to_string()
            } else {
                "○ ".to_string()
            };
            let mut lines = self.sub(|r| r.block_children(*item));
            lines.retain(|line| !line.is_blank());
            if lines.is_empty() {
                lines.push(DocLine::default());
            }
            rendered.push((marker, lines));
        }
        self.list_depth -= 1;

        self.doc.blank();
        for (marker, lines) in rendered {
            let pad = " ".repeat(UnicodeWidthStr::width(marker.as_str()));
            let first = vec![Span::styled(marker, Style::default().fg(Color::LightBlue))];
            self.extend_prefixed(lines, first, vec![Span::raw(pad)]);
        }
        self.doc.blank();
    }

    fn quoted(&mut self, node: Node, bar: Span<'static>) {
        let mut lines = self.sub(|r| r.block_children(node));
        while lines.first().is_some_and(DocLine::is_blank) {
            lines.remove(0);
        }
        while lines.last().is_some_and(DocLine::is_blank) {
            lines.pop();
        }
        self.doc.blank();
        self.extend_prefixed(lines, vec![bar.clone()], vec![bar]);
        self.doc.blank();
    }

    fn code_block(&mut self, text: &str) {
        let gutter = vec![Span::styled("│ ", dim())];
        self.doc.blank();
        for line in text.trim_matches('\n').split('\n') {
            let line = line.trim_end_matches('\r').replace('\t', "    ");
            let mut spans = gutter.clone();
            spans.push(Span::styled(line, Style::default().fg(Color::Green)));
            self.doc.lines.push(DocLine {
                spans,
                cont: gutter.clone(),
            });
        }
        self.doc.blank();
    }

    fn table(&mut self, node: Node) {
        fn rows<'a, 'i>(node: Node<'a, 'i>) -> Vec<Node<'a, 'i>> {
            let mut out = Vec::new();
            for child in node.children().filter(|c| c.is_element()) {
                match child.tag_name().name() {
                    "thead" | "tbody" | "tfoot" => out.extend(rows(child)),
                    _ => out.push(child),
                }
            }
            out
        }

        self.doc.blank();
        for row in rows(node) {
            let mut builder = LineBuilder::default();
            for (i, cell) in row.children().filter(|c| c.is_element()).enumerate() {
                if i > 0 {
                    builder.push(" │ ", dim());
                }
                for child in cell.children() {
                    self.inline(child, Style::default(), &mut builder);
                }
            }
            for line in builder.finish() {
                self.doc.push(line);
            }
        }
        self.doc.blank();
    }

    fn file_span(&mut self, node: Node) -> Span<'static> {
        let url = node
            .attribute("url")
            .or_else(|| node.attribute("src"))
            .unwrap_or("")
            .trim()
            .to_string();
        let name = node
            .attribute("filename")
            .or_else(|| node.attribute("name"))
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(String::from)
            .unwrap_or_else(|| filename_from_url(&url));
        let name = if name.is_empty() { "file".to_string() } else { name };
        let ok = is_downloadable(&url);
        let num = self.doc.add_file(name.clone(), url);
        Span::styled(file_label(num, &name, ok), file_style(ok))
    }

    fn inline_children(&mut self, node: Node, style: Style, builder: &mut LineBuilder) {
        for child in node.children() {
            self.inline(child, style, builder);
        }
    }

    fn inline(&mut self, node: Node, style: Style, builder: &mut LineBuilder) {
        if node.is_text() {
            if let Some(text) = node.text() {
                builder.push(&text.replace(['\n', '\r'], " "), style);
            }
            return;
        }
        if !node.is_element() {
            return;
        }
        match node.tag_name().name() {
            "bold" | "b" | "strong" => {
                self.inline_children(node, style.add_modifier(Modifier::BOLD), builder)
            }
            "italic" | "i" | "em" => {
                self.inline_children(node, style.add_modifier(Modifier::ITALIC), builder)
            }
            "underline" | "u" => {
                self.inline_children(node, style.add_modifier(Modifier::UNDERLINED), builder)
            }
            "strike" | "strikethrough" | "s" | "del" => {
                self.inline_children(node, style.add_modifier(Modifier::CROSSED_OUT), builder)
            }
            "code" => builder.push(&all_text(node), style.fg(Color::LightGreen)),
            "math" => builder.push(&format!("${}$", all_text(node)), style.fg(Color::Magenta)),
            "link" | "a" => {
                let href = node
                    .attribute("href")
                    .or_else(|| node.attribute("url"))
                    .unwrap_or("")
                    .to_string();
                let text = all_text(node);
                let link_style = style.fg(Color::Cyan).add_modifier(Modifier::UNDERLINED);
                if text.trim().is_empty() {
                    builder.push(&href, link_style);
                } else {
                    self.inline_children(node, link_style, builder);
                    if !href.is_empty() && text.trim() != href {
                        builder.push(&format!(" ({href})"), dim());
                    }
                }
            }
            "break" | "br" => builder.newline(),
            "file" => {
                let span = self.file_span(node);
                builder.push(&span.content, span.style);
            }
            "image" => builder.push(
                &format!("[image] {}", node.attribute("src").unwrap_or("")),
                dim(),
            ),
            _ => self.inline_children(node, style, builder),
        }
    }
}

#[derive(Default)]
struct LineBuilder {
    lines: Vec<Vec<Span<'static>>>,
    current: Vec<Span<'static>>,
}

impl LineBuilder {
    fn push(&mut self, text: &str, style: Style) {
        if text.is_empty() {
            return;
        }
        if let Some(last) = self.current.last_mut() {
            if last.style == style {
                last.content.to_mut().push_str(text);
                return;
            }
        }
        self.current.push(Span::styled(text.to_string(), style));
    }

    fn newline(&mut self) {
        self.lines.push(std::mem::take(&mut self.current));
    }

    fn finish(mut self) -> Vec<Vec<Span<'static>>> {
        self.newline();
        for line in &mut self.lines {
            trim_line(line);
        }
        self.lines
    }
}

fn trim_line(spans: &mut Vec<Span<'static>>) {
    while let Some(first) = spans.first_mut() {
        let trimmed = first.content.trim_start().to_string();
        if trimmed.is_empty() {
            spans.remove(0);
        } else {
            first.content = trimmed.into();
            break;
        }
    }
    while let Some(last) = spans.last_mut() {
        let trimmed = last.content.trim_end().to_string();
        if trimmed.is_empty() {
            spans.pop();
        } else {
            last.content = trimmed.into();
            break;
        }
    }
}

/// Crude HTML-to-text fallback for content that is not well-formed Ed XML.
fn strip_tags(src: &str) -> String {
    const BREAKS: &[&str] = &[
        "p", "br", "div", "li", "tr", "h1", "h2", "h3", "h4", "h5", "h6", "pre", "paragraph",
        "heading", "list-item", "break",
    ];
    let mut out = String::new();
    let mut tag = String::new();
    let mut in_tag = false;
    for c in src.chars() {
        if in_tag {
            if c == '>' {
                in_tag = false;
                let name = tag
                    .trim_start_matches('/')
                    .split(|c: char| c.is_whitespace() || c == '/')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if BREAKS.contains(&name.as_str()) {
                    out.push('\n');
                }
                tag.clear();
            } else {
                tag.push(c);
            }
        } else if c == '<' {
            in_tag = true;
        } else {
            out.push(c);
        }
    }
    out.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Wraps lines to `width` columns. Latin words wrap at spaces; CJK text wraps
/// between any two characters. Continuation rows get the line's `cont` prefix.
pub fn wrap(lines: &[DocLine], width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for line in lines {
        let chars: Vec<(char, Style)> = line
            .spans
            .iter()
            .flat_map(|span| span.content.chars().map(move |c| (c, span.style)))
            .collect();
        let total: usize = chars.iter().map(|(c, _)| char_width(*c)).sum();
        if width == 0 || total <= width {
            out.push(Line::from(line.spans.clone()));
            continue;
        }

        let cont_width: usize = line.cont.iter().map(|span| span.width()).sum();
        let (cont, cont_width) = if cont_width + 8 <= width {
            (line.cont.clone(), cont_width)
        } else {
            (Vec::new(), 0)
        };

        let mut rows: Vec<Vec<(char, Style)>> = vec![Vec::new()];
        let mut row_width = 0;
        let mut avail = width;
        for token in tokenize(&chars) {
            let token_width: usize = token.iter().map(|(c, _)| char_width(*c)).sum();
            if row_width + token_width <= avail {
                rows.last_mut().unwrap().extend_from_slice(token);
                row_width += token_width;
                continue;
            }
            let is_space = token.len() == 1 && token[0].0.is_whitespace();
            if is_space {
                rows.push(Vec::new());
                row_width = 0;
                avail = width - cont_width;
                continue;
            }
            if token_width > width - cont_width {
                for &(c, style) in token {
                    let w = char_width(c);
                    if row_width + w > avail {
                        rows.push(Vec::new());
                        row_width = 0;
                        avail = width - cont_width;
                    }
                    rows.last_mut().unwrap().push((c, style));
                    row_width += w;
                }
                continue;
            }
            rows.push(token.to_vec());
            row_width = token_width;
            avail = width - cont_width;
        }

        for (i, mut row) in rows.into_iter().enumerate() {
            while row.last().is_some_and(|(c, _)| c.is_whitespace()) {
                row.pop();
            }
            let mut spans = if i == 0 { Vec::new() } else { cont.clone() };
            spans.extend(group(&row));
            out.push(Line::from(spans));
        }
    }
    out
}

fn char_width(c: char) -> usize {
    c.width().unwrap_or(0)
}

/// Splits into words, single whitespace characters, and single wide characters.
fn tokenize(chars: &[(char, Style)]) -> Vec<&[(char, Style)]> {
    let mut tokens = Vec::new();
    let mut start = 0;
    for (i, (c, _)) in chars.iter().enumerate() {
        if c.is_whitespace() || char_width(*c) >= 2 {
            if start < i {
                tokens.push(&chars[start..i]);
            }
            tokens.push(&chars[i..i + 1]);
            start = i + 1;
        }
    }
    if start < chars.len() {
        tokens.push(&chars[start..]);
    }
    tokens
}

fn group(row: &[(char, Style)]) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for &(c, style) in row {
        match spans.last_mut() {
            Some(last) if last.style == style => last.content.to_mut().push(c),
            _ => spans.push(Span::styled(c.to_string(), style)),
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(src: &str) -> Doc {
        let mut doc = Doc::default();
        doc.append_ed(src);
        doc
    }

    fn texts(doc: &Doc) -> Vec<String> {
        doc.lines
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn renders_headings_paragraphs_and_inline_styles() {
        let doc = render(
            r#"<document version="2.0"><heading level="1">Intro</heading><paragraph>Hello <bold>world</bold> and <code>x = 1</code></paragraph></document>"#,
        );
        assert_eq!(texts(&doc), vec!["Intro", "", "Hello world and x = 1"]);
        let para = &doc.lines[2];
        assert!(para.spans.iter().any(|s| s.content == "world"
            && s.style.add_modifier.contains(Modifier::BOLD)));
    }

    #[test]
    fn renders_nested_lists() {
        let doc = render(
            r#"<document><list style="bullet"><list-item><paragraph>a</paragraph><list style="number"><list-item><paragraph>b</paragraph></list-item></list></list-item><list-item><paragraph>c</paragraph></list-item></list></document>"#,
        );
        assert_eq!(texts(&doc), vec!["● a", "  1. b", "● c"]);
        assert_eq!(doc.lines[1].cont.iter().map(|s| s.width()).sum::<usize>(), 5);
    }

    #[test]
    fn renders_code_blocks_with_gutter() {
        let doc = render("<document><pre>def f():\n\treturn 1</pre></document>");
        assert_eq!(texts(&doc), vec!["│ def f():", "│     return 1"]);
    }

    #[test]
    fn collects_files_and_links() {
        let doc = render(
            r#"<document><paragraph>See <link href="https://example.com">site</link></paragraph><file url="https://static.us.edusercontent.com/files/abc" filename="lab1.pdf"/><file url="https://example.com/x.zip" filename="x.zip"/><file url="https://static.us.edusercontent.com/files/abc" filename="dup.pdf"/></document>"#,
        );
        assert_eq!(
            texts(&doc),
            vec![
                "See site (https://example.com)",
                "",
                "[file 1] lab1.pdf",
                "[file 2] x.zip (external)",
                "[file 1] dup.pdf",
            ]
        );
        assert_eq!(doc.files.len(), 2);
        let downloadable = doc.downloadable_files();
        assert_eq!(downloadable.len(), 1);
        assert_eq!(downloadable[0].name, "lab1.pdf");
    }

    #[test]
    fn falls_back_for_html_and_plain_text() {
        assert_eq!(texts(&render("just text")), vec!["just text"]);
        assert_eq!(
            texts(&render("<p>a&nbsp;b<br>c</p>")),
            vec!["a b", "c"]
        );
    }

    #[test]
    fn slide_body_lists_pdf_file_first() {
        let slide = Slide {
            id: 7,
            title: "Week 1".into(),
            kind: "pdf".into(),
            file_url: "https://static.us.edusercontent.com/files/w1".into(),
            content: r#"<document><file url="https://static.us.edusercontent.com/files/extra" filename="extra.txt"/></document>"#.into(),
            ..Slide::default()
        };
        let doc = slide_body(&slide);
        let files = doc.downloadable_files();
        assert_eq!(files.len(), 2);
        assert_eq!((files[0].num, files[0].name.as_str()), (1, "Week 1.pdf"));
        assert_eq!((files[1].num, files[1].name.as_str()), (2, "extra.txt"));
    }

    #[test]
    fn wraps_latin_at_spaces_and_cjk_anywhere() {
        let mut doc = Doc::default();
        doc.text("hello wide world", Style::default());
        doc.text("日本語の文章です", Style::default());
        let rows: Vec<String> = wrap(&doc.lines, 10)
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(rows, vec!["hello wide", "world", "日本語の文", "章です"]);
    }

    #[test]
    fn wrapped_rows_keep_continuation_prefix() {
        let doc = render(r#"<document><list><list-item><paragraph>aaaa bbbb cccc dddd</paragraph></list-item></list></document>"#);
        let rows: Vec<String> = wrap(&doc.lines, 12)
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(rows, vec!["● aaaa bbbb", "  cccc dddd"]);
    }
}
