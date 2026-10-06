mod config;
mod settings;
mod system;
mod tui;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use config::{Profile, Store};
use std::io::{self, IsTerminal};
use system::{Discovery, GitIdentity};

#[derive(Parser)]
#[command(
    version,
    about,
    long_about = "Switch global Git name/email and the matching GitHub CLI account.\nRun without arguments to open the account picker."
)]
struct Cli {
    /// Configure accounts interactively (also accepts -setting)
    #[arg(short = 's', long = "setting", visible_alias = "settings")]
    setting: bool,
    #[command(subcommand)]
    command: Option<Action>,
}

#[derive(Subcommand)]
enum Action {
    /// Show saved identities and discovered GitHub CLI accounts
    List,
    /// Switch directly to a profile by label
    Use { label: String },
}

fn main() {
    if let Err(error) = run() {
        eprintln!("gitsw: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|arg| arg == "-setting") {
        args[1] = "--setting".into();
    }
    let cli = Cli::parse_from(args);
    if cli.setting && cli.command.is_some() {
        bail!("use gitsw -setting without a subcommand");
    }
    if cli.command.is_none() && (!io::stdin().is_terminal() || !io::stdout().is_terminal()) {
        let menu = if cli.setting {
            "settings menu"
        } else {
            "picker"
        };
        bail!("the {menu} needs a terminal. Use `gitsw list` or `gitsw use LABEL` instead");
    }
    let mut store = Store::load()?;
    if cli.command.is_none() {
        eprintln!("Loading Git identities and GitHub accounts…");
    }
    let discovery = system::discover();
    let current = system::git_identity()?;
    if system::merge_profiles(&mut store, &discovery, &current)? {
        store.save()?;
    }
    if cli.setting {
        return settings::run(&mut store, &discovery);
    }
    if let Some(Action::List) = cli.command {
        print_list(&store.profiles, &current, &discovery);
        return Ok(());
    }
    if store.profiles.is_empty() {
        bail!("no identities found. Use `gitsw -setting` or `gh auth login`");
    }
    let selection = match cli.command {
        Some(Action::Use { label }) => Some(
            store
                .find(&label)
                .with_context(|| format!("unknown profile {label:?}; run `gitsw list`"))?,
        ),
        None => tui::pick(&store.profiles, &current, &discovery)?,
        _ => unreachable!(),
    };
    if let Some(index) = selection {
        let profile = &store.profiles[index];
        system::switch(profile, &discovery)?;
        println!(
            "Switched to {}\n  Git: {} <{}>",
            profile.label, profile.name, profile.email
        );
        if let Some(gh) = &profile.github {
            println!(
                "  gh:  {}/{} (HTTPS Git uses gh credentials)",
                gh.host, gh.user
            );
        }
        let effective = system::effective_identity()?;
        if effective.name.as_deref() != Some(&profile.name)
            || effective.email.as_deref() != Some(&profile.email)
        {
            println!("Note: this repository or an included config overrides the global identity.");
        }
        for key in [
            "GIT_AUTHOR_NAME",
            "GIT_AUTHOR_EMAIL",
            "GIT_COMMITTER_NAME",
            "GIT_COMMITTER_EMAIL",
        ] {
            if std::env::var_os(key).is_some() {
                println!("Note: {key} is set and can override commit identity.");
            }
        }
    }
    Ok(())
}

fn print_list(profiles: &[Profile], current: &GitIdentity, discovery: &Discovery) {
    println!(
        "Global Git: {} <{}>",
        current.name.as_deref().unwrap_or("unset"),
        current.email.as_deref().unwrap_or("unset")
    );
    for profile in profiles {
        let git_active = current.matches(profile);
        let gh_active = profile
            .github
            .as_ref()
            .is_some_and(|gh| discovery.is_active(gh));
        println!(
            "\n{}{}{}\n  {} <{}>\n  {}",
            profile.label,
            if git_active { " [Git active]" } else { "" },
            if gh_active { " [gh active]" } else { "" },
            profile.name,
            if profile.email.is_empty() {
                "email required: gitsw -setting"
            } else {
                &profile.email
            },
            profile
                .github
                .as_ref()
                .map(|gh| format!("gh: {}/{}", gh.host, gh.user))
                .unwrap_or_else(|| "Git only".into())
        );
    }
    for warning in &discovery.warnings {
        println!("\nNote: {warning}");
    }
}
