# dev-sweep

Find old development files, check what they belong to, and choose what to remove in a terminal UI.

It handles project `node_modules`, Rust build output, Git worktrees, and Docker images and build cache. It leaves global npm, Bun, and pnpm caches alone. Cleanup always needs your approval. The daily job only reports what it finds.

Requires Linux or macOS and Rust 1.85 or newer.

## Install

```sh
git clone https://github.com/kielerdotdev/dev-sweep.git
cd dev-sweep
bash install.sh
~/.local/bin/dev-sweep tui
```

The installer puts the binary in `~/.local/bin`, creates `~/.config/dev-sweep/config`, and schedules a daily report around 03:00. Add `~/.local/bin` to your PATH if you want to run `dev-sweep` without the full path.

Use `bash install.sh --no-schedule` to install without the daily job. `bash install.sh --uninstall` removes the binary and job but keeps your config. For a Cargo-only installation, run `cargo install --path . --locked` followed by `dev-sweep init`.

## Use the TUI

```sh
dev-sweep tui
```

Rows appear as the scan finishes each folder. You can review finished rows while it scans the rest. Large directories take longer to measure; quitting stops the scan workers.

| Key | Action |
| --- | --- |
| Up/Down or j/k | Move |
| Tab / Shift+Tab | Change category |
| Space | Select or deselect |
| / | Search |
| s | Change sorting |
| PgUp / PgDn | Scroll details or a dialog |
| a | Ask configured models about the selection, or highlighted row |
| r | Review deletion |
| Esc | Cancel |
| ? / q | Help / quit |

After selecting files, press `r`, read the warnings, and type `DELETE N` for the number of selected items. Docker volumes need their own `DELETE VOLUME exact_name` confirmation, one at a time.

The All category groups dependency folders and build output under their worktree. Removing the worktree includes those folders and keeps the Git branch. Use Caches if you want to remove dependencies while keeping the checkout. You cannot select a folder and its parent together.

Sizes are disk occupancy estimates. Nested rows overlap, hardlinks share storage, and Docker layers may still be used elsewhere. Adding all the rows together will overcount.

## Set your OpenRouter key

The app reads `OPENROUTER_API_KEY` from the environment of the terminal that launches it. The key does not go in the dev-sweep config.

For the current Bash session:

```bash
read -rsp 'OpenRouter API key: ' OPENROUTER_API_KEY
printf '\n'
export OPENROUTER_API_KEY
~/.local/bin/dev-sweep tui
```

To save that key for later sessions, after entering it above:

```bash
mkdir -p ~/.config/dev-sweep
(umask 077; printf 'export OPENROUTER_API_KEY=%q\n' "$OPENROUTER_API_KEY" > ~/.config/dev-sweep/secrets.env)
chmod 600 ~/.config/dev-sweep/secrets.env
```

Before launching the TUI in a new Bash session:

```bash
source ~/.config/dev-sweep/secrets.env
~/.local/bin/dev-sweep tui
```

You can put the `source` line in your `~/.bashrc` if you want it loaded in every Bash session. Keep the secrets file outside the repository. The daily report does not need a key.

OpenRouter defaults to `cloudflare/clef` through its Decisions API. Your existing `OPENROUTER_API_KEY` works for Clef and Jev; separate provider keys are optional. Remove any old `OPENROUTER_MODEL=openai/gpt-4.1-mini` override. Chat models are no longer used.

```bash
# Optional alternative decision model:
export OPENROUTER_MODEL=typesafe/jev-1.13
```

Press **p** to build a cleanup proposal for eligible items in the current category and search. No manual selection is needed first. The dialog shows exactly which metadata will be sent; Enter approves those requests. There are at most four concurrent requests, one per item per configured provider, with a 512-item limit per proposal. Narrow the view for smaller batches. Requests may cost money.

When reviews finish, the draft lists proposed items and their consequences. Enter adopts the draft, replacing your current selections; Esc keeps them. Remove exceptions with Space, then press **r** and type the exact deletion confirmation. Nothing is removed by generating or adopting a proposal. Newly discovered items are not added to an existing draft.

Blocked candidates, native store cleanup, and Docker volumes are excluded from bulk proposals. Worktrees take precedence over their nested build artifacts. Failed reviews, tied choices, and provider disagreements stay out of the proposal. Their advice remains in the details pane. Press **a** to review a highlighted item or up to eight manual selections.

| Provider | Environment variables |
| --- | --- |
| OpenRouter Decisions | `OPENROUTER_API_KEY`, optional `OPENROUTER_MODEL`, default `cloudflare/clef` |
| Direct Cloudflare Clef | `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID`, optional `DEV_SWEEP_CLEF_MODEL` set to `clef` or `clef-flash` |
| Direct TypeSafe Jev | `TYPESAFE_API_KEY`, optional `TYPESAFE_MODEL`, default `jev-latest` |

All integrations return a typed choice among `regenerable`, `investigate`, and `retain`, with probabilities. Model confidence does not prove deletion is safe. Local checks cannot be overridden, and they run again before removal. File contents and keys are never included in model input. Long metadata is refused rather than silently truncated.

See [decision-model research](docs/decision-models.md) for the API, model tradeoffs, and evaluation limits.

## Choose folders and age

Edit `~/.config/dev-sweep/config`:

```ini
days=30
docker=true
global_caches=true
root=~/projects
root=~/research
root=~/.t3/worktrees
root=~/.codex/worktrees
exclude=~/projects/always-running-service
```

Repeat `root` and `exclude` as needed. Paths are unquoted and may contain spaces. `init` looks for projects under your home directory and adds common development and worktree folders. Add external drives or deeper projects yourself. It keeps an existing config.

Protect a folder and everything inside it with an exclusion or a keep marker:

```sh
touch ~/projects/always-running-service/.dev-sweep-keep
```

For one run, `--root` replaces the configured roots and disables global cleanup. `--exclude` adds an exclusion. `--days` changes the default 30-day idle limit.

```sh
dev-sweep scan --root "$HOME/research" --days 60 --no-docker
```

Global npm, Bun, and pnpm cache cleanup is disabled, including explicit `clean-store` commands. Common global installation and cache paths, plus paths supplied through npm cache/prefix variables, `PNPM_HOME`, and Bun installation/cache variables, are excluded from filesystem cleanup. Project-local `node_modules` remains available for cleanup.

`global_caches=true` enables uv cache pruning, Nix garbage collection, and old VS Code/Cursor extension download entries. It also adds temporary projects and Cargo output to discovery. `--no-global-caches` disables this group.

## What can be removed

Project caches need identifying files. `node_modules` needs a neighbouring `package.json`. Cargo `target` needs a manifest and build marker; custom Cargo output needs `.rustc_info.json` and fingerprint directories. Next.js, Turborepo, and Python caches have similar checks.

Every file must be older than the age limit. Recently accessed generated files stay. Git-tracked files, keep markers, excluded paths, unreadable files, and filesystem boundaries block removal. Projects with a process running from them stay. Process checks cover the current user's working directories and executables; some processes deny inspection.

Worktrees must be clean, unlocked, and idle. Their commit must exist in the main checkout or a remote-tracking branch. Unknown ignored files such as `.env`, untracked files, changed source, and submodules block removal. Recognized ignored dependencies and build output can be removed with the checkout. Main and bare worktrees stay. Remote-tracking refs reflect your last fetch.

The app rechecks the files before deletion. Age and process checks cannot tell whether you still want an old checkout or have a backup. Use exclusions for important offline environments and services.

## CLI commands

Commands preview their actions unless you add `--apply`.

```sh
dev-sweep scan
dev-sweep audit
dev-sweep audit --system
dev-sweep remove-worktree /absolute/path/to/worktree
dev-sweep clean-store uv
dev-sweep clean-store nix
dev-sweep clean-store docker-build
dev-sweep clean-store apt
dev-sweep clean-store journal
```

`audit` ranks disk use and includes retained data such as SDKs, toolchains, application data, and Docker volumes. `--system` uses noninteractive sudo for measurements. It does not delete anything. Permission failures or timeouts leave an incomplete report and return an error.

`clean --apply` approves cleanup of all eligible caches and enabled stores for that run. Use the TUI to approve individual selections.

Nix cleanup keeps registered roots and profile generations. Apt and journal cleanup require existing noninteractive sudo access. Journal cleanup removes archived logs older than the age limit and applies a 1 GiB cap.

With `docker=true`, approved cleanup prunes unused images and build cache older than the configured age. Image age means creation time. Images referenced by stopped or running containers stay. Build pruning keeps a 5 GB allowance. Containers and volumes stay unless you explicitly choose an individual volume.

```sh
dev-sweep remove-volume exact_volume_name
dev-sweep remove-volume exact_volume_name --apply
```

Volume removal checks for container references and uses `docker volume rm` without force. An unreferenced volume can still hold a database or uploads you need.

## Daily reports

Linux uses a systemd user timer at 03:00 with up to ten minutes of random delay. Missed runs happen when your user session resumes. The job runs `scan` without `--apply` and does not call models.

```sh
systemctl --user list-timers dev-sweep.timer
journalctl --user -u dev-sweep.service
systemctl --user disable --now dev-sweep.timer
```

macOS uses `~/Library/LaunchAgents/local.dev-sweep.plist`. Logs are in `~/Library/Logs/dev-sweep/`. Rerun the installer if you move Git or Docker, since the job uses the PATH saved at installation.

## Development

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --check
python3 tests/tui_pty.py
python3 tests/scan_cancel_pty.py
cargo build --release --locked
python3 tests/scan_benchmark.py target/release/dev-sweep
```

Tests create disposable projects and worktrees, and mock package managers and Docker. They cover refusal checks, changed files, model responses, terminal confirmation, and scan cancellation. They do not clean the host's projects or stores.
