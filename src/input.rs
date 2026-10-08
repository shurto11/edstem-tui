use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, Focus, Modal};

/// Vim-style keys only; arrow keys are deliberately ignored (as in teams-cli).
pub fn handle_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl && key.code == KeyCode::Char('c') {
        app.running = false;
        return;
    }
    if !matches!(app.modal, Modal::None) {
        modal_key(app, key);
        return;
    }

    if key.code == KeyCode::Char('g') && !ctrl {
        if app.pending_g {
            app.pending_g = false;
            app.go_top();
        } else {
            app.pending_g = true;
        }
        return;
    }
    app.pending_g = false;

    match key.code {
        KeyCode::Char('q') => app.running = false,
        KeyCode::Char('?') => app.modal = Modal::Help,
        KeyCode::Tab | KeyCode::BackTab => app.toggle_focus(),
        KeyCode::Char('d') if ctrl => app.half_page(1),
        KeyCode::Char('u') if ctrl => app.half_page(-1),
        KeyCode::Char('G') => app.go_bottom(),
        KeyCode::Char('d') => app.download_selected(),
        KeyCode::Char('D') => app.download_lesson_of_selected(),
        _ => match app.focus {
            Focus::Tree => match key.code {
                KeyCode::Char('j') => app.move_selection(1),
                KeyCode::Char('k') => app.move_selection(-1),
                KeyCode::Char('l') => app.tree_right(),
                KeyCode::Enter => app.tree_enter(),
                KeyCode::Char('h') | KeyCode::Esc => app.tree_left(),
                KeyCode::Char('r') => app.reload_selected(),
                _ => {}
            },
            Focus::Content => match key.code {
                KeyCode::Char('j') => app.scroll_content(1),
                KeyCode::Char('k') => app.scroll_content(-1),
                KeyCode::Char('h') | KeyCode::Esc => app.focus = Focus::Tree,
                _ => {}
            },
        },
    }
}

fn modal_key(app: &mut App, key: KeyEvent) {
    match &mut app.modal {
        Modal::None | Modal::Progress(_) => {}
        Modal::Help => {
            if matches!(
                key.code,
                KeyCode::Char('?' | 'q' | 'h') | KeyCode::Esc | KeyCode::Enter
            ) {
                app.modal = Modal::None;
            }
        }
        Modal::Result { .. } => {
            if matches!(
                key.code,
                KeyCode::Char('q' | 'h' | 'l') | KeyCode::Esc | KeyCode::Enter
            ) {
                app.modal = Modal::None;
            }
        }
        Modal::Picker {
            files, selected, ..
        } => match key.code {
            // row 0 is "All files", then one row per file
            KeyCode::Char('j') => *selected = (*selected + 1).min(files.len()),
            KeyCode::Char('k') => *selected = selected.saturating_sub(1),
            KeyCode::Char('l') | KeyCode::Enter => {
                let index = *selected;
                app.pick_download(index);
            }
            KeyCode::Char('h' | 'q') | KeyCode::Esc => app.modal = Modal::None,
            _ => {}
        },
    }
}
