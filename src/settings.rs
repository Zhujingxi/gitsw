use crate::{
    config::{Github, Profile, Store},
    system::Discovery,
    tui::{self, Row, Terminal},
};
use anyhow::{Result, bail};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    style::Color,
    terminal,
};
use unicode_width::UnicodeWidthChar;

pub fn run(store: &mut Store, discovery: &Discovery) -> Result<()> {
    tui::with_terminal(|terminal| {
        let mut notice = String::new();
        loop {
            let mut entries: Vec<String> = store
                .profiles
                .iter()
                .map(|p| format!("{}  {}", p.label, p.email))
                .collect();
            let new = entries.len();
            entries.push("New account".into());
            let restore = if store.hidden_profiles.is_empty() {
                None
            } else {
                let index = entries.len();
                entries.push("Restore hidden account".into());
                Some(index)
            };
            let done = entries.len();
            entries.push("Done".into());
            let title = if notice.is_empty() {
                "gitsw settings".into()
            } else {
                format!("gitsw settings | {notice}")
            };
            let Some(index) = choose(terminal, &title, &entries, 0)? else {
                break;
            };
            if index == done {
                break;
            }
            if index == new {
                notice = form(
                    terminal,
                    store,
                    discovery,
                    None,
                    Profile {
                        label: String::new(),
                        name: String::new(),
                        email: String::new(),
                        github: None,
                    },
                )?;
            } else if Some(index) == restore {
                let names: Vec<_> = store
                    .hidden_profiles
                    .iter()
                    .map(|p| p.label.clone())
                    .collect();
                if let Some(index) = choose(terminal, "Restore account", &names, 0)? {
                    notice = form(
                        terminal,
                        store,
                        discovery,
                        None,
                        store.hidden_profiles[index].clone(),
                    )?;
                }
            } else {
                notice = form(
                    terminal,
                    store,
                    discovery,
                    Some(index),
                    store.profiles[index].clone(),
                )?;
            }
        }
        terminal.finish("Settings closed.".into())
    })
}

fn key() -> Result<Option<KeyEvent>> {
    match event::read()? {
        Event::Key(key) if key.kind == KeyEventKind::Press => Ok(Some(key)),
        _ => Ok(None),
    }
}
fn cancel(key: KeyEvent) -> bool {
    key.code == KeyCode::Esc
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}
fn choose(
    terminal: &mut Terminal,
    title: &str,
    entries: &[String],
    mut selected: usize,
) -> Result<Option<usize>> {
    loop {
        let (width, height) = terminal::size()?;
        let mut rows = vec![Row::text(title, Color::Cyan)];
        if width < 20 || height < 5 {
            rows.push(Row::text("Resize terminal; Esc exits.", Color::Yellow));
        } else {
            let capacity = usize::from(height.saturating_sub(3)).min(8);
            let start = selected.saturating_sub(capacity.saturating_sub(1));
            for (index, entry) in entries.iter().enumerate().skip(start).take(capacity) {
                rows.push(Row::text(
                    format!("{} {entry}", if index == selected { ">" } else { " " }),
                    if index == selected {
                        Color::Cyan
                    } else {
                        Color::White
                    },
                ));
            }
            rows.push(Row::text(
                "↑/↓ or j/k  Enter choose  Esc/q back",
                Color::DarkGrey,
            ));
        }
        terminal.draw(&rows, width, height)?;
        let Some(key) = key()? else {
            continue;
        };
        if cancel(key) || key.code == KeyCode::Char('q') {
            return Ok(None);
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                selected = (selected + entries.len() - 1) % entries.len()
            }
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1) % entries.len(),
            KeyCode::Home => selected = 0,
            KeyCode::End => selected = entries.len() - 1,
            KeyCode::Enter if width >= 20 && height >= 5 => return Ok(Some(selected)),
            _ => {}
        }
    }
}

fn input(terminal: &mut Terminal, title: &str, initial: &str) -> Result<Option<String>> {
    let mut value: Vec<char> = initial.chars().collect();
    let mut cursor = value.len();
    loop {
        let (width, height) = terminal::size()?;
        let mut start = cursor;
        let mut columns = 1;
        let budget = usize::from(width.saturating_sub(4));
        while start > 0 && columns + value[start - 1].width().unwrap_or(0) <= budget {
            start -= 1;
            columns += value[start].width().unwrap_or(0);
        }
        let before: String = value[start..cursor].iter().collect();
        let after: String = value[cursor..].iter().collect();
        terminal.draw(
            &[
                Row::text(title, Color::Cyan),
                Row::text(format!("> {before}▏{after}"), Color::White),
                Row::text("Enter confirm  Esc cancel  Ctrl-U clear", Color::DarkGrey),
            ],
            width,
            height,
        )?;
        let Some(key) = key()? else {
            continue;
        };
        if cancel(key) {
            return Ok(None);
        }
        match key.code {
            KeyCode::Enter => return Ok(Some(value.iter().collect::<String>().trim().into())),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                value.clear();
                cursor = 0;
            }
            KeyCode::Char(c)
                if !c.is_control()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                value.insert(cursor, c);
                cursor += 1;
            }
            KeyCode::Backspace if cursor > 0 => {
                cursor -= 1;
                value.remove(cursor);
            }
            KeyCode::Delete if cursor < value.len() => {
                value.remove(cursor);
            }
            KeyCode::Left if cursor > 0 => cursor -= 1,
            KeyCode::Right if cursor < value.len() => cursor += 1,
            KeyCode::Home => cursor = 0,
            KeyCode::End => cursor = value.len(),
            _ => {}
        }
    }
}

fn github(
    terminal: &mut Terminal,
    discovery: &Discovery,
    current: Option<&Github>,
) -> Result<Option<Option<Github>>> {
    let mut links = vec![None];
    for (host, accounts) in &discovery.hosts {
        for account in accounts {
            if !account.token_source.to_ascii_lowercase().contains("token") {
                links.push(Some(Github {
                    host: host.clone(),
                    user: account.login.clone(),
                }));
            }
        }
    }
    if let Some(current) = current
        && !links.iter().any(|link| link.as_ref() == Some(current))
    {
        links.push(Some(current.clone()));
    }
    let mut entries: Vec<_> = links
        .iter()
        .map(|link| {
            link.as_ref()
                .map(|g| format!("{}/{}", g.host, g.user))
                .unwrap_or_else(|| "Git only".into())
        })
        .collect();
    entries.push("Other account...".into());
    let selected = links
        .iter()
        .position(|link| link.as_ref() == current)
        .unwrap_or(0);
    let Some(index) = choose(terminal, "GitHub account", &entries, selected)? else {
        return Ok(None);
    };
    if index < links.len() {
        return Ok(Some(links[index].clone()));
    }
    let Some(host) = input(terminal, "GitHub hostname", "github.com")? else {
        return Ok(None);
    };
    let Some(user) = input(
        terminal,
        "GitHub username (log in with gh auth login first)",
        "",
    )?
    else {
        return Ok(None);
    };
    Ok(Some(Some(Github { host, user })))
}

fn save_profile(store: &mut Store, index: Option<usize>, profile: Profile) -> Result<()> {
    profile.validate()?;
    if store
        .profiles
        .iter()
        .enumerate()
        .any(|(i, p)| Some(i) != index && p.label.eq_ignore_ascii_case(&profile.label))
    {
        bail!("That account label already exists.");
    }
    let mut next = store.clone();
    next.hidden_profiles.retain(|p| {
        !(p.name == profile.name && p.email == profile.email)
            && !(p.github.is_some() && p.github == profile.github)
    });
    if let Some(index) = index {
        next.profiles[index] = profile;
    } else {
        next.profiles.push(profile);
    }
    next.save()?;
    *store = next;
    Ok(())
}

fn form(
    terminal: &mut Terminal,
    store: &mut Store,
    discovery: &Discovery,
    index: Option<usize>,
    mut profile: Profile,
) -> Result<String> {
    let mut selected = 0;
    let mut error = String::new();
    loop {
        let mut entries = vec![
            format!("Label: {}", profile.label),
            format!("Commit name: {}", profile.name),
            format!("Commit email: {}", profile.email),
            format!(
                "GitHub: {}",
                profile
                    .github
                    .as_ref()
                    .map(|g| format!("{}/{}", g.host, g.user))
                    .unwrap_or_else(|| "Git only".into())
            ),
            "Save".into(),
        ];
        let delete = index.map(|_| {
            let index = entries.len();
            entries.push("Delete profile".into());
            index
        });
        let back = entries.len();
        entries.push("Back (discard changes)".into());
        let title = if error.is_empty() {
            "Account settings".into()
        } else {
            error.clone()
        };
        let Some(choice) = choose(terminal, &title, &entries, selected)? else {
            return Ok(String::new());
        };
        selected = choice;
        error.clear();
        match choice {
            0 => {
                if let Some(value) = input(terminal, "Account label", &profile.label)? {
                    profile.label = value;
                }
            }
            1 => {
                if let Some(value) = input(terminal, "Commit name", &profile.name)? {
                    profile.name = value;
                }
            }
            2 => {
                if let Some(value) = input(terminal, "Commit email", &profile.email)? {
                    profile.email = value;
                }
            }
            3 => {
                if let Some(link) = github(terminal, discovery, profile.github.as_ref())? {
                    profile.github = link;
                }
            }
            4 => match save_profile(store, index, profile.clone()) {
                Ok(()) => return Ok(format!("Saved {}", profile.label)),
                Err(e) => error = e.to_string(),
            },
            _ if Some(choice) == delete => {
                let confirmation = ["Keep profile".into(), "Delete profile".into()];
                if choose(
                    terminal,
                    "Delete from gitsw? GitHub stays logged in.",
                    &confirmation,
                    0,
                )? == Some(1)
                {
                    let mut next = store.clone();
                    let old = next
                        .profiles
                        .remove(index.expect("delete is only shown for saved profiles"));
                    next.hidden_profiles.push(old.clone());
                    next.save()?;
                    *store = next;
                    return Ok(format!("Deleted {}", old.label));
                }
            }
            _ if choice == back => return Ok(String::new()),
            _ => {}
        }
    }
}
