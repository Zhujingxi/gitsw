use crate::{
    config::Profile,
    system::{Discovery, GitIdentity},
};
use anyhow::Result;
use crossterm::{
    cursor::{Hide, MoveToColumn, MoveUp, Show},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute, queue,
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{self, Clear, ClearType},
};
use std::io::{self, Write};
use unicode_width::UnicodeWidthChar;

pub(crate) struct Row {
    spans: Vec<(String, Color)>,
}
impl Row {
    pub(crate) fn text(text: impl Into<String>, color: Color) -> Self {
        Self {
            spans: vec![(text.into(), color)],
        }
    }

    fn account(text: &str, color: Color, width: usize, commit: bool, github: bool) -> Self {
        let mut columns = 0;
        let mut text: String = text
            .chars()
            .filter(|c| !c.is_control())
            .take_while(|c| {
                columns += c.width().unwrap_or(0);
                columns <= width
            })
            .collect();
        let used: usize = text.chars().map(|c| c.width().unwrap_or(0)).sum();
        text.push_str(&" ".repeat(width.saturating_sub(used) + 1));
        Self {
            spans: vec![
                (text, color),
                (if commit { "┃" } else { " " }.into(), Color::Yellow),
                (if github { "┃" } else { " " }.into(), Color::Blue),
            ],
        }
    }
}

pub(crate) struct Terminal {
    lines: u16,
    finished: bool,
}
impl Terminal {
    fn enter() -> Result<Self> {
        terminal::enable_raw_mode()?;
        let guard = Self {
            lines: 0,
            finished: false,
        };
        execute!(io::stdout(), Hide)?;
        Ok(guard)
    }

    pub(crate) fn draw(&mut self, rows: &[Row], width: u16, height: u16) -> Result<()> {
        let mut out = io::stdout();
        // Only redraw the lines owned by the picker, relative to the cursor below it.
        let previous = self.lines.min(height.saturating_sub(1));
        if previous > 0 {
            queue!(out, MoveUp(previous))?;
        }
        queue!(out, MoveToColumn(0))?;
        let count = rows.len() as u16;
        for index in 0..previous.max(count) {
            queue!(out, Clear(ClearType::CurrentLine))?;
            if let Some(row) = rows.get(usize::from(index)) {
                let mut columns = 0;
                for (text, color) in &row.spans {
                    let text: String = text
                        .chars()
                        .filter(|c| !c.is_control())
                        .take_while(|c| {
                            columns += c.width().unwrap_or(0);
                            columns <= usize::from(width.saturating_sub(1))
                        })
                        .collect();
                    queue!(out, SetForegroundColor(*color), Print(text), ResetColor)?;
                }
            }
            queue!(out, Print("\r\n"))?;
        }
        if previous > count {
            queue!(out, MoveUp(previous - count), MoveToColumn(0))?;
        }
        out.flush()?;
        self.lines = count;
        Ok(())
    }

    pub(crate) fn finish(&mut self, text: String) -> Result<()> {
        let (width, height) = terminal::size()?;
        self.draw(&[Row::text(text, Color::Cyan)], width, height)?;
        self.finished = true;
        Ok(())
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        if !self.finished
            && !std::thread::panicking()
            && let Ok((width, height)) = terminal::size()
        {
            let _ = self.draw(&[], width, height);
        }
        let _ = execute!(io::stdout(), ResetColor, Show);
        let _ = terminal::disable_raw_mode();
    }
}

pub fn pick(
    profiles: &[Profile],
    current: &GitIdentity,
    discovery: &Discovery,
) -> Result<Option<usize>> {
    with_terminal(|terminal| {
        let result = picker_loop(terminal, profiles, current, discovery)?;
        terminal.finish(
            result
                .map(|i| format!("Selected: {}", profiles[i].label))
                .unwrap_or_else(|| "Cancelled.".into()),
        )?;
        Ok(result)
    })
}

pub(crate) fn with_terminal<T>(dialog: impl FnOnce(&mut Terminal) -> Result<T>) -> Result<T> {
    let mut terminal = Terminal::enter()?;
    let old_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|info| {
        let _ = execute!(io::stdout(), ResetColor, Show);
        let _ = terminal::disable_raw_mode();
        eprintln!("gitsw: {info}");
    }));
    let result = dialog(&mut terminal);
    std::panic::set_hook(old_hook);
    result
}

fn picker_loop(
    terminal: &mut Terminal,
    profiles: &[Profile],
    current: &GitIdentity,
    discovery: &Discovery,
) -> Result<Option<usize>> {
    let mut selection = profiles
        .iter()
        .position(|p| current.matches(p))
        .unwrap_or(0);
    let mut filter = String::new();
    let mut searching = false;
    loop {
        let visible: Vec<usize> = profiles
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                format!(
                    "{} {} {} {}",
                    p.label,
                    p.name,
                    p.email,
                    p.github
                        .as_ref()
                        .map(|g| format!("{}/{}", g.host, g.user))
                        .unwrap_or_default()
                )
                .to_lowercase()
                .contains(&filter.to_lowercase())
            })
            .map(|(i, _)| i)
            .collect();
        if !visible.is_empty() && !visible.contains(&selection) {
            selection = visible[0];
        }
        let (width, height) = terminal::size()?;
        let mut rows = Vec::new();
        if height < 5 || width < 20 {
            rows.push(Row::text("Resize terminal; q quits.", Color::Yellow));
        } else {
            rows.push(Row {
                spans: vec![
                    ("Choose identity  ".into(), Color::Cyan),
                    ("┃ Commit".into(), Color::Yellow),
                    ("  ".into(), Color::White),
                    ("┃ GitHub".into(), Color::Blue),
                ],
            });
            let capacity = usize::from(height.saturating_sub(4)).min(8);
            let selected_pos = visible.iter().position(|i| *i == selection).unwrap_or(0);
            let start = selected_pos.saturating_sub(capacity.saturating_sub(1));
            let label_width = profiles
                .iter()
                .map(|p| {
                    p.label
                        .chars()
                        .filter(|c| !c.is_control())
                        .map(|c| c.width().unwrap_or(0))
                        .sum::<usize>()
                })
                .max()
                .unwrap_or(0);
            let account_text = |index: usize| {
                let p = &profiles[index];
                let used: usize = p
                    .label
                    .chars()
                    .filter(|c| !c.is_control())
                    .map(|c| c.width().unwrap_or(0))
                    .sum();
                format!(
                    "{} {}{}  {}",
                    if index == selection { ">" } else { " " },
                    p.label,
                    " ".repeat(label_width.saturating_sub(used)),
                    p.email
                )
            };
            // Reserve two indicator columns even when either state is inactive.
            // Their position depends on all profiles, so searching cannot shift them.
            let account_width = (0..profiles.len())
                .map(|i| {
                    account_text(i)
                        .chars()
                        .filter(|c| !c.is_control())
                        .map(|c| c.width().unwrap_or(0))
                        .sum::<usize>()
                })
                .max()
                .unwrap_or(0)
                .min(usize::from(width.saturating_sub(4)));
            for &index in visible.iter().skip(start).take(capacity) {
                let p = &profiles[index];
                rows.push(Row::account(
                    &account_text(index),
                    if index == selection {
                        Color::Cyan
                    } else {
                        Color::White
                    },
                    account_width,
                    current.matches(p),
                    p.github.as_ref().is_some_and(|g| discovery.is_active(g)),
                ));
            }
            if visible.is_empty() {
                rows.push(Row::text("No matching profiles", Color::Yellow));
            }
            if let Some(&index) = visible.iter().find(|&&i| i == selection) {
                let p = &profiles[index];
                let account = p
                    .github
                    .as_ref()
                    .map(|g| format!("gh: {}/{}", g.host, g.user))
                    .unwrap_or_else(|| "Git only".into());
                let detail = if p.email.is_empty() {
                    "Email required: gitsw -setting".into()
                } else if let Some(warning) = discovery.warnings.first() {
                    warning.clone()
                } else {
                    format!("{} | {account}", p.name)
                };
                rows.push(Row::text(detail, Color::DarkGrey));
            }
            rows.push(Row::text(
                if searching {
                    format!("Search: {filter}_  Enter finishes search")
                } else if !filter.is_empty() {
                    format!("/{filter}  Enter switch  / search  Esc clear  q quit")
                } else {
                    "↑/↓ or j/k  Enter switch  / search  q/Esc quit".into()
                },
                Color::Cyan,
            ));
        }
        terminal.draw(&rows, width, height)?;
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                return Ok(None);
            }
            if searching {
                match key.code {
                    KeyCode::Esc => {
                        searching = false;
                        filter.clear();
                    }
                    KeyCode::Enter => searching = false,
                    KeyCode::Backspace => {
                        filter.pop();
                    }
                    KeyCode::Char(c)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        filter.push(c)
                    }
                    _ => {}
                }
                continue;
            }
            match key.code {
                KeyCode::Esc if !filter.is_empty() => filter.clear(),
                KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
                KeyCode::Char('/') => searching = true,
                KeyCode::Enter if height >= 5 && width >= 20 && !visible.is_empty() => {
                    return Ok(Some(selection));
                }
                KeyCode::Down | KeyCode::Char('j') if !visible.is_empty() => {
                    let pos = visible.iter().position(|i| *i == selection).unwrap_or(0);
                    selection = visible[(pos + 1) % visible.len()];
                }
                KeyCode::Up | KeyCode::Char('k') if !visible.is_empty() => {
                    let pos = visible.iter().position(|i| *i == selection).unwrap_or(0);
                    selection = visible[(pos + visible.len() - 1) % visible.len()];
                }
                KeyCode::Home if !visible.is_empty() => selection = visible[0],
                KeyCode::End if !visible.is_empty() => selection = *visible.last().unwrap(),
                _ => {}
            }
        }
    }
}
