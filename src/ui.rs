use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, Focus, Modal};
use crate::config::tilde;
use crate::doc::dim;

pub fn draw(f: &mut Frame, app: &mut App) {
    let [main, bar] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(f.area());
    let [left, right] =
        Layout::horizontal([Constraint::Ratio(1, 3), Constraint::Ratio(2, 3)]).areas(main);

    draw_tree(f, app, left);
    draw_content(f, app, right);
    draw_help_bar(f, app, bar);

    match &app.modal {
        Modal::None => {}
        Modal::Help => draw_help(f, app),
        Modal::Picker {
            files,
            dest,
            selected,
        } => {
            let mut items = vec![ListItem::new(Line::styled(
                format!("All files ({})", files.len()),
                Style::default().add_modifier(Modifier::BOLD),
            ))];
            items.extend(
                files
                    .iter()
                    .map(|file| ListItem::new(format!("[file {}] {}", file.num, file.name))),
            );
            let area = centered(f.area(), 80, items.len() as u16 + 2);
            let block = modal_block(" Download ")
                .title_bottom(Line::styled(format!(" to {} ", tilde(dest)), dim()));
            let list = List::new(items)
                .block(block)
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
            let mut state = ListState::default().with_selected(Some(*selected));
            f.render_widget(Clear, area);
            f.render_stateful_widget(list, area, &mut state);
        }
        Modal::Progress(text) => {
            let area = centered(f.area(), 70, 5);
            let body = Paragraph::new(vec![Line::raw(""), Line::raw(text.clone()).centered()])
                .block(modal_block(" Please wait "));
            f.render_widget(Clear, area);
            f.render_widget(body, area);
        }
        Modal::Result { title, lines } => {
            let text: Vec<Line> = lines.iter().map(|line| Line::raw(line.clone())).collect();
            let area = centered(f.area(), 90, text.len() as u16 + 2);
            let block = modal_block(&format!(" {title} "))
                .title_bottom(Line::styled(" Enter: close ", dim()));
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(text).block(block).wrap(Wrap { trim: false }),
                area,
            );
        }
    }
}

fn draw_tree(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = app.visible_rows();
    let items: Vec<ListItem> = rows
        .iter()
        .map(|&id| {
            let node = &app.nodes[id];
            let marker = if app.is_expandable(id) {
                if node.expanded {
                    "▼ "
                } else {
                    "▶ "
                }
            } else {
                "  "
            };
            ListItem::new(Line::from(vec![
                Span::raw("  ".repeat(node.depth)),
                Span::styled(marker, dim()),
                Span::styled(node.label.clone(), node.style),
                Span::styled(node.suffix.clone(), dim()),
            ]))
        })
        .collect();

    let focused = app.focus == Focus::Tree;
    let title = if app.user_name.is_empty() {
        " Ed ".to_string()
    } else {
        format!(" Ed: {} ", app.user_name)
    };
    let highlight = if focused {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default().bg(Color::DarkGray)
    };
    let list = List::new(items)
        .block(pane_block(title, focused))
        .highlight_style(highlight);

    app.tree_state
        .select(rows.iter().position(|&id| id == app.selected));
    app.tree_height = area.height.saturating_sub(2);
    f.render_stateful_widget(list, area, &mut app.tree_state);
}

fn draw_content(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Content;
    let block = pane_block(app.content_title.clone(), focused);
    let inner = block.inner(area);
    app.ensure_wrapped(inner.width);
    app.content_height = inner.height;

    let height = inner.height as usize;
    let total = app.wrapped.len();
    app.scroll = app.scroll.min(total.saturating_sub(height));
    let end = (app.scroll + height).min(total);
    let lines = app.wrapped[app.scroll..end].to_vec();

    let block = if total > height {
        block.title_bottom(
            Line::styled(format!(" {}-{}/{} ", app.scroll + 1, end, total), dim())
                .right_aligned(),
        )
    } else {
        block
    };
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_help_bar(f: &mut Frame, app: &App, area: Rect) {
    let (mode, keys) = match app.focus {
        Focus::Tree => (
            " TREE ",
            " j/k move  l open  h back  ^d/^u page  gg/G  d download  D lesson  r reload  Tab  ? help  q quit",
        ),
        Focus::Content => (
            " CONTENT ",
            " j/k scroll  ^d/^u page  gg/G  d download  D lesson  h/Esc/Tab tree  ? help  q quit",
        ),
    };
    let mut spans = vec![
        Span::styled(
            mode,
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(keys, dim()),
    ];
    if let Some(status) = app.status() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(status.to_string(), Style::default().fg(Color::Yellow)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_help(f: &mut Frame, app: &App) {
    let section = |name: &str| Span::styled(format!("{name:<9}"), Style::default().fg(Color::Yellow));
    let row = |section: Span<'static>, key: &str, what: &str| {
        Line::from(vec![
            section,
            Span::styled(format!("{key:<11}"), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(what.to_string()),
        ])
    };
    let blank = || Span::raw(" ".repeat(9));
    let lines = vec![
        row(section("Tree"), "j / k", "move"),
        row(blank(), "l / Enter", "expand / open the slide"),
        row(blank(), "h / Esc", "collapse / go to parent"),
        row(blank(), "r", "reload the selected item"),
        Line::raw(""),
        row(section("Content"), "j / k", "scroll"),
        row(blank(), "h/Esc/Tab", "back to the tree"),
        Line::raw(""),
        row(section("Both"), "Ctrl+d/u", "half page down / up"),
        row(blank(), "gg / G", "top / bottom"),
        row(blank(), "Tab", "switch pane"),
        row(blank(), "d", "download this slide's files"),
        row(blank(), "", "(on a lesson: every file in it)"),
        row(blank(), "D", "download every file in the lesson"),
        Line::raw(""),
        row(section("Dialogs"), "j/k l h", "move / choose / close"),
        row(section("App"), "?", "this help"),
        row(blank(), "q / Ctrl+C", "quit"),
        Line::raw(""),
        Line::styled(format!("Downloads go to {}", tilde(&app.download_root)), dim()),
    ];
    let area = centered(f.area(), 64, lines.len() as u16 + 2);
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines).block(modal_block(" Keys ")), area);
}

fn pane_block(title: String, focused: bool) -> Block<'static> {
    let border = if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    Block::bordered().title(title).border_style(border)
}

fn modal_block(title: &str) -> Block<'static> {
    Block::bordered()
        .title(title.to_string())
        .border_style(Style::default().fg(Color::Yellow))
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}
