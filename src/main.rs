#[cfg(not(unix))]
compile_error!("dev-sweep currently supports Linux and macOS");

use std::collections::HashSet;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

mod advice;
mod inventory;
mod stores;
mod tui;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const HELP: &str = "dev-sweep: reclaim space from stale development artifacts

Usage: dev-sweep [tui|scan|audit|clean|init|clean-store STORE|remove-worktree PATH|remove-volume NAME] [OPTIONS]

  tui                     Browse storage, request AI advice, approve selections
  scan                    Preview stale caches and worktrees (default)
  audit                   Rank disk usage and report managed stores, even if kept
  clean --apply           Delete stale generated caches; prune enabled stores
  clean                   Same preview as scan
  init                    Discover projects and write a config, without overwriting
  remove-worktree PATH    Preview removal of one clean, idle linked worktree
  clean-store STORE       Preview a native store cleanup (use --apply to run)
  remove-volume NAME      Preview one unused Docker volume (use --apply to remove)

Stores: npm, npm-unused, bun, pnpm, uv, nix, docker-build, apt, journal

Options:
  --apply                 Actually delete (clean/remove-worktree only)
  --root PATH             Scan root, repeatable; replaces configured roots
  --exclude PATH          Protect a subtree, repeatable
  --days N                Minimum idle age, default 30, minimum 1
  --no-docker             Skip Docker entirely
  --no-global-caches      Skip global package/cache cleanup
  --system                Include privileged directory sizes in audit
  --config PATH           Config file (default ~/.config/dev-sweep/config)
  --help                  Show this help

Worktrees are NEVER removed by clean or the automatic job.
Create .dev-sweep-keep in any directory to protect its entire subtree.
";

#[derive(Clone)]
struct Config {
    roots: Vec<PathBuf>,
    excludes: Vec<PathBuf>,
    days: u64,
    docker: bool,
    global_caches: bool,
}

impl Config {
    fn defaults(home: &Path) -> Self {
        Self {
            roots: [
                "code",
                "projects",
                "dev",
                "research",
                "metadata",
                "CascadeProjects",
                ".t3/projects",
                ".t3/worktrees",
                ".t3/scratch",
                ".codex/worktrees",
                ".cache",
            ]
            .iter()
            .map(|p| home.join(p))
            .filter(|p| p.is_dir())
            .collect(),
            excludes: Vec::new(),
            days: 30,
            docker: true,
            global_caches: true,
        }
    }

    fn read(path: &Path, home: &Path) -> Result<Self> {
        let mut config = Self {
            roots: Vec::new(),
            ..Self::defaults(home)
        };
        for (line_number, line) in fs::read_to_string(path)?.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line.split_once('=').ok_or_else(|| {
                format!("{}:{}: expected key=value", path.display(), line_number + 1)
            })?;
            match key.trim() {
                "root" => config.roots.push(expand(value.trim(), home)?),
                "exclude" => config.excludes.push(expand(value.trim(), home)?),
                "days" => config.days = parse_days(value.trim())?,
                "docker" => config.docker = value.trim().parse()?,
                "global_caches" => config.global_caches = value.trim().parse()?,
                key => return Err(format!("unknown config key {key:?}").into()),
            }
        }
        Ok(config)
    }

    fn protects(&self, path: &Path) -> bool {
        self.excludes.iter().any(|p| path.starts_with(p))
            || path.ancestors().any(|p| p.join(".dev-sweep-keep").exists())
    }

    fn contains(&self, path: &Path) -> bool {
        self.roots.iter().any(|p| path.starts_with(p)) && !self.protects(path)
    }

    fn cutoff(&self) -> SystemTime {
        SystemTime::now() - Duration::from_secs(self.days * 86400)
    }

    fn normalize(&mut self, allow_root: bool) -> Result<()> {
        for root in &mut self.roots {
            if root.exists() {
                *root = fs::canonicalize(&*root)?;
            }
            if root.parent().is_none() && !allow_root {
                return Err("refusing to scan filesystem root".into());
            }
        }
        // Protect both a symlink's location and its resolved path.
        for path in self.excludes.clone() {
            if path.exists() {
                self.excludes.push(fs::canonicalize(path)?);
            }
        }
        self.excludes.sort();
        self.excludes.dedup();
        self.roots.sort();
        self.roots.dedup();
        let roots = self.roots.clone();
        self.roots
            .retain(|p| !roots.iter().any(|other| other != p && p.starts_with(other)));
        Ok(())
    }
}

fn expand(value: &str, home: &Path) -> Result<PathBuf> {
    if value.is_empty() {
        return Err("empty path".into());
    }
    let path = if let Some(rest) = value.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(value)
    };
    if !path.is_absolute()
        || path
            .components()
            .any(|p| p == std::path::Component::ParentDir)
    {
        return Err(format!("path must be absolute or start with ~/: {value:?}").into());
    }
    Ok(path)
}

fn parse_days(value: &str) -> Result<u64> {
    let days = value.parse::<u64>()?;
    if !(1..=36500).contains(&days) {
        return Err("days must be between 1 and 36500".into());
    }
    Ok(days)
}

fn main() {
    if let Err(error) = run() {
        eprintln!("dev-sweep: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args == ["--version"] || args == ["-V"] {
        println!("dev-sweep {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    let home = PathBuf::from(env::var_os("HOME").ok_or("HOME is not set")?);
    let config_home = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let mut config_path = config_home.join("dev-sweep/config");
    // Find the config first so every CLI option overrides it, regardless of order.
    for (i, arg) in args.iter().enumerate() {
        if arg == "--config" {
            config_path = expand(args.get(i + 1).ok_or("--config needs a path")?, &home)?;
        }
    }
    let mut config = if config_path.exists() {
        Config::read(&config_path, &home)?
    } else {
        Config::defaults(&home)
    };
    let mut action = if args.is_empty() && io::stdin().is_terminal() && io::stdout().is_terminal() {
        "tui"
    } else {
        "scan"
    };
    let mut apply = false;
    let mut roots = Vec::new();
    let mut worktree = None;
    let mut store = None;
    let mut volume = None;
    let mut system = false;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "tui" | "scan" | "audit" | "clean" | "init" | "remove-worktree" | "clean-store"
            | "remove-volume"
                if i == 0 =>
            {
                action = arg;
                if action == "remove-worktree" {
                    i += 1;
                    worktree = Some(expand(
                        args.get(i).ok_or("remove-worktree needs a path")?,
                        &home,
                    )?);
                }
                if action == "clean-store" || action == "remove-volume" {
                    i += 1;
                    let value = args.get(i).ok_or("command needs a store or volume name")?;
                    if action == "clean-store" {
                        store = Some(value.as_str());
                    } else {
                        volume = Some(value.as_str());
                    }
                }
            }
            "--apply" => apply = true,
            "--no-docker" => config.docker = false,
            "--no-global-caches" => config.global_caches = false,
            "--system" => system = true,
            "--config" | "--root" | "--exclude" | "--days" => {
                i += 1;
                let value = args.get(i).ok_or_else(|| format!("{arg} needs a value"))?;
                match arg.as_str() {
                    "--root" => roots.push(expand(value, &home)?),
                    "--exclude" => config.excludes.push(expand(value, &home)?),
                    "--days" => config.days = parse_days(value)?,
                    _ => {}
                }
            }
            _ => return Err(format!("unknown argument {arg:?}; use --help").into()),
        }
        i += 1;
    }
    if apply && !["clean", "remove-worktree", "clean-store", "remove-volume"].contains(&action) {
        return Err("--apply is only valid with a cleanup/removal command".into());
    }
    if system && action != "audit" {
        return Err("--system is only valid with audit".into());
    }
    let explicit_roots = !roots.is_empty();
    if !roots.is_empty() {
        config.roots = roots;
        // A one-off project scope must not prune unrelated package caches.
        config.global_caches = false;
    }
    if action == "init" {
        if config_path.exists() {
            return Err(format!("config already exists: {}", config_path.display()).into());
        }
        config.roots.extend(discover_roots(&home)?);
        config.roots.extend(discover_temporary_roots(&home));
        config.normalize(false)?;
        fs::create_dir_all(config_path.parent().ok_or("invalid config path")?)?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&config_path)?;
        writeln!(
            file,
            "# dev-sweep: paths are unquoted; root/exclude can repeat\ndays={}\ndocker={}\nglobal_caches={}",
            config.days, config.docker, config.global_caches
        )?;
        for path in &config.roots {
            writeln!(file, "root={}", path.display())?;
        }
        for path in &config.excludes {
            writeln!(file, "exclude={}", path.display())?;
        }
        println!(
            "Created {} with {} scan roots",
            config_path.display(),
            config.roots.len()
        );
        return Ok(());
    }
    if config.global_caches && ["scan", "clean", "tui"].contains(&action) {
        config.roots.extend(discover_temporary_roots(&home));
    }
    config.normalize(action == "audit")?;
    if action == "audit" {
        return stores::audit(
            &home,
            if explicit_roots {
                Some(&config.roots)
            } else {
                None
            },
            system,
            config.docker,
        );
    }
    if action == "clean-store" {
        return stores::clean_store(store.ok_or("missing store")?, &config, &home, apply);
    }
    if action == "remove-volume" {
        return stores::remove_volume(volume.ok_or("missing volume")?, apply);
    }
    if action == "tui" {
        return tui::run(config, home);
    }
    if config.roots.is_empty() {
        return Err("no scan roots; run init or pass --root PATH".into());
    }
    let busy = busy_paths(true)?;
    if action == "remove-worktree" {
        let path = fs::canonicalize(worktree.ok_or("missing worktree path")?)?;
        let trees = worktrees(&path)?;
        let tree = trees
            .iter()
            .find(|t| t.path == path)
            .ok_or("not a registered worktree root")?;
        check_worktree(tree, &trees, &config, &busy)?;
        println!(
            "{} worktree {:?}, including recognized ignored caches and build output; branch preserved",
            if apply { "Removing" } else { "Would remove" },
            path
        );
        if apply {
            git(
                &path,
                &[
                    OsStr::new("worktree"),
                    OsStr::new("remove"),
                    path.as_os_str(),
                ],
            )?;
        }
        return Ok(());
    }
    sweep(&config, &busy, apply)
}

fn skip_directory(name: &OsStr) -> bool {
    [
        ".git", ".cache", ".venv", "venv", ".next", ".nuxt", ".output", ".turbo", "vendor", "dist",
        "build",
    ]
    .iter()
    .any(|s| name == OsStr::new(s))
}

fn discover_roots(home: &Path) -> Result<Vec<PathBuf>> {
    // ponytail: bounded discovery; add deep or external locations with --root.
    let mut roots = Vec::new();
    for entry in fs::read_dir(home)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() || entry.file_name().as_encoded_bytes().starts_with(b".") {
            continue;
        }
        if has_project(&entry.path(), 0)? {
            roots.push(entry.path());
        }
    }
    Ok(roots)
}

fn has_project(path: &Path, depth: u32) -> Result<bool> {
    if path.join(".git").exists()
        || path.join("package.json").is_file()
        || path.join("Cargo.toml").is_file()
    {
        return Ok(true);
    }
    if depth >= 6 {
        return Ok(false);
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && !skip_directory(&entry.file_name())
            && entry.file_name() != "node_modules"
            && entry.file_name() != "target"
            && has_project(&entry.path(), depth + 1)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn discover_temporary_roots(home: &Path) -> Vec<PathBuf> {
    let Ok(owner) = fs::metadata(home).map(|m| m.uid()) else {
        return Vec::new();
    };
    let mut pending = vec![(env::temp_dir(), 0), (PathBuf::from("/var/tmp"), 0)];
    let mut roots = Vec::new();
    let mut seen = HashSet::new();
    while let Some((path, depth)) = pending.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if !meta.is_dir()
                || meta.uid() != owner
                || skip_directory(&entry.file_name())
                || entry.file_name() == "node_modules"
            {
                continue;
            }
            let child = entry.path();
            if child.join(".git").exists()
                || child.join("package.json").is_file()
                || child.join("Cargo.toml").is_file()
                || cargo_output(&child)
            {
                roots.push(child);
            } else if depth < 2 {
                pending.push((child, depth + 1));
            }
        }
    }
    roots
}

fn cargo_output(path: &Path) -> bool {
    path.join(".rustc_info.json").is_file()
        && (path.join("debug/.fingerprint").is_dir() || path.join("release/.fingerprint").is_dir())
}

#[derive(Default, Clone, Debug, PartialEq)]
struct Stats {
    bytes: u64,
    newest: Option<SystemTime>,
}

fn stats(path: &Path, config: &Config, worktree: bool) -> Result<Stats> {
    stats_inner(path, config, worktree, true)
}

fn stats_inner(
    path: &Path,
    config: &Config,
    worktree: bool,
    stop_when_fresh: bool,
) -> Result<Stats> {
    if config.protects(path) {
        return Err(format!("protected path {:?}", path).into());
    }
    let device = fs::symlink_metadata(path)?.dev();
    // Git status may read tracked source files. Their atime cannot distinguish
    // our inspection from user activity; use source mtime, Git activity and
    // process checks. Generated/untracked files still count access times.
    let tracked: HashSet<PathBuf> = if worktree {
        git(path, &[OsStr::new("ls-files"), OsStr::new("-z")])?
            .stdout
            .split(|b| *b == 0)
            .filter(|name| !name.is_empty())
            .map(|name| path.join(OsString::from_vec(name.to_vec())))
            .collect()
    } else {
        HashSet::new()
    };
    let mut total = Stats::default();
    let mut seen = HashSet::new();
    let mut pending = vec![path.to_path_buf()];
    while let Some(path) = pending.pop() {
        if stores::scan_cancelled() {
            return Err("Scan cancelled".into());
        }
        let meta = fs::symlink_metadata(&path)?;
        if meta.dev() != device {
            return Err(format!("different filesystem inside {:?}", path).into());
        }
        if config.excludes.iter().any(|p| path.starts_with(p))
            || (meta.is_dir() && path.join(".dev-sweep-keep").exists())
        {
            return Err(format!("protected path {:?}", path).into());
        }
        let mut modified = meta.modified()?;
        // Reading directory entries updates directory atime. Only use file atime.
        if meta.is_file() && !tracked.contains(&path) {
            modified = modified.max(meta.accessed()?);
        }
        total.newest = Some(total.newest.map_or(modified, |time| time.max(modified)));
        // One fresh entry rules out deletion; avoid walking an active dependency tree.
        if stop_when_fresh && modified >= config.cutoff() {
            return Ok(total);
        }
        if seen.insert((meta.dev(), meta.ino())) {
            total.bytes = total.bytes.saturating_add(meta.blocks() * 512);
        }
        if meta.is_dir() {
            for entry in fs::read_dir(&path)? {
                let entry = entry?;
                if worktree && entry.file_name() == ".git" {
                    continue;
                }
                pending.push(entry.path());
            }
        }
    }
    Ok(total)
}

fn is_stale(stats: &Stats, cutoff: SystemTime) -> bool {
    stats.newest.is_some_and(|time| time < cutoff)
}

fn artifact_kind(path: &Path) -> Option<&'static str> {
    let parent = path.parent()?;
    let name = path.file_name()?;
    if cargo_output(path) {
        Some("Cargo build output")
    } else if name == "node_modules" && parent.join("package.json").is_file() {
        Some("node_modules")
    } else if path.file_name()? == "target"
        && parent.join("Cargo.toml").is_file()
        && (path.join("CACHEDIR.TAG").is_file() || path.join(".rustc_info.json").is_file())
    {
        Some("cargo target")
    } else if name == ".next"
        && parent.join("package.json").is_file()
        && (path.join("BUILD_ID").is_file() || path.join("cache").is_dir())
    {
        Some("Next.js build/cache")
    } else if name == ".turbo" && parent.join("package.json").is_file() {
        Some("Turborepo cache")
    } else if name == ".venv"
        && parent.join("pyproject.toml").is_file()
        && path.join("pyvenv.cfg").is_file()
    {
        Some("Python environment")
    } else if name == "__pycache__"
        && fs::read_dir(path).ok()?.filter_map(|e| e.ok()).all(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x == "pyc" || x == "pyo")
        })
    {
        Some("Python bytecode")
    } else if [".pytest_cache", ".mypy_cache", ".ruff_cache"]
        .iter()
        .any(|p| name == OsStr::new(p))
        && parent.join("pyproject.toml").is_file()
    {
        Some("Python tool cache")
    } else {
        None
    }
}

fn artifact_stats(path: &Path, config: &Config, busy: &[PathBuf]) -> Result<Stats> {
    if !config.contains(path) {
        return Err("outside scan roots or protected".into());
    }
    let meta = fs::symlink_metadata(path)?;
    let home = PathBuf::from(env::var_os("HOME").ok_or("HOME is not set")?);
    if meta.uid() != fs::metadata(home)?.uid() {
        return Err("artifact belongs to another user".into());
    }
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("not a real directory".into());
    }
    if fs::canonicalize(path)? != path {
        return Err("path contains a symlink".into());
    }
    artifact_kind(path).ok_or("not a recognized generated directory")?;
    let parent = path.parent().ok_or("no parent")?;
    if meta.dev() != fs::metadata(parent)?.dev() {
        return Err("artifact is on a different filesystem from its project".into());
    }
    if busy.iter().any(|p| p.starts_with(parent)) {
        return Err("project is in use by a process".into());
    }
    if config.excludes.iter().any(|p| p.starts_with(path)) {
        return Err("contains excluded path".into());
    }
    if parent.ancestors().any(|p| p.join(".git").exists()) {
        let output = git(
            parent,
            &[
                OsStr::new("--literal-pathspecs"),
                OsStr::new("ls-files"),
                OsStr::new("-z"),
                OsStr::new("--"),
                path.file_name().ok_or("missing name")?,
            ],
        )?;
        if !output.stdout.is_empty() {
            return Err("contains Git-tracked files".into());
        }
    }
    stats(path, config, false)
}

fn find_artifacts(config: &Config) -> Result<(Vec<PathBuf>, HashSet<PathBuf>, Vec<String>)> {
    let mut pending = config.roots.clone();
    let home = PathBuf::from(env::var_os("HOME").ok_or("HOME is not set")?);
    let owner = fs::metadata(&home)?.uid();
    let mut repos = HashSet::new();
    let mut artifacts = Vec::new();
    let mut errors = Vec::new();
    while let Some(path) = pending.pop() {
        if stores::scan_cancelled() {
            return Err("Scan cancelled".into());
        }
        if config.protects(&path) {
            continue;
        }
        if artifact_kind(&path).is_some()
            && fs::symlink_metadata(&path).is_ok_and(|m| m.uid() == owner)
        {
            artifacts.push(path);
            continue;
        }
        let entries = match fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                errors.push(format!("Skipping {:?}: {error}", path));
                continue;
            }
        };
        if path.join(".git").exists() {
            repos.insert(path.clone());
        }
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    errors.push(format!("Scan error: {e}"));
                    continue;
                }
            };
            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(e) => {
                    errors.push(format!("Scan error: {e}"));
                    continue;
                }
            };
            if !file_type.is_dir() {
                continue;
            }
            let child = entry.path();
            if (child.starts_with(env::temp_dir()) || child.starts_with("/var/tmp"))
                && !fs::symlink_metadata(&child).is_ok_and(|m| m.uid() == owner)
            {
                continue;
            }
            if artifact_kind(&child).is_some() {
                artifacts.push(child);
            } else if entry.file_name() == "node_modules" || entry.file_name() == "target" {
                continue;
            } else if !skip_directory(&entry.file_name()) {
                pending.push(child);
            }
        }
    }
    artifacts.sort();
    artifacts.dedup();
    Ok((artifacts, repos, errors))
}

fn sweep(config: &Config, busy: &[PathBuf], apply: bool) -> Result<()> {
    println!(
        "{}; minimum age {} days; {} roots",
        if apply { "Cleanup" } else { "Dry run" },
        config.days,
        config.roots.len()
    );
    let (artifacts, repos, messages) = find_artifacts(config)?;
    let mut errors = messages.len();
    for message in messages {
        eprintln!("{message}");
    }
    let mut bytes: u64 = 0;
    let mut count = 0;
    for path in artifacts {
        match artifact_stats(&path, config, busy) {
            Ok(info) if is_stale(&info, config.cutoff()) => {
                println!(
                    "{} {:>10}  {:?}",
                    if apply { "DELETE" } else { "CACHE " },
                    human_size(info.bytes),
                    path
                );
                if apply {
                    // Recheck before deletion; do not reuse the scan's age or type checks.
                    match artifact_stats(&path, config, &busy_paths(false)?) {
                        Ok(current) if is_stale(&current, config.cutoff()) => {
                            if let Err(error) = fs::remove_dir_all(&path) {
                                eprintln!("Failed {:?}: {error}", path);
                                errors += 1;
                                continue;
                            }
                        }
                        Ok(_) => {
                            eprintln!("Kept {:?}: became active", path);
                            continue;
                        }
                        Err(error) => {
                            eprintln!("Kept {:?}: {error}", path);
                            continue;
                        }
                    }
                }
                bytes = bytes.saturating_add(info.bytes);
                count += 1;
            }
            Ok(_) => {}
            Err(error) => eprintln!("Kept {:?}: {error}", path),
        }
    }
    println!(
        "{} {count} cache directories, {} estimated",
        if apply { "Removed" } else { "Found" },
        human_size(bytes)
    );
    let mut seen = HashSet::new();
    let mut tree_count = 0;
    for repo in repos {
        let trees = match worktrees(&repo) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("Worktree scan {:?}: {e}", repo);
                errors += 1;
                continue;
            }
        };
        for tree in &trees {
            if !seen.insert(tree.path.clone())
                || !config.contains(&tree.path)
                || trees.first().is_some_and(|t| t.path == tree.path)
            {
                continue;
            }
            match check_worktree(tree, &trees, config, busy) {
                Ok(_) => {
                    println!("WORKTREE {:?}  (manual removal only)", tree.path);
                    tree_count += 1;
                }
                Err(error) => eprintln!("Kept worktree {:?}: {error}", tree.path),
            }
        }
    }
    println!("Found {tree_count} idle worktrees eligible for explicit removal");
    if config.docker {
        for name in ["docker-images", "docker-build"] {
            if let Err(error) = stores::clean_store(
                name,
                config,
                &PathBuf::from(env::var_os("HOME").ok_or("HOME is not set")?),
                apply,
            ) {
                eprintln!("{name}: {error}");
                errors += 1;
            }
        }
    }
    if config.global_caches {
        let home = PathBuf::from(env::var_os("HOME").ok_or("HOME is not set")?);
        if let Err(error) = stores::global_caches(config, &home, busy, apply) {
            eprintln!("Global caches: {error}");
            errors += 1;
        }
        for name in ["npm-unused", "pnpm", "uv", "nix"] {
            if let Err(error) = stores::clean_store(name, config, &home, apply) {
                eprintln!("{name}: {error}");
                errors += 1;
            }
        }
    }
    if errors > 0 {
        return Err(format!("{errors} operations failed; see output above").into());
    }
    Ok(())
}

fn human_size(bytes: u64) -> String {
    let mut value = bytes as f64;
    for unit in ["B", "KiB", "MiB", "GiB", "TiB"] {
        if value < 1024.0 || unit == "TiB" {
            return format!("{value:.1} {unit}");
        }
        value /= 1024.0;
    }
    unreachable!()
}

fn git(path: &Path, args: &[&OsStr]) -> Result<Output> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()?;
    if !output.status.success() {
        return Err(format!("git: {}", String::from_utf8_lossy(&output.stderr).trim()).into());
    }
    Ok(output)
}

#[derive(Default)]
struct Worktree {
    path: PathBuf,
    locked: bool,
    bare: bool,
}

fn worktrees(path: &Path) -> Result<Vec<Worktree>> {
    let output = git(
        path,
        &[
            OsStr::new("worktree"),
            OsStr::new("list"),
            OsStr::new("--porcelain"),
            OsStr::new("-z"),
        ],
    )?;
    Ok(parse_worktrees(&output.stdout))
}

fn parse_worktrees(bytes: &[u8]) -> Vec<Worktree> {
    let mut trees = Vec::new();
    let mut current: Option<Worktree> = None;
    for field in bytes.split(|b| *b == 0) {
        if let Some(path) = field.strip_prefix(b"worktree ") {
            if let Some(tree) = current.take() {
                trees.push(tree);
            }
            current = Some(Worktree {
                path: PathBuf::from(OsString::from_vec(path.to_vec())),
                ..Worktree::default()
            });
        } else if let Some(tree) = &mut current {
            if field == b"locked" || field.starts_with(b"locked ") {
                tree.locked = true;
            }
            if field == b"bare" {
                tree.bare = true;
            }
        }
    }
    if let Some(tree) = current {
        trees.push(tree);
    }
    trees
}

fn check_worktree(
    tree: &Worktree,
    trees: &[Worktree],
    config: &Config,
    busy: &[PathBuf],
) -> Result<Stats> {
    let path = &tree.path;
    let main = trees.first().ok_or("empty worktree list")?;
    if tree.bare || main.path == *path {
        return Err("main/bare worktree".into());
    }
    if tree.locked {
        return Err("locked".into());
    }
    if !config.contains(path) {
        return Err("outside scan roots or protected".into());
    }
    if fs::canonicalize(path)? != *path {
        return Err("path contains a symlink".into());
    }
    if busy.iter().any(|p| p.starts_with(path)) {
        return Err("in use by a process".into());
    }
    if config.excludes.iter().any(|p| p.starts_with(path)) {
        return Err("contains excluded path".into());
    }
    let info = stats(path, config, true)?;
    if !is_stale(&info, config.cutoff()) {
        return Err("recent filesystem activity".into());
    }
    // Check metadata before Git commands, which can read the index. mtime only.
    let metadata = git(
        path,
        &[OsStr::new("rev-parse"), OsStr::new("--absolute-git-dir")],
    )?;
    let gitdir = path_from_line(&metadata.stdout);
    for name in ["index", "HEAD", "logs/HEAD"] {
        if let Ok(meta) = fs::metadata(gitdir.join(name)) {
            if meta.modified()? >= config.cutoff() {
                return Err("recent Git activity".into());
            }
        }
    }
    let status = git(
        path,
        &[
            OsStr::new("status"),
            OsStr::new("--porcelain=v1"),
            OsStr::new("-z"),
            OsStr::new("--untracked-files=normal"),
            OsStr::new("--ignored=matching"),
        ],
    )?;
    let mut inspected = HashSet::new();
    for entry in status
        .stdout
        .split(|b| *b == 0)
        .filter(|entry| !entry.is_empty())
    {
        if !entry.starts_with(b"!! ") {
            return Err("contains changed or untracked files".into());
        }
        let relative = PathBuf::from(OsString::from_vec(entry[3..].to_vec()));
        if relative.is_absolute()
            || relative
                .components()
                .any(|p| !matches!(p, std::path::Component::Normal(_)))
        {
            return Err("invalid ignored path from Git".into());
        }
        let ignored = path.join(relative);
        let artifact = ignored
            .ancestors()
            .take_while(|p| *p != path)
            .find(|p| artifact_kind(p).is_some())
            .ok_or_else(|| format!("contains unrecognized ignored data: {:?}", ignored))?;
        if inspected.insert(artifact.to_path_buf())
            && !is_stale(&artifact_stats(artifact, config, busy)?, config.cutoff())
        {
            return Err("recent generated-file activity".into());
        }
    }
    // A submodule's files can be clean yet have commits only stored locally.
    if path.join(".gitmodules").exists() {
        return Err("contains submodules".into());
    }
    let head = git(path, &[OsStr::new("rev-parse"), OsStr::new("HEAD")])?;
    let hash = String::from_utf8(head.stdout)?.trim().to_string();
    let remotes = git(
        path,
        &[
            OsStr::new("for-each-ref"),
            OsStr::new("--format=%(refname)"),
            OsStr::new("--contains"),
            OsStr::new(&hash),
            OsStr::new("refs/remotes/"),
        ],
    )?;
    let main_contains = !main.bare
        && git(
            &main.path,
            &[
                OsStr::new("merge-base"),
                OsStr::new("--is-ancestor"),
                OsStr::new(&hash),
                OsStr::new("HEAD"),
            ],
        )
        .is_ok();
    if remotes.stdout.is_empty() && !main_contains {
        return Err("HEAD is not in the main checkout or a remote-tracking branch".into());
    }
    Ok(info)
}

fn path_from_line(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsString::from_vec(
        bytes.strip_suffix(b"\n").unwrap_or(bytes).to_vec(),
    ))
}

#[cfg(target_os = "linux")]
fn busy_paths(verbose: bool) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let mut denied = false;
    let own = std::process::id().to_string();
    let uid = fs::metadata("/proc/self")?.uid();
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let name = entry.file_name();
        if name == OsStr::new(&own) || !name.as_encoded_bytes().iter().all(u8::is_ascii_digit) {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if meta.uid() != uid {
            continue;
        }
        for item in ["cwd", "exe"] {
            match fs::read_link(entry.path().join(item)) {
                Ok(path) => paths.push(path),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) if error.kind() == io::ErrorKind::PermissionDenied => denied = true,
                Err(error) => return Err(format!("cannot check process activity: {error}").into()),
            }
        }
    }
    if denied && verbose {
        eprintln!("Some processes deny inspection; use exclusions for long-lived services.");
    }
    Ok(paths)
}

#[cfg(target_os = "macos")]
fn busy_paths(_verbose: bool) -> Result<Vec<PathBuf>> {
    let uid = fs::metadata(env::var_os("HOME").ok_or("HOME is not set")?)?
        .uid()
        .to_string();
    let output = Command::new("lsof")
        .args(["-a", "-u", &uid, "-d", "cwd,txt", "-Fpn"])
        .output()?;
    if !output.status.success() {
        return Err("lsof failed; cannot check process activity".into());
    }
    let mut own = false;
    let pid = std::process::id().to_string();
    let mut paths = Vec::new();
    for line in output.stdout.split(|b| *b == b'\n') {
        if let Some(id) = line.strip_prefix(b"p") {
            own = id == pid.as_bytes();
        }
        if !own {
            if let Some(name) = line.strip_prefix(b"n") {
                paths.push(PathBuf::from(OsString::from_vec(name.to_vec())));
            }
        }
    }
    Ok(paths)
}
