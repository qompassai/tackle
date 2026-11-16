//! Pure ratatui widget constructors. No I/O, no terminal access:
//! tests render through [`ratatui::backend::TestBackend`] and assert
//! on buffer text (the phlow-tui precedent).

use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph, Wrap};

/// Most characters kept from any one rendered line.
pub const LINE_CHARS_MAX: usize = 240;

/// Most lines kept in any one panel body.
pub const PANEL_LINES_MAX: usize = 200;

/// Truncate one line to [`LINE_CHARS_MAX`] characters.
pub fn truncate_line(text: &str) -> String {
    if text.chars().count() <= LINE_CHARS_MAX {
        return text.to_string();
    }
    let mut kept: String = text.chars().take(LINE_CHARS_MAX).collect();
    kept.push('…');
    kept
}

/// A bordered panel titled `title` holding `lines`, each truncated.
pub fn text_panel(lines: &[String], title: &str, color: Color) -> Paragraph<'static> {
    let body: Vec<Line<'static>> = lines
        .iter()
        .take(PANEL_LINES_MAX)
        .map(|line| Line::from(truncate_line(line)))
        .collect();
    Paragraph::new(body)
        .block(
            Block::bordered()
                .title(Line::from(title.to_string()).style(Style::default().fg(color)))
                .border_style(Style::default().fg(color)),
        )
        .wrap(Wrap { trim: false })
}

/// The screen tab bar: all screens, the active one bracketed.
pub fn tab_line(screens: &[&str], active: usize) -> String {
    screens
        .iter()
        .enumerate()
        .map(|(index, name)| {
            if index == active {
                format!("[{name}]")
            } else {
                format!(" {name} ")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(panel: Paragraph<'static>) -> String {
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| frame.render_widget(panel, frame.area()))
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let mut text = String::new();
        for row in 0..buffer.area.height {
            for col in 0..buffer.area.width {
                text.push_str(buffer[(col, row)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn ok_panel_renders_title_and_body() {
        let text = render(text_panel(
            &["editor: LIVE".to_string()],
            "Editor",
            Color::Green,
        ));
        assert!(text.contains("Editor"));
        assert!(text.contains("editor: LIVE"));
    }

    #[test]
    fn adv_overlong_line_is_truncated_in_panel() {
        let long = "z".repeat(LINE_CHARS_MAX + 500);
        let text = render(text_panel(&[long], "Log", Color::Cyan));
        assert!(text.contains('…'));
    }

    #[test]
    fn ok_tab_line_brackets_only_the_active_screen() {
        let line = tab_line(&["Overview", "Editor", "Log"], 1);
        assert!(line.contains("[Editor]"));
        assert!(!line.contains("[Overview]"));
    }
}
