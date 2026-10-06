#![cfg(unix)]
use serde_json::json;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    process::{Command, Output},
};
use tempfile::TempDir;

struct Fixture {
    root: TempDir,
    home: PathBuf,
    config: PathBuf,
    git: PathBuf,
    bin: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let config = root.path().join("config");
        let bin = root.path().join("bin");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(config.join("gitsw")).unwrap();
        fs::create_dir_all(&bin).unwrap();
        let git = home.join(".gitconfig");
        fs::write(&git, "# Keep this comment\n[user]\n name = Alice\n email = alice@example.com\n[core]\n editor = vim\n").unwrap();
        fs::write(root.path().join("active"), "Alice\n").unwrap();
        fs::write(bin.join("gh"), r##"#!/bin/sh
case "$1 $2" in
  'auth status')
    if [ "$FAIL_DISCOVERY" = 1 ]; then exit 1; fi
    active=$(cat "$TEST_ROOT/active")
    a=false; b=false
    if [ "$active" = Alice ]; then a=true; else b=true; fi
    printf '{"hosts":{"github.com":[{"login":"Alice","active":%s,"tokenSource":"keyring"},{"login":"Bob","active":%s,"tokenSource":"keyring"}]}}' "$a" "$b"
    ;;
  'auth switch')
    printf '%s\n' "$*" >> "$TEST_ROOT/calls"
    if [ "$FAIL_SWITCH" = 1 ] && [ "$6" = Bob ]; then echo 'mock switch failure' >&2; exit 1; fi
    printf '%s\n' "$6" > "$TEST_ROOT/active"
    ;;
  'auth setup-git')
    printf '%s\n' "$*" >> "$TEST_ROOT/calls"
    if [ "$GIT_CONFIG_GLOBAL" = "$TEST_GIT" ]; then echo 'unstaged config!' >&2; exit 1; fi
    if [ "$FAIL_SETUP" = 1 ]; then echo 'mock helper failure' >&2; exit 1; fi
    git config --global --replace-all credential.https://github.com.helper '!gh auth git-credential'
    if [ "$CONCURRENT_EDIT" = 1 ]; then printf '\n# concurrent change\n' >> "$TEST_GIT"; fi
    ;;
  'api --hostname') printf '{"login":"Bob","id":42,"name":"Bob Example","email":null}';;
  *) exit 9;;
esac
"##).unwrap();
        fs::set_permissions(bin.join("gh"), fs::Permissions::from_mode(0o755)).unwrap();
        let f = Self {
            root,
            home,
            config,
            git,
            bin,
        };
        f.profiles(json!({"profiles":[
            {"label":"personal","name":"Alice","email":"alice@example.com","github":{"host":"github.com","user":"Alice"}},
            {"label":"work","name":"Bob Example","email":"bob@example.com","github":{"host":"github.com","user":"Bob"}},
            {"label":"local","name":"Local Person","email":"local@example.com"}
        ]}));
        f
    }
    fn profiles(&self, data: serde_json::Value) {
        fs::write(self.config.join("gitsw/profiles.json"), data.to_string()).unwrap();
    }
    fn cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_gitsw"));
        cmd.args(args)
            .current_dir(&self.home)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("GIT_CONFIG_GLOBAL", &self.git)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "PATH",
                format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("TEST_ROOT", self.root.path())
            .env("TEST_GIT", &self.git);
        for key in [
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "GH_ENTERPRISE_TOKEN",
            "GITHUB_ENTERPRISE_TOKEN",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_PARAMETERS",
            "GIT_AUTHOR_NAME",
            "GIT_AUTHOR_EMAIL",
            "GIT_COMMITTER_NAME",
            "GIT_COMMITTER_EMAIL",
        ] {
            cmd.env_remove(key);
        }
        cmd
    }
    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }
    fn git_value(&self, key: &str) -> String {
        let out = Command::new("git")
            .args(["config", "--file"])
            .arg(&self.git)
            .args(["--get", key])
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().into()
    }
    fn active(&self) -> String {
        fs::read_to_string(self.root.path().join("active"))
            .unwrap()
            .trim()
            .into()
    }
}
fn success(out: Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn switches_git_gh_and_helper_preserving_unrelated_config() {
    let f = Fixture::new();
    fs::set_permissions(&f.git, fs::Permissions::from_mode(0o640)).unwrap();
    success(f.run(&["use", "WORK"]));
    assert_eq!(f.git_value("user.name"), "Bob Example");
    assert_eq!(f.git_value("user.email"), "bob@example.com");
    assert_eq!(f.git_value("core.editor"), "vim");
    assert!(
        f.git_value("credential.https://github.com.helper")
            .contains("gh auth git-credential")
    );
    assert_eq!(f.active(), "Bob");
    assert!(
        fs::read_to_string(&f.git)
            .unwrap()
            .contains("# Keep this comment")
    );
    assert_eq!(
        fs::metadata(&f.git).unwrap().permissions().mode() & 0o777,
        0o640
    );
}

#[test]
fn switch_and_helper_failures_leave_git_untouched_and_restore_gh() {
    for fail in ["FAIL_SWITCH", "FAIL_SETUP"] {
        let f = Fixture::new();
        let before = fs::read(&f.git).unwrap();
        let out = f.cmd(&["use", "work"]).env(fail, "1").output().unwrap();
        assert!(!out.status.success());
        assert_eq!(fs::read(&f.git).unwrap(), before);
        assert_eq!(f.active(), "Alice");
        assert!(String::from_utf8_lossy(&out.stderr).contains("global Git config was not changed"));
    }
}

#[test]
fn concurrent_edits_are_preserved_and_gh_is_restored() {
    let f = Fixture::new();
    let out = f
        .cmd(&["use", "work"])
        .env("CONCURRENT_EDIT", "1")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(f.active(), "Alice");
    assert_eq!(f.git_value("user.name"), "Alice");
    assert!(
        fs::read_to_string(&f.git)
            .unwrap()
            .ends_with("# concurrent change\n")
    );
}

#[test]
fn token_override_blocks_switch_before_mutating_anything() {
    let f = Fixture::new();
    let before = fs::read(&f.git).unwrap();
    let out = f
        .cmd(&["use", "work"])
        .env("GH_TOKEN", "fake-test-token")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(fs::read(&f.git).unwrap(), before);
    assert_eq!(f.active(), "Alice");
    assert!(!f.root.path().join("calls").exists());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("fake-test-token"));
}

#[test]
fn existing_git_lock_prevents_switch_and_is_left_intact() {
    let f = Fixture::new();
    let before = fs::read(&f.git).unwrap();
    let lock = f.home.join(".gitconfig.lock");
    fs::write(&lock, "other process").unwrap();
    let out = f.run(&["use", "work"]);
    assert!(!out.status.success());
    assert_eq!(fs::read(&f.git).unwrap(), before);
    assert_eq!(f.active(), "Alice");
    assert_eq!(fs::read_to_string(lock).unwrap(), "other process");
    assert!(!f.root.path().join("calls").exists());
}

#[test]
fn git_only_works_when_gh_is_unavailable() {
    let f = Fixture::new();
    success(
        f.cmd(&["use", "local"])
            .env("FAIL_DISCOVERY", "1")
            .output()
            .unwrap(),
    );
    assert_eq!(f.git_value("user.name"), "Local Person");
    assert_eq!(f.active(), "Alice");
}

#[test]
fn migrates_legacy_profiles_and_discovers_other_accounts() {
    let f = Fixture::new();
    fs::remove_file(f.config.join("gitsw/profiles.json")).unwrap();
    fs::write(
        f.config.join("gitsw/accounts.json"),
        r#"{"accounts":[{"label":"personal","name":"alice","email":"alice@example.com"}]}"#,
    )
    .unwrap();
    let listed = success(f.run(&["list"]));
    assert!(listed.contains("gh: github.com/Alice"));
    assert!(listed.contains("42+Bob@users.noreply.github.com"));
    let saved: serde_json::Value =
        serde_json::from_slice(&fs::read(f.config.join("gitsw/profiles.json")).unwrap()).unwrap();
    assert_eq!(saved["profiles"][0]["name"], "alice");
    assert_eq!(saved["profiles"][0]["email"], "alice@example.com");
    assert_eq!(saved["profiles"][0]["github"]["user"], "Alice");
}

#[test]
fn preserves_symlinked_git_config() {
    let f = Fixture::new();
    let target = f.home.join("dotfiles.gitconfig");
    fs::rename(&f.git, &target).unwrap();
    symlink(&target, &f.git).unwrap();
    success(f.run(&["use", "local"]));
    assert!(
        fs::symlink_metadata(&f.git)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(f.git_value("user.name"), "Local Person");
}

#[test]
fn global_identity_wins_over_includes_without_editing_included_file() {
    let f = Fixture::new();
    let included = f.home.join("included.gitconfig");
    let bytes = "[user]\n name = Included User\n email = included@example.com\n";
    fs::write(&included, bytes).unwrap();
    fs::write(&f.git, "[user]\n name = Alice\n email = alice@example.com\n[include]\n path = included.gitconfig\n").unwrap();
    success(f.run(&["use", "local"]));
    let out = Command::new("git")
        .args(["config", "--global", "--get", "user.name"])
        .env("GIT_CONFIG_GLOBAL", &f.git)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Local Person");
    assert_eq!(fs::read_to_string(&included).unwrap(), bytes);
}

#[test]
fn respects_xdg_global_git_config_when_home_config_is_absent() {
    let f = Fixture::new();
    fs::remove_file(&f.git).unwrap();
    fs::create_dir_all(f.config.join("git")).unwrap();
    fs::write(
        f.config.join("git/config"),
        "[user]\n name = XDG User\n email = xdg@example.com\n",
    )
    .unwrap();
    success(
        f.cmd(&["use", "local"])
            .env_remove("GIT_CONFIG_GLOBAL")
            .output()
            .unwrap(),
    );
    assert!(!f.git.exists());
    assert!(
        fs::read_to_string(f.config.join("git/config"))
            .unwrap()
            .contains("Local Person")
    );
}

#[test]
fn old_maintenance_commands_are_removed_and_settings_requires_terminal() {
    let f = Fixture::new();
    let help = success(f.run(&["--help"]));
    assert!(help.contains("--setting"));
    for command in ["add", "edit", "remove"] {
        assert!(!f.run(&[command]).status.success());
    }
    for flag in ["-setting", "--setting", "-s"] {
        let out = f.run(&[flag]);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("needs a terminal"));
    }
}

#[test]
fn non_terminal_default_command_exits_with_instructions() {
    let f = Fixture::new();
    let out = f.run(&[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("picker needs a terminal"));
}

#[test]
fn malformed_config_is_not_silently_overwritten() {
    let f = Fixture::new();
    let path = f.config.join("gitsw/profiles.json");
    fs::write(&path, "invalid JSON").unwrap();
    assert!(!f.run(&["list"]).status.success());
    assert_eq!(fs::read_to_string(path).unwrap(), "invalid JSON");
}
