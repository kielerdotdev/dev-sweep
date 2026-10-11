# dev-sweep

A Rust utility for stale development caches, managed package stores, Docker storage, and forgotten Git worktrees. `audit` accounts for the large directories even when they are retained. A Ratatui browser provides selection, warnings and optional AI safety advice. Supports Linux and macOS; building requires Rust 1.85 or later.

## Install

From this checkout, with Rust installed:

```sh
bash install.sh
```

This installs `~/.local/bin/dev-sweep`, discovers development folders under your home directory, creates a config, and enables daily reports around 03:00. The scheduled job does not delete anything or contact AI providers. Installation needs no sudo. The default idle age is **30 days**.

```sh
~/.local/bin/dev-sweep tui                # interactive browser and approval
~/.local/bin/dev-sweep scan               # preview without deleting
~/.local/bin/dev-sweep audit --system     # whole-disk accounting, including privileged sizes
~/.local/bin/dev-sweep clean --apply      # clean now
bash install.sh --report-only            # explicitly install daily reports
bash install.sh --no-schedule            # install the binary/config only
bash install.sh --uninstall              # remove binary/job, keep config
```

Add `~/.local/bin` to your shell's PATH to use `dev-sweep` directly. You can also use `cargo install --path . --locked`, then run `dev-sweep init`; use the installer for scheduling.

## Interactive cleanup

Run `dev-sweep tui`, or just `dev-sweep` from an interactive terminal. Scanning runs in the background. The browser includes fresh or blocked artifacts, package stores, Docker storage and volumes, linked worktrees, SDKs, toolchains and application data. Protected system sizes can be unknown; `audit --system` provides privileged measurements outside the TUI.

Worktree and project-cache inspections use up to four workers and publish rows as they finish. Eligible paths reuse the completed safety walk for their size and snapshot. Each repository's worktree list is discovered once. Large retained directories can take longer to measure; finished entries can be reviewed while scanning continues. Quitting cancels all measurement workers. Deletion still performs fresh safety and snapshot checks.

| Key | Action |
| --- | --- |
| Up/Down or j/k | Move through items |
| Tab / Shift+Tab | Change category |
| Space | Select or deselect |
| / then text | Search paths and names |
| Esc | Cancel dialog or clear search |
| s | Toggle size sorting |
| PgUp / PgDn | Scroll details or dialog |
| a | Request AI advice for selected items, or highlighted item |
| r | Review exact selections and consequences |
| ? / q | Help / quit |

Deletion requires typing `DELETE N` and pressing Enter in the review dialog. A volume requires `DELETE VOLUME exact_name`, one volume at a time. Selection alone never removes anything. Unreviewed AI status is explicit. Local safety checks cannot be overridden by AI and run again before removal; changed artifact identity, activity, size or protection causes refusal. Overlapping checkout/store/child selections are refused. In All, recognized caches under registered linked worktrees are grouped into one worktree row, whose allocated size includes them. The details, AI evidence and confirmation list included generated folders. Caches keeps those folders available for independent cleanup if you want to retain the checkout.

Sizes show allocated occupancy, with explicit unknown values and store totals. They are not a promise of reclaimable space. Nested rows overlap. Whole npm/Bun cache clears and native manager pruning have different consequences from age-filtered individual artifact removal.

## AI safety advice

Reviews happen inside the TUI, only after `a` and confirmation of the metadata-sharing dialog. Each configured provider reviews the same candidate metadata: paths/names, sizes, local blockers, planned action, idle threshold and consequences. File contents are not read into prompts. Model output never selects items, authorizes deletion, supplies executed commands, or overrides local checks. Provider failures remain **unreviewed**.

Supported providers:

- [Cloudflare Clef / Clef Flash](https://developers.cloudflare.com/workers-ai/models/clef/): `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID`. `DEV_SWEEP_CLEF_MODEL` chooses `clef-flash` by default or `clef`. Uses typed System One choices with validated probabilities and model confidence.
- [TypeSafe Jev](https://docs.typesafe.ai/api): `TYPESAFE_API_KEY`; optional `TYPESAFE_MODEL`, default `jev-latest`. Also uses typed decisions.
- [OpenRouter](https://openrouter.ai/docs/api/reference/overview): `OPENROUTER_API_KEY`; optional `OPENROUTER_MODEL`, default `openai/gpt-4.1-mini`. Provides a short JSON assessment, rationale and warnings.

Configure one or several providers in your terminal environment. For example, enter an OpenRouter key with masked input in Bash so the value never enters shell history:

```bash
read -rsp 'OpenRouter API key: ' OPENROUTER_API_KEY; printf '\n'
export OPENROUTER_API_KEY
dev-sweep tui
unset OPENROUTER_API_KEY
```

Use the same masked-input pattern for Cloudflare or TypeSafe tokens. Do not paste keys into chat, commit them, or put them in command arguments. The scheduler does not need these credentials.

The dialog shows every configured provider and exact metadata before sending it. Up to eight candidates can be reviewed per request. Calls can incur provider charges. Requests have a 45-second timeout, no HTTP redirects, bounded response bodies, and strict response validation. Missing credentials show setup instructions. Confidence describes a model answer, never the probability deletion is safe. Compare differing answers and investigate uncertain or persistent data.

## Configure

Edit `~/.config/dev-sweep/config`, or `$XDG_CONFIG_HOME/dev-sweep/config` if set:

```ini
days=30
docker=true
global_caches=true
root=~/projects
root=~/research
root=~/.t3/worktrees
root=~/.codex/worktrees
root=~/.cache
exclude=~/projects/always-running-service
```

Paths are unquoted and may contain spaces. Repeat `root` and `exclude` as needed. `init` discovers visible home folders containing Git, Node, or Rust projects up to six directory levels deep. It also adds existing common development folders, hidden cache and T3/Codex worktree locations, and your own temporary projects/Cargo outputs up to three levels under the platform temp directory and `/var/tmp`. Temporary source trees remain; only recognized generated caches qualify. It never overwrites a config. Add deeper projects or external drives manually.

Use CLI overrides for a one-off run:

```sh
dev-sweep scan --root "$HOME/research" --days 60 --no-docker
dev-sweep clean --root "$HOME/projects" --exclude "$HOME/projects/service" --apply --no-docker
touch ~/projects/service/.dev-sweep-keep
```

`--root` replaces configured roots and disables global package-cache cleanup for that run. `--exclude` adds protection. `.dev-sweep-keep` protects its directory and every descendant during filesystem cleanup, including when it is placed inside an artifact directory. For native store commands, put the marker at the store's cache root; an exclusion anywhere within the store also blocks its entire cleanup.

## Account for the whole disk

```sh
dev-sweep audit                          # root filesystem plus details within your home
dev-sweep audit --system                 # noninteractive sudo for system directory sizes
dev-sweep audit --root "$HOME/projects"  # focus the disk inventory
```

`audit` measures each top-level directory on the root filesystem separately, plus details within your home, and ranks directories two levels below those roots. It includes Docker's detailed images, containers, volumes, and build-cache report, Nix's unreferenced-path count, and journal usage. It labels installed SDKs, emulators, application history, and installed tool versions for review. Each directory inventory has a five-minute limit; partial measurements survive a timeout and other roots are still measured. Permission failures and timeouts make the report explicitly incomplete and return a failing exit code. Use `--root` for a narrower inventory on very large trees. `--system` measures protected directories through `sudo -n du`; it does not run a privileged cleanup.

Nested directory rows overlap and must not be added together. Hardlinks, Docker image layers, and copy-on-write files also make occupancy differ from reclaimable space. An unused Docker volume can still contain a database you want to keep. SDK images, virtual devices, toolchains, application histories, and old software versions are visible in the inventory and are retained.

## Clean managed stores

Preview any command without `--apply`:

```sh
dev-sweep clean-store npm                # full npm cache clear, including force flag
dev-sweep clean-store bun                # Bun's cache removal
dev-sweep clean-store pnpm               # pnpm's unused store pruning
dev-sweep clean-store uv                 # uv's unused cache pruning
dev-sweep clean-store nix                # Nix garbage collection, retain profile generations
dev-sweep clean-store docker-build       # unused Docker build cache older than days, keep 5 GB
dev-sweep clean-store apt                # sudo -n apt-get clean
dev-sweep clean-store journal            # archived logs, retention days and 1 GB cap
dev-sweep clean-store bun --apply         # actually run the selected cleanup
```

These commands use the manager's own locks and metadata. Native store prunes use the manager's definition of unused; they do not share the filesystem age rule. Full npm/Bun clears discard download caches regardless of age and require explicit `clean-store`. Nix GC keeps all registered roots and profile generations; it does not pass `-d` or delete rollback history. Apt and journal cleanup are explicit commands and require noninteractive sudo access. Journal vacuuming affects archived journals, with both the age and size limits applied.

`global_caches=true` includes npm cache verification/garbage collection, pnpm store pruning, uv cache pruning, and Nix GC in reports and an explicitly invoked `clean --apply`. It also discovers your own temporary projects/Cargo outputs on every run and includes stale individual npm npx cache entries, npm logs, and VS Code/Cursor extension download archives. Fresh entries, keep markers, exclusions, and symlink targets stay. `--no-global-caches` disables this group. Installed executables are found on PATH and common user installation paths.

With `docker=true`, reports include native cleanup commands for old unused images and build cache. Only your explicit approval in the TUI or `clean --apply` runs those commands. Docker volumes and containers stay. To explicitly delete one volume's data:

```sh
dev-sweep remove-volume exact_volume_name          # inspect and check container references
dev-sweep remove-volume exact_volume_name --apply  # delete that named volume
```

Volume removal refuses references from running or stopped containers and calls `docker volume rm` without force. No command automatically prunes all volumes.

## Cleanup rules

- `node_modules` must sit next to a `package.json`.
- `target` must sit next to a `Cargo.toml` and contain Cargo's `CACHEDIR.TAG` or `.rustc_info.json`.
- Shared or custom Cargo output directories also qualify with `.rustc_info.json` and a `debug/.fingerprint` or `release/.fingerprint` directory, even outside the source checkout. A configured root can itself be a Cargo output directory. Artifacts owned by another user stay.
- Next.js `.next` output requires a package manifest and a `BUILD_ID` or cache directory; `.turbo` requires a package manifest.
- Python `.venv` requires `pyproject.toml` and `pyvenv.cfg`. Python tool caches require `pyproject.toml`; `__pycache__` must contain only `.pyc`/`.pyo` entries.
- Every entry must be older than the age limit. File access times count too, so recently read binaries or dependencies stay. Scanning metadata does not read file contents. Directory access times are ignored because scanning updates them.
- Git-tracked artifacts, protected trees, unreadable trees, filesystem boundaries inside artifacts, and directories containing symlinks in their own path stay. Symlinks inside artifacts are measured without following their targets and are unlinked with the artifact directory.
- Projects with a current process working directory or executable inside them stay. Linux checks `/proc` for your user. macOS uses `lsof` for your user. Linux skips processes that deny inspection and prints a notice; these often include systemd and SSH agents. If process enumeration fails, the run stops.
- Docker image cleanup runs [`docker image prune --all --force --filter until=720h`](https://docs.docker.com/reference/cli/docker/image/prune/) at the default age. Docker preserves images referenced by any container, including stopped containers. Image age is **creation time**, not last use. Build-cache cleanup uses [`docker builder prune`](https://docs.docker.com/reference/cli/docker/builder/prune/) with the same duration and a 5 GB storage allowance.

Age and process checks are conservative heuristics, not proof that a directory is unused. A process can start during cleanup; use an exclusion or keep marker for long-lived services and important offline environments. Deleting caches means the next install/build may take longer. The byte report estimates allocated blocks; shared layers and hardlinks can reduce actual reclaimed space.

The Docker dry run prints the exact prune command rather than claiming to predict its result. Missing Docker is skipped; an installed Docker CLI with an unavailable daemon reports an error during cleanup. Set `docker=false` for machines without a working Docker daemon.

## Worktrees

`scan` lists idle linked worktrees eligible for manual removal. It checks filesystem and Git metadata activity, locks, changed files, untracked files, ignored files, and submodules. Tracked source freshness uses modification time, since Git inspection can update source access times; generated and untracked files still include access time. The checked-out commit must already be contained in the main checkout or a remote-tracking branch. Main and bare worktrees always stay. Remote-tracking refs reflect your last fetch, so this is a local reachability check.

```sh
dev-sweep remove-worktree /absolute/path/to/worktree          # recheck and preview
dev-sweep remove-worktree /absolute/path/to/worktree --apply  # recheck and remove
```

Removal uses [`git worktree remove`](https://git-scm.com/docs/git-worktree.html) without force. It keeps the branch and refuses dirty or locked worktrees. Recognized ignored generated directories, including node_modules and Cargo output, are removed with the approved checkout. They must pass ownership, protection, filesystem, process, tracking and age checks. Unknown ignored data such as .env files, untracked files and changed source block removal. No separate cache cleanup or force flag is needed. Automatic cleanup reports worktrees but never removes them.

## Automatic runs

The default installer schedules `scan`, with no `--apply`. Linux uses a systemd user timer at 03:00 with up to ten minutes of random delay. Missed runs happen after your user session resumes. The job uses the PATH captured during installation; rerun the installer if you move Docker or Git.

```sh
systemctl --user list-timers dev-sweep.timer
journalctl --user -u dev-sweep.service
systemctl --user disable --now dev-sweep.timer
```

macOS uses `~/Library/LaunchAgents/local.dev-sweep.plist` at 03:00 during your login session. Logs live in `~/Library/Logs/dev-sweep/`. Linux requires a systemd user session; the installer exits with instructions if one is unavailable.

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
python3 tests/tui_pty.py
python3 tests/scan_cancel_pty.py
python3 tests/scan_benchmark.py target/release/dev-sweep
```

Tests use temporary projects, worktrees, symlinks, package stores, and mocked Docker/manager/inventory commands. They check command timeouts, changed-candidate rechecks, malformed model responses, local HTTP metadata requests and TUI layout. The PTY check exercises grouped worktree caches, separate cache cleanup, cancellation, invalid and exact confirmation, source and branch preservation, and terminal restoration. They do not prune the host's Docker images, package stores, or projects.
