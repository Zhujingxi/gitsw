use crate::config::{self, Github, Profile, Store};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    env,
    ffi::OsStr,
    fs,
    io::{Read, Write},
    path::PathBuf,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, Default)]
pub struct GitIdentity {
    pub name: Option<String>,
    pub email: Option<String>,
}
impl GitIdentity {
    pub fn matches(&self, p: &Profile) -> bool {
        self.name.as_deref() == Some(&p.name) && self.email.as_deref() == Some(&p.email)
    }
}

#[derive(Debug, Deserialize)]
pub struct GhAccount {
    pub login: String,
    #[serde(default)]
    pub active: bool,
    #[serde(default, rename = "tokenSource")]
    pub token_source: String,
}
#[derive(Debug, Default)]
pub struct Discovery {
    pub hosts: BTreeMap<String, Vec<GhAccount>>,
    pub warnings: Vec<String>,
}
impl Discovery {
    pub fn is_active(&self, gh: &Github) -> bool {
        self.hosts.get(&gh.host).is_some_and(|accounts| {
            accounts
                .iter()
                .any(|a| a.active && a.login.eq_ignore_ascii_case(&gh.user))
        })
    }
}

// Drain both pipes concurrently, bound subprocess latency, and never print auth-status payloads.
fn output(command: &mut Command) -> Result<Output> {
    command
        .env("GH_PROMPT_DISABLED", "1")
        .env_remove("GH_DEBUG")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .context("could not start command; check that git/gh is installed")?;
    let mut stdout = child.stdout.take().context("missing stdout")?;
    let mut stderr = child.stderr.take().context("missing stderr")?;
    let out = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let err = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("command timed out after 20 seconds");
        }
        thread::sleep(Duration::from_millis(20));
    };
    Ok(Output {
        status,
        stdout: out
            .join()
            .map_err(|_| anyhow::anyhow!("stdout reader failed"))??,
        stderr: err
            .join()
            .map_err(|_| anyhow::anyhow!("stderr reader failed"))??,
    })
}
fn checked(command: &mut Command, description: &str) -> Result<Vec<u8>> {
    let result = output(command).with_context(|| description.to_string())?;
    if !result.status.success() {
        let detail: String = String::from_utf8_lossy(&result.stderr)
            .chars()
            .filter(|c| !c.is_control() || *c == '\n')
            .take(500)
            .collect();
        bail!("{description}: {}", detail.trim());
    }
    Ok(result.stdout)
}
fn git_value(key: &str, global: bool) -> Result<Option<String>> {
    let mut command = Command::new("git");
    command.arg("config");
    if global {
        command.arg("--global");
    }
    let result = output(command.args(["--get", key]))?;
    match result.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8(result.stdout)?
                .trim_end_matches(['\r', '\n'])
                .to_string(),
        )),
        Some(1) => Ok(None),
        _ => bail!("cannot read Git config {key}; check your Git config syntax"),
    }
}
pub fn git_identity() -> Result<GitIdentity> {
    Ok(GitIdentity {
        name: git_value("user.name", true)?,
        email: git_value("user.email", true)?,
    })
}
pub fn effective_identity() -> Result<GitIdentity> {
    Ok(GitIdentity {
        name: git_value("user.name", false)?,
        email: git_value("user.email", false)?,
    })
}

pub fn discover() -> Discovery {
    match discover_inner() {
        Ok(discovery) => discovery,
        Err(_) => Discovery { warnings: vec!["GitHub accounts unavailable. Git-only profiles still work; check `gh auth status` or `gh auth login`.".into()], ..Discovery::default() },
    }
}
fn discover_inner() -> Result<Discovery> {
    #[derive(Deserialize)]
    struct Status {
        hosts: BTreeMap<String, Vec<GhAccount>>,
    }
    let bytes = checked(
        Command::new("gh").args(["auth", "status", "--json", "hosts"]),
        "cannot discover gh accounts",
    )?;
    let status: Status =
        serde_json::from_slice(&bytes).context("cannot parse gh accounts; upgrade GitHub CLI")?;
    let mut warnings = Vec::new();
    for (host, accounts) in &status.hosts {
        if accounts
            .iter()
            .any(|a| a.token_source.to_ascii_lowercase().contains("token"))
        {
            warnings.push(format!("{host}: an environment token overrides saved gh accounts. Unset GH_TOKEN/GITHUB_TOKEN (or enterprise equivalents) before switching."));
        }
    }
    Ok(Discovery {
        hosts: status.hosts,
        warnings,
    })
}

pub fn merge_profiles(
    store: &mut Store,
    discovery: &Discovery,
    current: &GitIdentity,
) -> Result<bool> {
    let mut changed = !store.path.exists();
    for (host, accounts) in &discovery.hosts {
        for account in accounts {
            // Environment credentials cannot be selected with gh auth switch.
            if account.token_source.to_ascii_lowercase().contains("token") {
                continue;
            }
            let gh = Github {
                host: host.clone(),
                user: account.login.clone(),
            };
            if store
                .profiles
                .iter()
                .chain(&store.hidden_profiles)
                .any(|p| {
                    p.github.as_ref().is_some_and(|g| {
                        g.host == *host && g.user.eq_ignore_ascii_case(&account.login)
                    })
                })
            {
                continue;
            }
            let matches: Vec<_> = store
                .profiles
                .iter()
                .enumerate()
                .filter(|(_, p)| p.github.is_none() && p.name.eq_ignore_ascii_case(&account.login))
                .map(|(i, _)| i)
                .collect();
            if matches.len() == 1 {
                store.profiles[matches[0]].github = Some(gh);
            } else {
                let (name, email) =
                    public_identity(&gh).unwrap_or((account.login.clone(), String::new()));
                let label = unique_label(store, &account.login, host);
                store.profiles.push(Profile {
                    label,
                    name,
                    email,
                    github: Some(gh),
                });
            }
            changed = true;
        }
    }
    if let (Some(name), Some(email)) = (&current.name, &current.email)
        && !store
            .profiles
            .iter()
            .chain(&store.hidden_profiles)
            .any(|p| current.matches(p))
    {
        let label = unique_label(store, "Current Git", "local");
        store.profiles.push(Profile {
            label,
            name: name.clone(),
            email: email.clone(),
            github: None,
        });
        changed = true;
    }
    Ok(changed)
}
fn unique_label(store: &Store, base: &str, host: &str) -> String {
    if store.find(base).is_none() {
        return base.into();
    }
    let base = format!("{base}@{host}");
    if store.find(&base).is_none() {
        return base;
    }
    for n in 2.. {
        let label = format!("{base}-{n}");
        if store.find(&label).is_none() {
            return label;
        }
    }
    unreachable!()
}
fn public_identity(gh: &Github) -> Result<(String, String)> {
    #[derive(Deserialize)]
    struct User {
        login: String,
        id: u64,
        name: Option<String>,
        email: Option<String>,
    }
    let bytes = checked(
        Command::new("gh").args(["api", "--hostname", &gh.host, &format!("users/{}", gh.user)]),
        "cannot fetch public GitHub identity",
    )?;
    let user: User = serde_json::from_slice(&bytes)?;
    let email = user.email.filter(|s| !s.is_empty()).unwrap_or_else(|| {
        if gh.host == "github.com" {
            format!("{}+{}@users.noreply.github.com", user.id, user.login)
        } else {
            String::new()
        }
    });
    Ok((
        user.name.filter(|s| !s.is_empty()).unwrap_or(user.login),
        email,
    ))
}

fn global_config_path() -> Result<PathBuf> {
    let path = if let Some(path) = env::var_os("GIT_CONFIG_GLOBAL").filter(|s| !s.is_empty()) {
        let path = PathBuf::from(path);
        if let Ok(suffix) = path.strip_prefix("~") {
            config::home()?.join(suffix)
        } else {
            path
        }
    } else {
        let home_path = config::home()?.join(".gitconfig");
        let xdg_path = config::config_dir()?.join("git/config");
        if home_path.exists() || !xdg_path.exists() {
            home_path
        } else {
            xdg_path
        }
    };
    // Respect symlinked dotfiles by replacing their target, rather than the symlink itself.
    if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return fs::canonicalize(&path).context("cannot resolve global Git config symlink");
    }
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(env::current_dir()?.join(path))
    }
}
fn read_optional(path: &std::path::Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn gh_switch(gh: &Github) -> Result<()> {
    checked(
        Command::new("gh").args(["auth", "switch", "--hostname", &gh.host, "--user", &gh.user]),
        "cannot switch gh account",
    )?;
    Ok(())
}
fn token_override(host: &str) -> bool {
    let keys = if host == "github.com" || host.ends_with(".ghe.com") {
        ["GH_TOKEN", "GITHUB_TOKEN"]
    } else {
        ["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"]
    };
    keys.iter()
        .any(|key| env::var_os(key).is_some_and(|s| !s.is_empty()))
}

struct GitLock(PathBuf);
impl GitLock {
    fn acquire(path: &std::path::Path) -> Result<Self> {
        let mut lock = path.as_os_str().to_os_string();
        lock.push(".lock");
        let lock = PathBuf::from(lock);
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .with_context(|| {
                format!(
                    "cannot lock {}; another Git process may be editing it",
                    path.display()
                )
            })?;
        Ok(Self(lock))
    }
}
impl Drop for GitLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub fn switch(profile: &Profile, discovery: &Discovery) -> Result<()> {
    profile
        .validate()
        .context("incomplete profile; configure it with `gitsw -setting`")?;
    let previous = if let Some(gh) = &profile.github {
        if token_override(&gh.host) {
            bail!(
                "an environment token overrides gh on {}. Unset the token environment variables before switching",
                gh.host
            );
        }
        let accounts = discovery
            .hosts
            .get(&gh.host)
            .context("GitHub account unavailable; check `gh auth status`")?;
        if !accounts
            .iter()
            .any(|a| a.login.eq_ignore_ascii_case(&gh.user))
        {
            bail!(
                "{} is not logged in on {}; use `gh auth login` first",
                gh.user,
                gh.host
            );
        }
        Some(Github {
            host: gh.host.clone(),
            user: accounts
                .iter()
                .find(|a| a.active)
                .context("cannot determine previous gh account; check `gh auth status`")?
                .login
                .clone(),
        })
    } else {
        None
    };

    let path = global_config_path()?;
    let parent = path.parent().context("Git config has no parent")?;
    fs::create_dir_all(parent)?;
    let _lock = GitLock::acquire(&path)?;
    let original = read_optional(&path)?;
    let mut staged = tempfile::Builder::new()
        .prefix(".gitsw-")
        .tempfile_in(parent)?;
    if let Some(bytes) = &original {
        staged.write_all(bytes)?;
        staged
            .as_file()
            .set_permissions(fs::metadata(&path)?.permissions())?;
    }
    staged.flush()?;
    let identity = tempfile::NamedTempFile::new_in(parent)?;
    for (key, value) in [("user.name", &profile.name), ("user.email", &profile.email)] {
        let result = output(Command::new("git").args([
            OsStr::new("config"),
            OsStr::new("--file"),
            staged.path().as_os_str(),
            OsStr::new("--unset-all"),
            OsStr::new(key),
        ]))?;
        if !result.status.success() && result.status.code() != Some(5) {
            bail!("cannot prepare Git config; check its syntax");
        }
        checked(
            Command::new("git").args([
                OsStr::new("config"),
                OsStr::new("--file"),
                identity.path().as_os_str(),
                OsStr::new("--replace-all"),
                OsStr::new(key),
                OsStr::new(value),
            ]),
            "cannot prepare Git identity",
        )?;
    }
    // Append the identity after include/includeIf sections so included names cannot win.
    let mut file = fs::OpenOptions::new().append(true).open(staged.path())?;
    file.write_all(b"\n")?;
    file.write_all(&fs::read(identity.path())?)?;
    file.flush()?;

    let apply = || -> Result<()> {
        if let Some(gh) = &profile.github {
            gh_switch(gh)?;
            checked(
                Command::new("gh")
                    .env("GIT_CONFIG_GLOBAL", staged.path())
                    .args(["auth", "setup-git", "--hostname", &gh.host]),
                "cannot configure gh Git credentials",
            )?;
        }
        Ok(())
    };
    let result = apply().and_then(|_| {
        if read_optional(&path)? != original {
            bail!("global Git config changed during switching; try again");
        }
        // Git rewrites the staged path through its own lock/rename, so reopen it for fsync.
        fs::File::open(staged.path())?.sync_all()?;
        staged
            .persist(&path)
            .map_err(|e| e.error)
            .context("cannot save global Git config")?;
        Ok(())
    });
    if let Err(error) = result {
        if let Some(previous) = previous
            && let Err(rollback) = gh_switch(&previous)
        {
            bail!(
                "{error:#}. Git config was not changed. Restoring gh also failed: {rollback:#}; restore with `gh auth switch --hostname {} --user {}`",
                previous.host,
                previous.user
            );
        }
        return Err(error.context("switch cancelled; global Git config was not changed"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_name_links_case_insensitively_without_changing_identity() {
        let mut store = Store {
            profiles: vec![Profile {
                label: "Personal".into(),
                name: "alice".into(),
                email: "personal@example.com".into(),
                github: None,
            }],
            path: PathBuf::from("/not/a/config"),
            ..Store::default()
        };
        let discovery = Discovery {
            hosts: BTreeMap::from([(
                "github.com".into(),
                vec![GhAccount {
                    login: "Alice".into(),
                    active: true,
                    token_source: "keyring".into(),
                }],
            )]),
            warnings: vec![],
        };
        merge_profiles(&mut store, &discovery, &GitIdentity::default()).unwrap();
        assert_eq!(store.profiles.len(), 1);
        assert_eq!(store.profiles[0].email, "personal@example.com");
        assert_eq!(store.profiles[0].github.as_ref().unwrap().user, "Alice");
    }
}
