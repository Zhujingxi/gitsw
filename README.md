# gitsw

A small Rust terminal picker to switch your project or global Git commit identity and the matching GitHub CLI account together. The picker appears inline below your command and preserves the surrounding terminal output.

```sh
gitsw
```

Use **↑/↓** or **j/k** to select, **Enter** to switch, **/** to search, and **q**, **Esc**, or **Ctrl-C** to cancel. While searching, Enter finishes the search; press Enter again to switch. Each account row ends with two fixed indicator positions: a yellow `┃` marks the active commit identity, and a blue `┃` marks the active GitHub account. Both bars appear when both are active; a single bar indicates only its corresponding state. The indicators never shift the account text.

After selecting an identity, choose **Project level** (this repository only) or **Global level** (the default for all repositories), then press Enter to apply. Project level is selected initially inside a Git repository. Outside a repository, only Global level is available. Cancelling either prompt leaves Git and GitHub authentication unchanged. The commit indicator reflects the effective identity in the current repository, or the global identity when using `--global`.

## Install

Requires Rust/Cargo to build, Git to run, and `gh` for GitHub account switching. Intended for macOS and Linux. Python 3 is only needed for the PTY smoke tests.

```sh
./scripts/install.sh                  # installs to ~/.local/bin
./scripts/install.sh /usr/local/bin   # replace an existing installation here
```

Add the chosen directory to your shell's PATH if needed. The installed binary runs without Cargo.

## Profiles

`gitsw` discovers accounts already logged in through `gh auth login`, imports the current global Git identity, and preserves identities from the old `~/.config/gitsw/accounts.json` format. It links a saved identity to a GitHub account when its Git name uniquely matches the GitHub login (case insensitive). Other profiles can be linked using the GitHub account selector in `gitsw -setting`.

For a newly discovered GitHub account, it fetches the public name and email. If a github.com account has no public email, it uses `ACCOUNT_ID+LOGIN@users.noreply.github.com`. These defaults are cached and can be edited. If the public lookup fails, or an enterprise account has no public email, configure its email before switching.

```sh
gitsw                 # select an account, then choose project or global scope
gitsw -setting        # configure accounts in the inline TUI
gitsw list
gitsw use work        # choose project or global scope interactively
gitsw use work --local   # apply to this repository without a scope prompt
gitsw use work --global  # apply globally without a scope prompt
```

In settings, select an account and press Enter to configure its label, commit name, commit email, and GitHub account link. Select a field to edit it; Enter confirms and Esc cancels the field edit. Ctrl-U clears the input. Use **Save** to persist changes or **Back** to discard them. Changes take effect when you next switch to that profile.

**New account** creates a profile. **Delete profile** removes it from the picker after confirmation without logging out of GitHub. Deleted profiles stay hidden from automatic discovery; **Restore hidden account** makes them available again. Settings do not change the active Git or GitHub identity.

Labels are case insensitive and unique. Multiple saved identities may link to the same GitHub account. Choose **Git only** to configure an identity without changing GitHub authentication. For GitHub accounts that are not listed, first run `gh auth login`, or configure a host/login with **Other account...**. The old `add`, `edit`, and `remove` subcommands are removed. Settings also accept `--setting`, `--settings`, and `-s`.

`--local` (also `--project`) and `--global` can also be used with the account picker to skip the scope prompt. Scripts without a terminal must pass one of these flags to `gitsw use LABEL`.

Profiles live in `${XDG_CONFIG_HOME:-~/.config}/gitsw/profiles.json`:

```json
{
  "profiles": [
    {
      "label": "work",
      "name": "Alice Example",
      "email": "alice@company.com",
      "github": { "host": "github.com", "user": "alice-work" }
    }
  ]
}
```

Only identity metadata is saved. GitHub CLI retains ownership of tokens and keychain credentials. Account links can be changed directly in the settings TUI. Deleted profiles are recorded in the optional `hidden_profiles` list for restoration.

## Switching behavior

For a linked profile, switching runs `gh auth switch --hostname HOST --user LOGIN` and configures the GitHub CLI HTTPS Git credential helper for that host with `gh auth setup-git`. The identity and credential helper are saved in the chosen Git config scope. GitHub CLI's active account is shared across projects regardless of that scope. Git `user.name` is a commit author name; the separate GitHub login identifies the authenticated account.

Git updates are staged in the chosen config's directory and saved atomically after the GitHub steps succeed. The original config stays intact on failure, and gitsw attempts to restore the previous GitHub account. It reports the recovery command if restoration also fails. It preserves unrelated config, comments, file permissions, and symlinked config files. Git's standard lock file prevents concurrent Git edits; additional file changes detected before saving cancel the switch. Included configs are preserved, with the chosen identity written after includes.

For global scope, `GIT_CONFIG_GLOBAL` is respected. Otherwise, gitsw follows Git's global write location: `~/.gitconfig`, or the XDG Git config if it exists and `~/.gitconfig` does not. For project scope, gitsw resolves the repository's config through Git, including when invoked from a subdirectory or linked worktree. Linked worktrees share the repository-level config.

Repository-local configuration can override the global identity; gitsw reports this after switching. Worktree-specific config and author/committer environment variables can also override commit identity. `GH_TOKEN`, `GITHUB_TOKEN`, and the enterprise equivalents override saved GitHub accounts; linked switching refuses to proceed until the relevant variables are unset. SSH keys and commit signing keys are managed separately.

## Development

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
python3 tests/tui_smoke.py
cargo build --release --locked
```

The integration tests use real Git with isolated configs and a fake `gh`. They cover project/global switching, linked worktrees, rollback, explicit scope requirements for scripts, token overrides, profile migration, includes, symlinks, XDG configs, and removal of the old maintenance commands. PTY tests cover keyboard selection, scope selection and cancellation, colored indicators, searching, small terminals, terminal restoration, and settings workflows for creating, editing, linking, deleting, and restoring profiles.
