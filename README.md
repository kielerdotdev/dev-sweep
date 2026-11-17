# dev-sweep

Find old development files, check what they belong to, and choose what to remove in a terminal UI.

It handles project `node_modules`, Rust build output, Git worktrees, and Docker images and build cache. It leaves global npm, Bun, and pnpm caches alone. Scan and clean require approval for deletion. The daily job only reports what it finds. Optional reap automation snapshots finished agent work before removing its checkout.

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

Press **p** or **Enter**, then approve the AI review. The AI returns a checklist with every recommended cleanup checked. Untick exceptions with Space, then type the displayed `DELETE N` phrase and press Enter in that same checklist. There is no separate adoption step.

Rows appear as the scan finishes each folder. You can review finished rows while it scans the rest. Measurements use shared queues with up to eight active workers across projects, caches and large storage areas. Parent and child scans run separately so they do not compete for the same files. Completed rows appear immediately; large directories take longer, and quitting cancels the workers.

| Key | Action |
| --- | --- |
| Up/Down or j/k | Move |
| Tab / Shift+Tab | Change category; Tab in a plan shows review outcomes |
| Enter / p | Build a cleanup plan, or reopen the last plan |
| n | Build a fresh plan |
| Space | Untick a plan exception, or toggle a manual selection |
| / | Search |
| s | Change sorting |
| PgUp / PgDn | Scroll details or a dialog |
| a | Ask configured models about the selection, or highlighted row |
| r | Review deletion |
| Esc | Cancel |
| ? / q | Help / quit |

For manual cleanup, select files with Space and press `r`, read the warnings, and type `DELETE N` for the number of selected items. Docker volumes need their own `DELETE VOLUME exact_name` confirmation, one at a time.

The All category groups dependency folders and build output under removable worktrees. Eligible caches inside retained worktrees appear separately so you can reclaim generated files without removing their checkout. Removing the worktree includes those folders and keeps the Git branch. Use Caches if you want to remove dependencies while keeping the checkout. You cannot select a folder and its parent together.

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

Press **p** or **Enter** to build a cleanup plan for eligible items in the current category and search. No manual selection is needed first. The dialog summarizes the model, request count and shared metadata. Press `m` for exact evidence, or Enter to start reviews. There are at most four concurrent requests, one per item per configured provider, with a 512-item limit per plan. Requests may cost money.

Review progress stays visible while scanning continues. If no candidates have arrived yet, the empty-state dialog explains why; Enter waits for the remaining inventory and opens metadata approval when ready. It does not start requests automatically. Esc in the inventory stops new requests; already-running requests may take up to 45 seconds to finish. Authentication, credit, model, rate-limit and connection failures stop the remaining batch.

The AI returns a checklist with recommended items already checked. Space unticks exceptions. Type the displayed `DELETE N` phrase and press Enter to approve and remove those exact items, without leaving the checklist. Tab shows items retained by the models and failed reviews. Esc returns without changing manual selections or deleting anything. `p` reopens the saved plan without new requests; `n` starts a fresh review. Newly discovered items are not added to an existing draft.

Blocked candidates, native store cleanup, and Docker volumes are excluded from bulk plans. Worktrees take precedence over their nested build artifacts; independently eligible caches in blocked worktrees remain available. Failed reviews, tied choices, and provider disagreements stay unchecked. Press **a** to review a highlighted item or up to eight manual selections.

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
dev-sweep reap
dev-sweep restore /absolute/path/to/reaped/worktree
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
# The script finds the test-only PTY fixture built by cargo test.
python3 tests/proposal_pty.py
python3 tests/measurement_parallel.py
python3 tests/scan_cancel_pty.py
cargo build --release --locked
python3 tests/scan_benchmark.py target/release/dev-sweep
```

Tests create disposable projects and worktrees, and mock package managers and Docker. They cover refusal checks, changed files, model responses, terminal confirmation, and scan cancellation. They do not clean the host's projects or stores.

## Automatic agent cleanup and recovery

`dev-sweep reap` previews finished linked worktrees under `agent_root` paths, idle
artifacts, and selected Docker leftovers. `reap --apply` refuses unless the config
contains `reap=true`. Scan and clean never remove worktrees; explicit
`remove-worktree` retains its existing clean/reachable/idle checks.

```ini
reap=false
reap_requires_push=true
agent_root=~/.t3/worktrees
agent_root=~/.codex/worktrees
t3_state=~/.t3/userdata/statev2.sqlite
reap_after_hours=12
orphan_idle_days=3
artifact_idle_days=2
volume_after_hours=24
snapshot_max_file_mb=50
snapshot_max_total_mb=200
```

These are the defaults. Repeat `agent_root` to replace the default agent roots.
These roots are independent of ordinary `root` entries and `--root`; exclusions
and keep markers still apply. Durations and MiB limits are positive integers.
`init` writes all these keys, with reap disabled.

T3 is opened strictly read-only, including when its live database uses WAL.
Every referencing thread must be archived, deleted, or explicitly settled with a
settlement timestamp. An active override wins over a stale settlement timestamp.
The newest completion or update must be at least `reap_after_hours` old. Without a
T3 database only the conservative orphan idle rule can qualify a worktree; a database
that exists but cannot be read retains every worktree.

With `reap_requires_push=true` (the default), a worktree also has to hold no work
that exists only on this machine: no uncommitted or untracked changes, and HEAD on a
remote-tracking branch or already contained in a remote `main`/`master` (squash merges
and cherry-picks count). Set it to `false` to reap unpushed work too; the local
snapshot below is then the only copy. Orphans need both filesystem and Git inactivity.
Other agents use that same orphan rule. Main, bare, locked, busy, protected,
submodule-containing and nested-repository worktrees stay. Large untracked or
ignored non-artifact files block snapshots at the per-file or total limit.

Before removing a worktree, reap uses a temporary Git index to snapshot tracked,
untracked non-ignored and small ignored non-artifact files. This includes `.env`.
Recognised ignored artifacts such as node_modules, target, .next, .turbo and Python
environments are regenerable and excluded. The snapshot commit lives in the shared
repository as `refs/dev-sweep/wip/<project>/<worktree>`. Repeated snapshots get a
unique timestamp suffix. A companion `refs/dev-sweep/index/...` ref preserves staged-only content.
Branches and unpushed commits stay local; default push refspecs do not push these refs.
The fsynced journal at `~/.local/state/dev-sweep/reaped.jsonl` records the original
path, common Git directory, branch, HEAD, refs, thread ids, estimated bytes and time
before worktree removal. Interrupted removals can therefore leave recoverable journal
entries for worktrees that still exist.

```sh
dev-sweep restore /original/worktree/path
dev-sweep restore /original/worktree/path --apply
```

Reap checks content and staging again immediately before removal.

Restore uses the latest journal entry and refuses an existing destination. It
restores the original HEAD and branch when that branch is still available at the
same commit. A missing, moved or occupied branch restores detached at the original
HEAD. Unborn branches restore unborn. Files and the original staging boundary are
restored, including untracked and ignored files.

The lossless guarantee covers Git-representable source content and staging, not
regenerable artifacts or Docker data. Snapshots share the repository's failure
domain: keep the common repository, journal and snapshot refs. This is not an
external backup. Git does not preserve empty directories, timestamps, extended
attributes or all permission bits; configured clean/smudge filters and encodings
retain Git's normal semantics. Process inspection can be denied, and concurrent
writers can race cleanup: protect services and important offline data with keep
markers or exclusions. Snapshots can contain secrets, so protect local repository
access. Reap does not upload or push anything.

Remaining linked agent worktrees use `artifact_idle_days` for recognised build and
dependency directories, with the existing process, ownership, marker and tracked-file
checks. Ordinary main checkouts retain the configured `days` rule.

Docker inspection uses bounded list/inspect calls, never `docker system df -v`.
Only dangling, unlabelled volumes with exactly 64 lowercase hex characters and an
old enough creation time qualify as anonymous. Named volumes qualify only with
`dev-sweep.owner=/absolute/path` pointing to a missing path. Stopped containers with
that label and a missing owner are removed with `docker container rm -v`; running
containers stay. Reaping also removes stopped Compose containers whose working
directory lies in the worktree, then dangling volumes labelled for those Compose
projects. Running Compose or owner containers block worktree removal. Volume removal
rechecks container references and never uses force or prune. This intentionally deletes
orphan database contents: anonymous names and age cannot prove data is unwanted.
Keep important persistent volumes named and without orphan-owner labels. Unavailable
or slow Docker is skipped; when Docker checking is enabled, unknown Docker state also
blocks worktree removal. `docker=false` or `--no-docker` explicitly disables those checks.

On Linux, `bash install.sh --reap` additionally installs and enables an hourly
`dev-sweep-reap.timer` running `reap --apply` with Nice=10 and idle I/O. It still
requires `reap=true` in the config. This flag does not change your config or enable
reap on macOS. `--no-schedule` suppresses all timers. Uninstall removes both timers.
Inspect logs with `journalctl --user -u dev-sweep-reap.service`.
