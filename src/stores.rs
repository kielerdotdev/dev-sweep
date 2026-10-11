use super::*;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

thread_local! {
    // The read-only inventory worker owns its cancellation context.
    static SCAN_CANCEL: std::cell::RefCell<Option<Arc<AtomicBool>>> = const { std::cell::RefCell::new(None) };
}

pub fn set_scan_cancel(cancel: Arc<AtomicBool>) {
    SCAN_CANCEL.with(|value| *value.borrow_mut() = Some(cancel));
}

pub fn scan_context() -> Option<Arc<AtomicBool>> {
    SCAN_CANCEL.with(|value| value.borrow().clone())
}

pub fn scan_cancelled() -> bool {
    SCAN_CANCEL.with(|value| {
        value
            .borrow()
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
    })
}

// Capture to files so slow commands cannot deadlock on a full output pipe.
// Native managers, rather than filesystem deletion, own their databases and locks.
pub fn capture(program: &str, args: &[OsString], seconds: u64) -> Result<Output> {
    capture_inner(program, args, seconds, false)
}

fn capture_inner(
    program: &str,
    args: &[OsString],
    seconds: u64,
    keep_partial: bool,
) -> Result<Output> {
    if scan_cancelled() {
        return Err("Scan cancelled".into());
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut dir;
    loop {
        dir = env::temp_dir().join(format!(
            "dev-sweep-command-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => break,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    let stdout = fs::File::create(dir.join("stdout"))?;
    let stderr = fs::File::create(dir.join("stderr"))?;
    let mut command = Command::new(executable(program).unwrap_or_else(|| PathBuf::from(program)));
    let mut child = command
        .args(args)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .process_group(0)
        .spawn()?;
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if scan_cancelled() || started.elapsed() >= Duration::from_secs(seconds) {
            timed_out = true;
            let group = format!("-{}", child.id());
            // TERM lets sudo forward the signal to its privileged command.
            // A separate process group also terminates shell grandchildren.
            let _ = Command::new("/bin/kill")
                .args(["-TERM", "--", &group])
                .stderr(Stdio::null())
                .status();
            let grace = Instant::now();
            while child.try_wait()?.is_none() && grace.elapsed() < Duration::from_secs(2) {
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = Command::new("/bin/kill")
                .args(["-KILL", "--", &group])
                .stderr(Stdio::null())
                .status();
            let _ = child.kill();
            break child.wait()?;
        }
        // Quick commands should not each pay a 50 ms polling delay.
        let delay = if started.elapsed() < Duration::from_millis(100) {
            2
        } else {
            50
        };
        std::thread::sleep(Duration::from_millis(delay));
    };
    let mut output = Output {
        status,
        stdout: fs::read(dir.join("stdout"))?,
        stderr: fs::read(dir.join("stderr"))?,
    };
    if timed_out {
        if scan_cancelled() {
            return Err("Scan cancelled".into());
        }
        let message = format!("{program} timed out after {seconds}s; the report is incomplete");
        if !keep_partial {
            return Err(message.into());
        }
        output.status = std::process::ExitStatus::from_raw(1 << 8);
        output
            .stderr
            .extend_from_slice(format!("\n{message}\n").as_bytes());
    }
    Ok(output)
}

fn strings(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

pub fn available(program: &str) -> bool {
    executable(program).is_some()
}

fn executable(program: &str) -> Option<PathBuf> {
    let mut roots: Vec<_> = env::split_paths(&env::var_os("PATH").unwrap_or_default()).collect();
    if let Some(home) = env::var_os("HOME") {
        roots.extend(
            [
                ".local/bin",
                ".cargo/bin",
                ".bun/bin",
                ".local/share/pnpm",
                ".nix-profile/bin",
            ]
            .iter()
            .map(|p| PathBuf::from(&home).join(p)),
        );
    }
    roots
        .into_iter()
        .map(|p| p.join(program))
        .find(|p| fs::metadata(p).is_ok_and(|m| m.is_file() && m.mode() & 0o111 != 0))
}

fn show_output(output: &Output) {
    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
}

type StorePlan = (String, Vec<OsString>, Option<PathBuf>);

pub fn store_plan(name: &str, config: &Config, home: &Path) -> Result<StorePlan> {
    let age = format!("until={}h", config.days * 24);
    let journal_age = format!("--vacuum-time={}d", config.days);
    let cache_home = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cache"));
    let (program, args, cache, query): (
        &str,
        Vec<OsString>,
        Option<PathBuf>,
        Option<Vec<OsString>>,
    ) = match name {
        "docker-images" => (
            "docker",
            strings(&["image", "prune", "--all", "--force", "--filter", &age]),
            None,
            None,
        ),
        "docker-build" => (
            "docker",
            strings(&[
                "builder",
                "prune",
                "--all",
                "--force",
                "--filter",
                &age,
                "--keep-storage",
                "5GB",
            ]),
            None,
            None,
        ),
        "npm" | "npm-unused" | "bun" | "pnpm" => {
            return Err(
                "Global npm, Bun and pnpm caches are protected; cleanup is disabled".into(),
            );
        }
        "uv" => (
            "uv",
            strings(&["cache", "prune"]),
            Some(cache_home.join("uv")),
            Some(strings(&["cache", "dir"])),
        ),
        "nix" => (
            "nix-store",
            strings(&["--gc"]),
            Some(PathBuf::from("/nix/store")),
            None,
        ),
        "apt" => (
            "sudo",
            strings(&["-n", "apt-get", "clean"]),
            Some(PathBuf::from("/var/cache/apt/archives")),
            None,
        ),
        "journal" => (
            "sudo",
            strings(&["-n", "journalctl", &journal_age, "--vacuum-size=1G"]),
            Some(PathBuf::from("/var/log/journal")),
            None,
        ),
        _ => return Err(format!("unknown store {name:?}; use --help").into()),
    };
    if !available(program) {
        return Err(format!("{program} is not installed").into());
    }
    let cache = if let Some(query) = query {
        let output = capture(program, &query, 30)?;
        if !output.status.success() {
            return Err(format!("cannot locate {name} cache").into());
        }
        let text = String::from_utf8(output.stdout)?.trim().to_string();
        let path = PathBuf::from(text);
        if !path.is_absolute() {
            return Err(format!("{name} returned an invalid cache path").into());
        }
        Some(path)
    } else {
        cache
    };
    if let Some(cache) = &cache {
        let resolved = if cache.exists() {
            fs::canonicalize(cache)?
        } else {
            cache.clone()
        };
        if config.protects(cache)
            || config.protects(&resolved)
            || config
                .excludes
                .iter()
                .any(|p| p.starts_with(cache) || p.starts_with(&resolved))
        {
            return Err("protected; skipped".into());
        }
    }
    Ok((program.into(), args, cache))
}

pub fn clean_store(name: &str, config: &Config, home: &Path, apply: bool) -> Result<()> {
    let (program, args, _) = match store_plan(name, config, home) {
        Ok(plan) => plan,
        Err(error)
            if error.to_string() == "protected; skipped"
                || error.to_string().ends_with("is not installed") =>
        {
            println!("STORE {name}: {error}; skipped");
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    println!(
        "STORE {name}: {} {}",
        if apply { "Run" } else { "Would run" },
        std::iter::once(OsString::from(&program))
            .chain(args.iter().cloned())
            .map(|a| format!("{:?}", a))
            .collect::<Vec<_>>()
            .join(" ")
    );
    if !apply {
        return Ok(());
    }
    let output = capture(&program, &args, 300)?;
    show_output(&output);
    if !output.status.success() {
        return Err(format!("{name} cleanup failed: {}", output.status).into());
    }
    Ok(())
}

pub fn remove_volume(name: &str, apply: bool) -> Result<()> {
    if name.is_empty()
        || name.starts_with('-')
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
    {
        return Err("invalid Docker volume name".into());
    }
    let exists = capture(
        "docker",
        &strings(&["volume", "inspect", "--format", "{{.Name}}", name]),
        30,
    )?;
    if !exists.status.success() {
        show_output(&exists);
        return Err("volume does not exist or cannot be inspected".into());
    }
    let refs = capture(
        "docker",
        &strings(&[
            "ps",
            "--all",
            "--quiet",
            "--filter",
            &format!("volume={name}"),
        ]),
        30,
    )?;
    if !refs.status.success() {
        show_output(&refs);
        return Err("cannot check volume references".into());
    }
    if !refs.stdout.is_empty() {
        return Err("volume is referenced by a running or stopped container".into());
    }
    println!(
        "{} Docker volume {name:?}; this deletes the volume's data",
        if apply { "Remove" } else { "Would remove" }
    );
    if apply {
        let result = capture("docker", &strings(&["volume", "rm", name]), 60)?;
        show_output(&result);
        if !result.status.success() {
            return Err("Docker refused volume removal".into());
        }
    }
    Ok(())
}

pub fn global_roots(home: &Path) -> Vec<PathBuf> {
    // Only these reproducible per-entry caches are removed directly.
    // Package databases, toolchains, Android images and application history use review/native commands.
    let mut roots = vec![
        home.join(".vscode-server/data/CachedExtensionVSIXs"),
        home.join(".cursor-server/data/CachedExtensionVSIXs"),
    ];
    roots.sort();
    roots.dedup();
    roots
}

pub fn global_stats(path: &Path, root: &Path, config: &Config, busy: &[PathBuf]) -> Result<Stats> {
    let scope = Config {
        roots: vec![root.to_path_buf()],
        ..config.clone()
    };
    if !scope.contains(path) || scope.excludes.iter().any(|p| p.starts_with(path)) {
        return Err("protected entry".into());
    }
    if busy
        .iter()
        .any(|p| p.starts_with(path) || p.starts_with(root))
    {
        return Err("cache entry is in use".into());
    }
    let meta = fs::symlink_metadata(path)?;
    let owner = fs::metadata(env::var_os("HOME").ok_or("HOME is not set")?)?.uid();
    if meta.uid() != owner
        || meta.file_type().is_symlink()
        || meta.dev() != fs::metadata(root)?.dev()
        || fs::canonicalize(path)? != path
    {
        return Err("different owner, symlink or filesystem boundary".into());
    }
    stats(path, &scope, false)
}

pub fn global_caches(config: &Config, home: &Path, busy: &[PathBuf], apply: bool) -> Result<()> {
    let roots = global_roots(home);
    let mut bytes = 0u64;
    let mut count = 0;
    for root in roots {
        if !root.is_dir()
            || config.protects(&root)
            || fs::symlink_metadata(&root)?.file_type().is_symlink()
        {
            continue;
        }
        let root = fs::canonicalize(root)?;
        for entry in fs::read_dir(&root)? {
            let path = entry?.path();
            let inspect = |busy: &[PathBuf]| global_stats(&path, &root, config, busy);
            let info = match inspect(busy) {
                Ok(info) => info,
                Err(_) => continue,
            };
            if !is_stale(&info, config.cutoff()) {
                continue;
            }
            println!("GLOBAL {:>10} {:?}", human_size(info.bytes), path);
            if apply {
                if !is_stale(&inspect(&busy_paths(false)?)?, config.cutoff()) {
                    continue;
                }
                let meta = fs::symlink_metadata(&path)?;
                if meta.is_dir() {
                    fs::remove_dir_all(&path)?;
                } else {
                    fs::remove_file(&path)?;
                }
            }
            bytes += info.bytes;
            count += 1;
        }
    }
    println!(
        "{} {count} global cache entries, {} estimated",
        if apply { "Removed" } else { "Found" },
        human_size(bytes)
    );
    Ok(())
}

pub fn audit(home: &Path, roots: Option<&Vec<PathBuf>>, system: bool, docker: bool) -> Result<()> {
    println!("Disk audit: allocated sizes, including retained data; nested rows overlap and must not be added together.");
    let disk = capture("df", &strings(&["-h", "/"]), 10)?;
    show_output(&disk);
    let selected = if let Some(roots) = roots {
        roots.clone()
    } else {
        let root_device = fs::metadata("/")?.dev();
        let mut roots = Vec::new();
        for entry in fs::read_dir("/")? {
            let entry = entry?;
            let meta = fs::symlink_metadata(entry.path())?;
            if meta.is_dir() && meta.dev() == root_device {
                roots.push(entry.path());
            }
        }
        roots.sort();
        // Bound each top-level directory separately so one large tree does not
        // prevent the audit from measuring the other storage areas.
        if home.is_dir() && !roots.iter().any(|root| root == home) {
            roots.push(home.to_path_buf());
        }
        roots
    };
    let mut rows = Vec::new();
    let mut incomplete = false;
    for root in selected {
        if !root.is_dir() {
            continue;
        }
        let mut args = if cfg!(target_os = "linux") {
            strings(&["-k", "-x", "--max-depth=2", "--null"])
        } else {
            strings(&["-k", "-x", "-d", "2"])
        };
        args.push(root.as_os_str().to_os_string());
        let program = if system && !root.starts_with(home) {
            args.insert(0, OsString::from("du"));
            args.insert(0, OsString::from("-n"));
            "sudo"
        } else {
            "du"
        };
        println!("Measuring {:?}...", root);
        match capture_inner(program, &args, 300, true) {
            Ok(output) => {
                if !output.status.success() {
                    incomplete = true;
                    eprint!("{}", String::from_utf8_lossy(&output.stderr));
                }
                let delimiter = if cfg!(target_os = "linux") { 0 } else { b'\n' };
                for line in output.stdout.split(|b| *b == delimiter) {
                    if let Some(tab) = line.iter().position(|b| *b == b'\t') {
                        if let Ok(size) = String::from_utf8_lossy(&line[..tab]).parse::<u64>() {
                            rows.push((
                                size * 1024,
                                PathBuf::from(OsString::from_vec(line[tab + 1..].to_vec())),
                            ));
                        }
                    }
                }
            }
            Err(error) => {
                eprintln!("Incomplete {:?}: {error}", root);
                incomplete = true;
            }
        }
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.0));
    for (bytes, path) in rows.iter().take(60) {
        println!("{:>10} {:?}  {}", human_size(*bytes), path, category(path));
    }
    println!("\nManaged stores (occupancy, not a promise of reclaimable space):");
    let mut probes = vec![
        ("nix-store", strings(&["--gc", "--print-dead"])),
        ("journalctl", strings(&["--disk-usage"])),
    ];
    if docker {
        probes.insert(0, ("docker", strings(&["system", "df", "--verbose"])));
    }
    for (program, args) in probes {
        if !available(program) {
            continue;
        }
        println!("{program}:");
        match capture(program, &args, if program == "docker" { 180 } else { 60 }) {
            Ok(output) if output.status.success() => {
                if program == "nix-store" {
                    println!("{} unreferenced Nix paths; clean-store nix uses GC without deleting profile generations", output.stdout.split(|b| *b == b'\n').filter(|p| !p.is_empty()).count());
                } else {
                    show_output(&output);
                }
            }
            Ok(output) => {
                show_output(&output);
                incomplete = true;
            }
            Err(error) => {
                eprintln!("{error}");
                incomplete = true;
            }
        }
    }
    if incomplete {
        return Err("audit partially complete; permission/timeout failures are shown above; --system enables noninteractive sudo for directory sizes".into());
    }
    Ok(())
}

fn category(path: &Path) -> &'static str {
    let text = path.to_string_lossy();
    if text.contains("/android") || text.contains("/.android") || text.contains("android-sdk") {
        "installed SDK/emulator data; review"
    } else if text.contains("/docker") || text.contains("/containerd") {
        "container-managed data; native prune or explicit volume removal"
    } else if text.starts_with("/nix") {
        "Nix-managed store; native GC"
    } else if text.starts_with("/var/tmp/") || text == "/var/tmp" {
        "temporary projects/builds; scan applies age/process checks"
    } else if text.contains("/sessions")
        || text.contains("/userdata")
        || text.contains("/opencode") && !text.contains("/.cache/")
    {
        "application history/data; retained"
    } else if text.contains("/_cacache")
        || text.contains("/_npx")
        || text.contains("/.bun/install")
        || text.contains("/.cache")
        || text.contains("CachedExtensionVSIXs")
    {
        "cache; scan age rules or clean-store"
    } else if text.contains("/toolchains")
        || text.contains("/extensions")
        || text.contains("/runtime")
        || text.contains("/versions")
        || text.contains("/.vite-plus")
    {
        "installed tools/versions; review"
    } else {
        "occupancy; scan determines cleanup eligibility"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelled_inventory_terminates_its_command_promptly() {
        let cancel = Arc::new(AtomicBool::new(false));
        set_scan_cancel(cancel.clone());
        let trigger = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            cancel.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        let result = capture("/bin/sh", &strings(&["-c", "exec sleep 30"]), 30);
        assert!(result.unwrap_err().to_string().contains("cancelled"));
        assert!(started.elapsed() < Duration::from_secs(3));
        trigger.join().unwrap();
        SCAN_CANCEL.with(|value| *value.borrow_mut() = None);
    }
    #[test]
    fn native_commands_time_out_without_pipe_deadlocks() {
        let result = capture("/bin/sh", &strings(&["-c", "exec sleep 30"]), 1);
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[test]
    fn timed_out_inventory_keeps_partial_output_and_terminates_children() {
        let output = capture_inner(
            "/bin/sh",
            &strings(&[
                "-c",
                "printf 'partial\\n'; sleep 30 & printf '%s\\n' $!; wait",
            ]),
            1,
            true,
        )
        .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.starts_with(b"partial\n"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("timed out"));
        let pid = String::from_utf8_lossy(&output.stdout)
            .lines()
            .nth(1)
            .unwrap()
            .to_owned();
        #[cfg(target_os = "linux")]
        if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
            assert!(stat.contains(") Z "), "child is still running: {stat}");
        }
        #[cfg(not(target_os = "linux"))]
        assert!(!Command::new("/bin/kill")
            .args(["-0", &pid])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success());
    }
}
