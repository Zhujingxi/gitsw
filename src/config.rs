use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Github {
    pub host: String,
    pub user: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Profile {
    pub label: String,
    pub name: String,
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<Github>,
}

impl Profile {
    pub fn validate(&self) -> Result<()> {
        validate_text("label", &self.label)?;
        validate_text("name", &self.name)?;
        validate_text("email", &self.email)?;
        if !self.email.contains('@') || self.email.chars().any(char::is_whitespace) {
            bail!("email must contain @ and have no whitespace");
        }
        if let Some(gh) = &self.github {
            validate_text("GitHub username", &gh.user)?;
            validate_text("GitHub host", &gh.host)?;
            if !gh
                .user
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
            {
                bail!("invalid GitHub username");
            }
            if !gh
                .host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
            {
                bail!("invalid GitHub hostname");
            }
        }
        Ok(())
    }
}

fn validate_text(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        bail!("{field} must be nonempty and cannot contain control characters");
    }
    Ok(())
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Store {
    #[serde(default)]
    pub profiles: Vec<Profile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_profiles: Vec<Profile>,
    #[serde(skip)]
    pub path: PathBuf,
}

pub fn home() -> Result<PathBuf> {
    env::var_os("HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .context("HOME is not set")
}

pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    Ok(home()?.join(".config"))
}

impl Store {
    pub fn load() -> Result<Self> {
        let dir = config_dir()?.join("gitsw");
        let path = dir.join("profiles.json");
        let mut store = if path.exists() {
            serde_json::from_slice::<Store>(&fs::read(&path)?).with_context(|| {
                format!(
                    "invalid JSON in {}; fix this file before continuing",
                    path.display()
                )
            })?
        } else {
            let legacy = dir.join("accounts.json");
            if legacy.exists() {
                #[derive(Deserialize)]
                struct Legacy {
                    accounts: Vec<Profile>,
                }
                let old: Legacy = serde_json::from_slice(&fs::read(&legacy)?)
                    .context("invalid legacy accounts.json; migration stopped")?;
                Store {
                    profiles: old.accounts,
                    ..Self::default()
                }
            } else {
                Self::default()
            }
        };
        store.path = path;
        for (i, profile) in store.profiles.iter().enumerate() {
            validate_text("profile label", &profile.label)?;
            if store.profiles[..i]
                .iter()
                .any(|p| p.label.eq_ignore_ascii_case(&profile.label))
            {
                bail!(
                    "duplicate profile label {:?} in {}",
                    profile.label,
                    store.path.display()
                );
            }
        }
        Ok(store)
    }

    pub fn find(&self, label: &str) -> Option<usize> {
        self.profiles
            .iter()
            .position(|profile| profile.label.eq_ignore_ascii_case(label))
    }

    pub fn save(&self) -> Result<()> {
        atomic_write(&self.path, &serde_json::to_vec_pretty(self)?)
            .context("could not save profiles")
    }
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("config path has no parent")?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_terminal_controls_and_invalid_email() {
        let mut p = Profile {
            label: "Work".into(),
            name: "Some Person".into(),
            email: "me@example.com".into(),
            github: None,
        };
        assert!(p.validate().is_ok());
        p.label = "\x1b[2J".into();
        assert!(p.validate().is_err());
        p.label = "Work".into();
        p.email = "no email".into();
        assert!(p.validate().is_err());
    }
}
