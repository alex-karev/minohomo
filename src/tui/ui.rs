use super::{ACTIONS, App, Input, Modal};
use crate::storage::SourceKind;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, Tabs, Wrap},
};
use unicode_width::UnicodeWidthStr;

fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}
fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}
fn selected() -> Style {
    bold().add_modifier(Modifier::REVERSED)
}
fn block(title: impl Into<Line<'static>>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_style(bold())
}
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
fn multiline(text: &str) -> String {
    text.lines().map(clean).collect::<Vec<_>>().join("\n")
}
fn value(text: &str) -> String {
    if text.is_empty() {
        "unknown".into()
    } else {
        clean(text)
    }
}

pub(super) fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    // No fixed RGB or ANSI palette. Inherit the terminal's foreground/background.
    frame.render_widget(
        Block::default().style(Style::default().fg(Color::Reset).bg(Color::Reset)),
        area,
    );
    if area.width < 64 || area.height < 22 {
        frame.render_widget(
            Paragraph::new(
                "Minohomo\n\nResize to at least 64 columns × 22 rows.\nq / Ctrl-C quits.",
            )
            .block(block("Terminal too small"))
            .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if app.status.api_error.is_some() { 8 } else { 7 }),
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(if app.busy.is_some() || !app.notice.is_empty() {
                3
            } else {
                1
            }),
        ])
        .split(area);
    let status = &app.status;
    let mut header = vec![
        Line::from(vec![
            Span::styled("Mihomo  ", bold()),
            Span::raw(format!(
                "{}   Mode: {}   Tray preference: {}",
                value(&status.service),
                value(&status.mode),
                if status.tray_enabled { "on" } else { "off" }
            )),
        ]),
        Line::from(format!(
            "{}: {}",
            if status.group.is_empty() {
                "Proxy"
            } else {
                &status.group
            },
            value(&status.current)
        )),
        Line::from(format!(
            "Sources: {}   Geosite: {}   Profile: {}",
            status.sources.len(),
            value(&status.geosite),
            clean(&app.root.display().to_string())
        )),
    ];
    if let Some(error) = &status.api_error {
        header.push(Line::styled(
            format!("API offline: {}", clean(error)),
            dim(),
        ));
    }
    header.extend([
        Line::styled(
            "m mode  x proxy  s service  t tray  F5 refresh  ? help",
            dim(),
        ),
        Line::styled("c config  r rules  Tab panels  q quit", dim()),
    ]);
    frame.render_widget(
        Paragraph::new(header).block(block(format!(" minohomo {} ", env!("CARGO_PKG_VERSION")))),
        sections[0],
    );
    frame.render_widget(
        Tabs::new(["1 Sources", "2 Network", "3 Actions"])
            .select(app.tab)
            .highlight_style(selected())
            .divider("  |  "),
        sections[1],
    );
    match app.tab {
        0 => sources(frame, app, sections[2]),
        1 => network(frame, app, sections[2]),
        _ => actions(frame, app, sections[2]),
    }
    let status_line = if let Some(label) = app.busy {
        let spinner = ["|", "/", "-", "\\"][(app.tick.elapsed().as_millis() / 150) as usize % 4];
        format!("{spinner} {label}...")
    } else {
        format!(
            "{}{}",
            if app.failed { "ERROR: " } else { "" },
            clean(&app.notice)
        )
    };
    let hint = match app.tab {
        0 => "a add  n rename  d remove  u update  U update all",
        1 => "p ping all  i IP/location lookup  Up/Down scroll delays",
        _ => "Up/Down select  Enter run  PgUp/PgDn scroll",
    };
    let footer = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(sections[3]);
    frame.render_widget(
        Paragraph::new(status_line)
            .style(if app.failed { bold() } else { Style::default() })
            .wrap(Wrap { trim: false }),
        footer[0],
    );
    frame.render_widget(Paragraph::new(hint).style(dim()), footer[1]);
    if let Some(modal) = app.modal.as_mut() {
        draw_modal(frame, modal);
    }
}
fn sources(frame: &mut Frame, app: &mut App, area: Rect) {
    let sections = Layout::vertical([Constraint::Min(3), Constraint::Length(5)]).split(area);
    let rows = app.status.sources.iter().map(|source| {
        Row::new(vec![
            Cell::from(clean(source.name.as_deref().unwrap_or("unnamed"))),
            Cell::from(if source.kind == SourceKind::Subscription {
                "sub"
            } else {
                "proxy"
            }),
            Cell::from(source.proxies.len().to_string()),
            Cell::from(source.updated_at.map_or_else(
                || "never".into(),
                |date| {
                    date.with_timezone(&chrono::Local)
                        .format("%m-%d %H:%M")
                        .to_string()
                },
            )),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Min(12),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Length(12),
        ],
    )
    .header(
        Row::new(["Source name", "Type", "Nodes", "Updated"])
            .style(bold())
            .bottom_margin(1),
    )
    .block(block(" Saved sources "))
    .row_highlight_style(selected())
    .highlight_symbol("> ");
    frame.render_stateful_widget(table, sections[0], &mut app.sources);
    let text = if app.status.sources.is_empty() {
        "No saved sources. Press a to add a subscription URL or direct proxy link.\nThen press U to fetch, validate, and deploy. Geosite download-only is in Actions.\nUpdates use sudo; the dashboard restores your terminal for password prompts.".into()
    } else if let Some(source) = app
        .sources
        .selected()
        .and_then(|i| app.status.sources.get(i))
    {
        let nodes = source
            .proxies
            .iter()
            .take(4)
            .filter_map(|proxy| proxy["name"].as_str())
            .map(clean)
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{} cached nodes: {}{}\nLinks and credentials stay hidden. Add/remove changes require Update or Apply cache.\nActions > Apply cached sources to deploy edits",
            source.proxies.len(),
            if nodes.is_empty() { "none" } else { &nodes },
            if source.proxies.len() > 4 {
                ", ..."
            } else {
                ""
            }
        )
    } else {
        "Select a source with Up/Down.".into()
    };
    frame.render_widget(
        Paragraph::new(text)
            .block(block(" Selected source / next steps "))
            .wrap(Wrap { trim: false }),
        sections[1],
    );
}
fn network(frame: &mut Frame, app: &mut App, area: Rect) {
    let sections = Layout::vertical([Constraint::Length(8), Constraint::Min(3)]).split(area);
    frame.render_widget(
        Paragraph::new(multiline(&app.ip))
            .block(block(
                " Public IP + location | i refresh (external request) ",
            ))
            .wrap(Wrap { trim: false }),
        sections[0],
    );
    let rows = app.pings.iter().map(|(name, delay)| {
        Row::new([
            clean(name),
            delay.map_or_else(|| "failed".into(), |ms| format!("{ms} ms")),
        ])
    });
    let table = Table::new(rows, [Constraint::Min(15), Constraint::Length(12)])
        .header(Row::new(["API proxy / group", "Delay"]).style(bold()))
        .block(block(if app.pings.is_empty() {
            " Delay test | p to run | fastest first "
        } else {
            " Delay results | Up/Down scroll | p rerun "
        }))
        .row_highlight_style(selected())
        .highlight_symbol("> ");
    frame.render_stateful_widget(table, sections[1], &mut app.ping_state);
}
fn actions(frame: &mut Frame, app: &mut App, area: Rect) {
    let sections =
        Layout::horizontal([Constraint::Percentage(56), Constraint::Percentage(44)]).split(area);
    let list = List::new(ACTIONS.iter().map(|(_, name, _)| ListItem::new(*name)))
        .block(block(" Actions "))
        .highlight_style(selected())
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, sections[0], &mut app.actions);
    let (_, title, description) = ACTIONS[app.actions.selected().unwrap_or(0)];
    frame.render_widget(Paragraph::new(vec![Line::styled(title, bold()), Line::default(), Line::from(description), Line::default(), Line::from("Enter runs the selected action."), Line::default(), Line::styled("Terminal colors are inherited. Selected rows use reverse video, not a fixed theme.", dim())])
        .block(block(" What this does ")).wrap(Wrap { trim: false }), sections[1]);
}
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(4));
    let height = height.min(area.height.saturating_sub(2));
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}
fn input(frame: &mut Frame, area: Rect, label: &str, input: &Input, focused: bool) {
    let available = area.width.saturating_sub(2) as usize;
    let cursor = UnicodeWidthStr::width(&input.text[..input.cursor]);
    let scroll = cursor.saturating_sub(available.saturating_sub(1));
    frame.render_widget(
        Paragraph::new(input.text.as_str())
            .scroll((0, scroll.min(u16::MAX as usize) as u16))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(label.to_owned())
                    .border_style(if focused { bold() } else { dim() }),
            ),
        area,
    );
    if focused && area.width > 2 && area.height > 2 {
        frame.set_cursor_position(Position::new(
            area.x + 1 + (cursor - scroll) as u16,
            area.y + 1,
        ));
    }
}
fn draw_modal(frame: &mut Frame, modal: &mut Modal) {
    match modal {
        Modal::Add(form) => {
            let area = centered(frame.area(), 88, 17);
            frame.render_widget(Clear, area);
            frame.render_widget(block(" Add source | Esc cancel "), area);
            let inner = Block::default().borders(Borders::ALL).inner(area);
            let parts = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(2),
                Constraint::Min(2),
                Constraint::Length(2),
            ])
            .split(inner);
            frame.render_widget(
                Paragraph::new("Tab / Shift-Tab moves fields. Paste links. Enter saves."),
                parts[0],
            );
            input(
                frame,
                parts[1],
                "Name (optional; blank generates a name)",
                &form.name,
                form.field == 0,
            );
            input(
                frame,
                parts[2],
                "Subscription URL or proxy link",
                &form.link,
                form.field == 1,
            );
            frame.render_widget(
                Paragraph::new(format!(
                    "[{}] HTTP(S) link is a proxy endpoint, not a subscription",
                    if form.direct { "x" } else { " " }
                ))
                .style(if form.field == 2 {
                    selected()
                } else {
                    Style::default()
                })
                .wrap(Wrap { trim: false }),
                parts[3],
            );
            frame.render_widget(
                Paragraph::new(clean(&form.error))
                    .style(bold())
                    .wrap(Wrap { trim: false }),
                parts[4],
            );
            frame.render_widget(Paragraph::new("Space toggles checkbox. Ctrl-U clears a field.\nSave does not fetch or deploy; use Update afterward.").style(dim()), parts[5]);
        }
        Modal::Rename(form) => {
            let area = centered(frame.area(), 80, 12);
            frame.render_widget(Clear, area);
            frame.render_widget(block(" Rename source | Esc cancel "), area);
            let inner = Block::default().borders(Borders::ALL).inner(area);
            let parts = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(3),
                Constraint::Min(2),
                Constraint::Length(3),
            ])
            .split(inner);
            frame.render_widget(
                Paragraph::new(format!("Current name: {}", clean(&form.old_name))),
                parts[0],
            );
            input(frame, parts[1], "New unique source name", &form.name, true);
            frame.render_widget(
                Paragraph::new(clean(&form.error))
                    .style(bold())
                    .wrap(Wrap { trim: false }),
                parts[2],
            );
            frame.render_widget(
                Paragraph::new(
                    "Enter saves. Ctrl-U clears. Esc cancels.
Cache and timestamps stay unchanged.
Apply cached sources to update generated subscription names.",
                )
                .style(dim()),
                parts[3],
            );
        }
        Modal::Confirm { description, .. } => {
            let area = centered(frame.area(), 78, 10);
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(format!(
                    "{}\n\ny Confirm    n / Enter / Esc Cancel",
                    multiline(description)
                ))
                .block(block(" Confirm action "))
                .wrap(Wrap { trim: false }),
                area,
            );
        }
        Modal::Choice(choice) => {
            let area = centered(frame.area(), 82, 19);
            frame.render_widget(Clear, area);
            frame.render_widget(block(format!(" {} ", clean(&choice.title))), area);
            let inner = Block::default().borders(Borders::ALL).inner(area);
            let parts = Layout::vertical([
                Constraint::Length(3),
                Constraint::Min(2),
                Constraint::Length(1),
            ])
            .split(inner);
            input(frame, parts[0], "Type to filter", &choice.filter, true);
            let visible = choice.visible();
            let list = List::new(
                visible
                    .iter()
                    .map(|i| ListItem::new(clean(&choice.options[*i]))),
            )
            .highlight_style(selected())
            .highlight_symbol("> ");
            frame.render_stateful_widget(list, parts[1], &mut choice.state);
            frame.render_widget(
                Paragraph::new("Up/Down choose  Enter apply  Esc cancel").style(dim()),
                parts[2],
            );
        }
        Modal::Help { scroll } => {
            let area = centered(frame.area(), 88, 23);
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled("Navigation", bold()),
                    Line::from("1/2/3 or Tab: panel | Up/Down or j/k: select | PgUp/PgDn: page"),
                    Line::from("Enter: run action | q / Ctrl-C: quit | Esc: cancel dialog"),
                    Line::default(),
                    Line::styled("Sources + configuration", bold()),
                    Line::from("a add | n rename (Sources) | d remove | u update selected | U all"),
                    Line::from("c config.yaml | r rules.yaml | g geosite update + reload"),
                    Line::from("Actions: apply cached sources, geosite download without reload"),
                    Line::default(),
                    Line::styled("Mihomo + network", bold()),
                    Line::from("m mode | x proxy | s start/stop/restart | t toggle tray"),
                    Line::from("p ping all proxies | i IP/location via ipwho.is | F5 refresh"),
                    Line::default(),
                    Line::from("Updates and service control hand off to your terminal for sudo."),
                    Line::from("Runtime mode/proxy changes use the API. Config edits need Apply."),
                    Line::from("IP lookup is on demand. The provider sees your outbound IP."),
                    Line::from(
                        "Default terminal colors; reverse-video selection. No fixed palette.",
                    ),
                    Line::default(),
                    Line::styled(
                        "Enter / Esc / ? closes help. All commands are in Actions.",
                        dim(),
                    ),
                ])
                .scroll((*scroll, 0))
                .block(block(" Help | Up/Down scroll | Esc close "))
                .wrap(Wrap { trim: false }),
                area,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    #[test]
    fn renders_all_tabs_and_dialogs_at_supported_sizes() {
        let mut app = App::test_app();
        for (width, height) in [(100, 32), (64, 22), (30, 8)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            for tab in 0..3 {
                app.tab = tab;
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            }
            app.modal = Some(Modal::Add(Default::default()));
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let mut name = Input::default();
            name.insert("New source");
            app.modal = Some(Modal::Rename(super::super::RenameForm {
                old_name: "Old source".into(),
                name,
                error: "Name already in use".into(),
            }));
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            app.modal = Some(Modal::Help { scroll: 0 });
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            app.modal = None;
        }
    }
    #[test]
    fn strips_terminal_control_sequences_from_display_text() {
        assert!(!clean("node\u{1b}[2J\nname").contains('\u{1b}'));
        assert!(!clean("node\nname").contains('\n'));
    }
    #[test]
    fn dashboard_inherits_colors_and_hides_source_credentials() {
        let mut app = App::test_app();
        app.status.service = "active".into();
        app.status.mode = "rule".into();
        app.status.current = "Node1".into();
        app.status.sources.push(crate::storage::Source {
            id: 1,
            kind: SourceKind::Subscription,
            name: Some("Personal".into()),
            link: "https://private.test/sub?token=supersecret".into(),
            added_at: chrono::Utc::now(),
            updated_at: None,
            proxies: vec![],
        });
        let mut terminal = Terminal::new(TestBackend::new(100, 32)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let text = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Personal"));
        assert!(text.contains("Node1"));
        assert!(!text.contains("supersecret"));
        assert!(!text.contains("private.test"));
        for cell in buffer.content() {
            assert_eq!(cell.fg, Color::Reset);
            assert_eq!(cell.bg, Color::Reset);
        }
        app.choose(
            "Select proxy",
            vec!["Node1".into()],
            vec![super::super::ChoiceValue::Proxy(
                "PROXY".into(),
                "Node1".into(),
            )],
        );
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        app.modal = Some(Modal::Confirm {
            description: "Remove source?".into(),
            args: vec![],
        });
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    }
}
