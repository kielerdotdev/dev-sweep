use std::fs::{self, File, FileTimes};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "dev-sweep-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn file(&self, path: &str, content: &str) -> PathBuf {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        path
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_dev-sweep"))
            .args(args)
            .args(["--config", self.0.join("config").to_str().unwrap()])
            .args([
                "--root",
                self.0.to_str().unwrap(),
                "--no-docker",
                "--no-global-caches",
            ])
            .current_dir(std::env::temp_dir())
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
fn age(path: &Path) {
    let meta = fs::symlink_metadata(path).unwrap();
    if meta.is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            age(&entry.unwrap().path());
        }
    }
    if meta.file_type().is_symlink() {
        assert!(Command::new("touch")
            .args(["-h", "-t", "200001010000"])
            .arg(path)
            .status()
            .unwrap()
            .success());
    } else {
        let old = SystemTime::now() - Duration::from_secs(90 * 86400);
        File::open(path)
            .unwrap()
            .set_times(FileTimes::new().set_accessed(old).set_modified(old))
            .unwrap();
    }
}
fn git(path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    success(&output);
}

#[test]
fn caches_keep_source_fresh_tracked_protected_and_active_data() {
    let f = Fixture::new();
    f.file("old/package.json", "{}");
    f.file("old/node_modules/dependency/index.js", "stale");
    f.file("rust/Cargo.toml", "[package]");
    f.file("rust/target/.rustc_info.json", "{}");
    f.file("rust/target/debug/artifact", "stale");
    f.file("fake/Cargo.toml", "[package]");
    f.file("fake/target/source.rs", "keep");
    f.file("fresh/package.json", "{}");
    f.file("fresh/node_modules/file", "old parent, fresh child");
    f.file("accessed/package.json", "{}");
    f.file("accessed/node_modules/file", "recently read");
    f.file("protected/package.json", "{}");
    f.file("protected/node_modules/file", "keep");
    f.file("protected/.dev-sweep-keep", "");
    f.file("nested/package.json", "{}");
    f.file("nested/node_modules/deep/.dev-sweep-keep", "");
    f.file("excluded/package.json", "{}");
    f.file("excluded/node_modules/deep/file", "keep");
    f.file("active/package.json", "{}");
    f.file("active/node_modules/file", "keep");
    f.file("tracked/package.json", "{}");
    f.file("tracked/node_modules/source", "keep tracked files");
    git(&f.0.join("tracked"), &["init"]);
    git(&f.0.join("tracked"), &["add", "."]);
    git(&f.0.join("tracked"), &["commit", "-m", "initial"]);
    let external = Fixture::new();
    external.file("important", "never delete");
    symlink(&external.0, f.0.join("old/node_modules/link")).unwrap();
    f.file("linked/package.json", "{}");
    symlink(&external.0, f.0.join("linked/node_modules")).unwrap();
    f.file("alias/package.json", "{}");
    fs::create_dir(f.0.join("alias/node_modules")).unwrap();
    symlink(&external.0, f.0.join("alias/node_modules/link")).unwrap();
    f.file(
        "config",
        &format!(
            "days=30\ndocker=false\nexclude={}\nexclude={}\n",
            f.0.join("excluded/node_modules/deep").display(),
            f.0.join("alias/node_modules/link").display()
        ),
    );
    age(&f.0);
    fs::write(f.0.join("fresh/node_modules/file"), "new").unwrap();
    File::open(f.0.join("accessed/node_modules/file"))
        .unwrap()
        .set_times(FileTimes::new().set_accessed(SystemTime::now()))
        .unwrap();
    let mut active = Command::new("sleep")
        .arg("60")
        .current_dir(f.0.join("active"))
        .spawn()
        .unwrap();
    let preview = f.run(&["scan"]);
    success(&preview);
    assert!(
        f.0.join("old/node_modules").exists(),
        "dry run must leave files alone"
    );
    assert!(String::from_utf8_lossy(&preview.stdout).contains("Found 2 cache directories"));
    let result = f.run(&["clean", "--apply"]);
    active.kill().unwrap();
    active.wait().unwrap();
    success(&result);
    assert!(!f.0.join("old/node_modules").exists());
    assert!(!f.0.join("rust/target").exists());
    assert!(f.0.join("old/package.json").exists());
    assert!(external.0.join("important").exists());
    for project in [
        "fake",
        "fresh",
        "accessed",
        "protected",
        "nested",
        "excluded",
        "active",
        "tracked",
        "linked",
        "alias",
    ] {
        let artifact = if project == "fake" {
            "target"
        } else {
            "node_modules"
        };
        assert!(
            f.0.join(project).join(artifact).exists(),
            "{project} must be kept"
        );
    }
}

#[test]
fn corrupt_repository_blocks_nested_artifact_deletion() {
    let f = Fixture::new();
    f.file("broken/.git", "gitdir: /dev-sweep-nonexistent-git-dir\n");
    f.file("broken/pkg/package.json", "{}");
    f.file(
        "broken/pkg/node_modules/source",
        "keep when Git checks fail",
    );
    age(&f.0);
    assert!(!f.run(&["clean", "--apply"]).status.success());
    assert!(f.0.join("broken/pkg/node_modules/source").exists());
}

#[test]
fn worktrees_require_age_cleanliness_reachability_and_explicit_removal() {
    let f = Fixture::new();
    let repo = f.0.join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "--initial-branch=main"]);
    fs::write(repo.join("source"), "keep source").unwrap();
    fs::write(repo.join(".gitignore"), "ignored\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "initial"]);
    for name in [
        "clean with space\nand newline",
        "dirty",
        "locked",
        "unique",
        "ignored",
        "recent",
        "marker",
    ] {
        let branch = if name.starts_with("clean") {
            "clean"
        } else {
            name
        };
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                branch,
                f.0.join(name).to_str().unwrap(),
            ],
        );
    }
    fs::write(f.0.join("dirty/untracked"), "keep").unwrap();
    fs::write(f.0.join("ignored/ignored"), "keep").unwrap();
    git(
        &repo,
        &["worktree", "lock", f.0.join("locked").to_str().unwrap()],
    );
    fs::write(f.0.join("unique/source"), "unmerged work").unwrap();
    git(&f.0.join("unique"), &["commit", "-am", "unmerged"]);
    f.file("marker/target/.dev-sweep-keep", "");
    git(&f.0.join("marker"), &["add", "."]);
    git(
        &f.0.join("marker"),
        &["commit", "-m", "protect nested source directory"],
    );
    git(&repo, &["merge", "--ff-only", "marker"]);
    f.file("config", "days=30\ndocker=false\n");
    age(&f.0);
    fs::write(f.0.join("recent/source"), "recent change").unwrap();
    let output = f.run(&["scan"]);
    success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("Found 1 idle worktrees"));
    success(&f.run(&["clean", "--apply"]));
    assert!(
        f.0.join("clean with space\nand newline").exists(),
        "automatic clean must not remove worktrees"
    );
    // Git status may read tracked files; restore the fixture's idle timestamps.
    age(&f.0);
    for name in ["repo", "dirty", "locked", "unique", "ignored", "marker"] {
        let output = f.run(&[
            "remove-worktree",
            f.0.join(name).to_str().unwrap(),
            "--apply",
        ]);
        assert!(!output.status.success(), "must reject {name}");
        assert!(f.0.join(name).exists());
    }
    let clean = f.0.join("clean with space\nand newline");
    success(&f.run(&["remove-worktree", clean.to_str().unwrap()]));
    assert!(clean.exists());
    age(&f.0);
    success(&f.run(&["remove-worktree", clean.to_str().unwrap(), "--apply"]));
    assert!(!clean.exists());
    git(&repo, &["show-ref", "--verify", "refs/heads/clean"]);
}

#[test]
fn config_validation_and_discovery() {
    let f = Fixture::new();
    f.file("home/unusual location/deep/project/package.json", "{}");
    f.file("home/.t3/worktrees/tree/Cargo.toml", "");
    let home = f.0.join("home");
    let config = f.0.join("created config");
    let init = || {
        Command::new(env!("CARGO_BIN_EXE_dev-sweep"))
            .args(["init", "--config"])
            .arg(&config)
            .env("HOME", &home)
            .output()
            .unwrap()
    };
    success(&init());
    let contents = fs::read_to_string(&config).unwrap();
    assert!(contents.contains(home.join("unusual location").to_str().unwrap()));
    assert!(contents.contains(home.join(".t3/worktrees").to_str().unwrap()));
    assert!(!init().status.success(), "must not overwrite config");
    assert_eq!(contents, fs::read_to_string(&config).unwrap());
    for args in [
        vec!["scan", "--apply"],
        vec!["clean", "--days", "0"],
        vec!["scan", "--root", "/"],
        vec!["scan", "--days", "99999999999999"],
        vec!["scan", "--unknown"],
    ] {
        assert!(!f.run(&args).status.success());
    }
    f.file("config", "dayz=30\n");
    assert!(
        !f.run(&["scan"]).status.success(),
        "unknown config keys must fail"
    );
}

#[test]
fn docker_only_prunes_age_filtered_images_and_dry_run_does_not_call_it() {
    let f = Fixture::new();
    let log = f.0.join("docker-args");
    let docker = f.file(
        "bin/docker",
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$DEV_SWEEP_TEST_LOG\"\n",
    );
    fs::set_permissions(docker, fs::Permissions::from_mode(0o755)).unwrap();
    f.file("config", "days=45\ndocker=true\nglobal_caches=false\n");
    let run = |action: &str, apply: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dev-sweep"));
        command
            .arg(action)
            .args(["--config", f.0.join("config").to_str().unwrap()])
            .args(["--root", f.0.to_str().unwrap()])
            .env("PATH", f.0.join("bin"))
            .env("DEV_SWEEP_TEST_LOG", &log);
        if apply {
            command.arg("--apply");
        }
        command.output().unwrap()
    };
    success(&run("scan", false));
    assert!(!log.exists());
    success(&run("clean", true));
    assert_eq!(
        fs::read_to_string(&log).unwrap(),
        "image\nprune\n--all\n--force\n--filter\nuntil=1080h\nbuilder\nprune\n--all\n--force\n--filter\nuntil=1080h\n--keep-storage\n5GB\n"
    );
    f.file("bin/docker", "#!/bin/sh\nexit 1\n");
    assert!(
        !run("clean", true).status.success(),
        "prune failures must propagate"
    );
}

#[test]
fn additional_generated_caches_require_markers_and_preserve_source() {
    let f = Fixture::new();
    f.file("web/package.json", "{}");
    f.file("web/.next/cache/compiled", "old");
    f.file("web/.turbo/cache", "old");
    f.file("python/pyproject.toml", "[project]");
    f.file("python/.venv/pyvenv.cfg", "home=/usr/bin");
    f.file("python/.venv/lib/dependency", "old");
    f.file("python/.pytest_cache/cache", "old");
    f.file("python/.ruff_cache/cache", "old");
    f.file("python/__pycache__/source.cpython.pyc", "old bytecode");
    f.file("python/source.py", "keep source");
    f.file("unknown/.venv/custom-data", "keep without marker");
    f.file(
        "unknown/__pycache__/source.py",
        "keep source despite directory name",
    );
    f.file("unknown/.next/source", "keep without manifest");
    f.file("shared-rust/.rustc_info.json", "{}");
    f.file("shared-rust/debug/.fingerprint/fingerprint", "generated");
    f.file("shared-rust/debug/build/binary", "generated");
    age(&f.0);
    success(&f.run(&["clean", "--apply"]));
    for cache in [
        "web/.next",
        "web/.turbo",
        "python/.venv",
        "python/.pytest_cache",
        "python/.ruff_cache",
        "python/__pycache__",
        "shared-rust",
    ] {
        assert!(!f.0.join(cache).exists(), "{cache} should be removed");
    }
    for source in [
        "python/source.py",
        "unknown/.venv/custom-data",
        "unknown/__pycache__/source.py",
        "unknown/.next/source",
    ] {
        assert!(f.0.join(source).exists(), "{source} should stay");
    }
    // A shared Cargo output can itself be a configured scan root.
    let direct = f
        .file("direct-rust/.rustc_info.json", "{}")
        .parent()
        .unwrap()
        .to_path_buf();
    f.file("direct-rust/debug/.fingerprint/file", "generated");
    age(&direct);
    let result = Command::new(env!("CARGO_BIN_EXE_dev-sweep"))
        .args(["clean", "--apply", "--no-docker", "--root"])
        .arg(&direct)
        .arg("--config")
        .arg(f.0.join("config"))
        .current_dir(std::env::temp_dir())
        .output()
        .unwrap();
    success(&result);
    assert!(!direct.exists());
}

#[test]
fn global_cache_entries_obey_age_markers_and_symlink_protection() {
    let f = Fixture::new();
    let home = f.0.join("home");
    fs::create_dir_all(home.join("projects")).unwrap();
    fs::create_dir_all(home.join("tmp")).unwrap();
    f.file("home/.npm/_npx/old/node_modules/file", "old");
    f.file("home/.npm/_npx/fresh/file", "fresh");
    f.file("home/.npm/_npx/kept/.dev-sweep-keep", "");
    f.file(
        "home/.vscode-server/data/CachedExtensionVSIXs/old.vsix",
        "download",
    );
    let external = Fixture::new();
    external.file("important", "keep");
    symlink(&external.0, home.join(".npm/_npx/link")).unwrap();
    f.file(
        "config",
        &format!(
            "root={}\ndocker=false\nglobal_caches=true\nexclude=/var/tmp\n",
            home.join("projects").display()
        ),
    );
    age(&f.0);
    fs::write(home.join(".npm/_npx/fresh/file"), "new").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_dev-sweep"))
        .args(["clean", "--apply", "--config"])
        .arg(f.0.join("config"))
        .env("HOME", &home)
        .env("TMPDIR", home.join("tmp"))
        .env("PATH", f.0.join("empty-bin"))
        .current_dir(std::env::temp_dir())
        .output()
        .unwrap();
    success(&result);
    assert!(home.join(".npm/_npx/old/node_modules/file").exists());
    assert!(!home
        .join(".vscode-server/data/CachedExtensionVSIXs/old.vsix")
        .exists());
    assert!(home.join(".npm/_npx/fresh/file").exists());
    assert!(home.join(".npm/_npx/kept/.dev-sweep-keep").exists());
    assert!(home.join(".npm/_npx/link").exists());
    assert!(external.0.join("important").exists());
}

#[test]
fn native_package_cleanup_uses_resolved_cache_paths_and_honors_exclusions() {
    let f = Fixture::new();
    let cache = f.0.join("custom uv cache");
    fs::create_dir(&cache).unwrap();
    let script = f.file("bin/uv", "#!/bin/sh\nif [ \"$2\" = dir ]; then printf '%s\\n' \"$DEV_SWEEP_TEST_CACHE\"; else printf '%s\\n' \"$*\" >> \"$DEV_SWEEP_TEST_LOG\"; fi\n");
    fs::set_permissions(script, fs::Permissions::from_mode(0o755)).unwrap();
    let log = f.0.join("calls");
    let run = |apply: bool| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_dev-sweep"));
        cmd.args(["clean-store", "uv", "--config"])
            .arg(f.0.join("config"))
            .env("HOME", f.0.join("home"))
            .env("PATH", f.0.join("bin"))
            .env("DEV_SWEEP_TEST_CACHE", &cache)
            .env("DEV_SWEEP_TEST_LOG", &log);
        if apply {
            cmd.arg("--apply");
        }
        cmd.output().unwrap()
    };
    success(&run(false));
    assert!(!log.exists());
    success(&run(true));
    assert_eq!(fs::read_to_string(&log).unwrap(), "cache prune\n");
    fs::remove_file(&log).unwrap();
    f.file(
        "config",
        &format!("exclude={}/important-subtree\n", cache.display()),
    );
    success(&run(true));
    assert!(
        !log.exists(),
        "excluded descendants block whole-store cleanup"
    );
    f.file("config", "");
    fs::write(cache.join(".dev-sweep-keep"), "").unwrap();
    success(&run(true));
    assert!(!log.exists(), "keep marker blocks whole-store cleanup");
}

#[test]
fn global_node_cache_cleanup_is_disabled_even_with_apply() {
    let f = Fixture::new();
    for name in ["npm", "npm-unused", "bun", "pnpm"] {
        let script = f.file(
            &format!("bin/{name}"),
            "#!/bin/sh\necho called >> \"$DEV_SWEEP_TEST_LOG\"\n",
        );
        fs::set_permissions(script, fs::Permissions::from_mode(0o755)).unwrap();
        for apply in [false, true] {
            let mut cmd = Command::new(env!("CARGO_BIN_EXE_dev-sweep"));
            cmd.args(["clean-store", name, "--config"])
                .arg(f.0.join("config"))
                .env("PATH", f.0.join("bin"))
                .env("DEV_SWEEP_TEST_LOG", f.0.join("calls"));
            if apply {
                cmd.arg("--apply");
            }
            assert!(!cmd.output().unwrap().status.success());
            assert!(!f.0.join("calls").exists(), "must not invoke {name}");
        }
    }
}

#[test]
fn docker_volume_removal_is_explicit_and_refuses_container_references() {
    let f = Fixture::new();
    let script = f.file("bin/docker", "#!/bin/sh\nif [ \"$1 $2\" = 'volume inspect' ]; then echo volume; elif [ \"$1\" = ps ]; then if [ \"$DEV_SWEEP_TEST_ACTIVE\" = 1 ]; then echo container; fi; elif [ \"$1 $2\" = 'volume rm' ]; then printf '%s\\n' \"$*\" > \"$DEV_SWEEP_TEST_LOG\"; fi\n");
    fs::set_permissions(script, fs::Permissions::from_mode(0o755)).unwrap();
    let log = f.0.join("removed");
    let run = |apply: bool, active: bool, name: &str| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_dev-sweep"));
        cmd.args(["remove-volume", name, "--config"])
            .arg(f.0.join("config"))
            .env("HOME", f.0.join("home"))
            .env("PATH", f.0.join("bin"))
            .env("DEV_SWEEP_TEST_ACTIVE", if active { "1" } else { "0" })
            .env("DEV_SWEEP_TEST_LOG", &log);
        if apply {
            cmd.arg("--apply");
        }
        cmd.output().unwrap()
    };
    success(&run(false, false, "unused_volume"));
    assert!(!log.exists());
    assert!(!run(true, true, "used_volume").status.success());
    assert!(!log.exists());
    assert!(!run(true, false, "--force").status.success());
    assert!(!log.exists());
    success(&run(true, false, "unused_volume"));
    assert_eq!(
        fs::read_to_string(log).unwrap(),
        "volume rm unused_volume\n"
    );
}

#[test]
fn disk_audit_reports_retained_data_and_handles_newlines_in_paths() {
    let f = Fixture::new();
    for (name, body) in [
        ("du", "printf '2048\\t%s\\0' \"$DEV_SWEEP_TEST_DIR\""),
        ("df", "echo 'Filesystem size used available'"),
        ("nix-store", "printf '/nix/store/unused\\n'"),
        ("journalctl", "echo 'journals use 8M'"),
    ] {
        let path = f.file(&format!("bin/{name}"), &format!("#!/bin/sh\n{body}\n"));
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_dev-sweep"))
        .args(["audit", "--root"])
        .arg(&f.0)
        .args(["--config"])
        .arg(f.0.join("config"))
        .arg("--no-docker")
        .env("PATH", f.0.join("bin"))
        .env("HOME", f.0.join("home"))
        .env("DEV_SWEEP_TEST_DIR", f.0.join("source with\nnewline"))
        .output()
        .unwrap();
    success(&output);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("2.0 MiB"));
    assert!(text.contains("source with\\nnewline"));
    assert!(text.contains("1 unreferenced Nix paths"));
    assert!(text.contains("nested rows overlap"));
}

#[test]
fn worktree_removal_includes_generated_caches_but_preserves_unknown_data() {
    let f = Fixture::new();
    let repo = f.0.join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "--initial-branch=main"]);
    fs::write(repo.join("source"), "committed source").unwrap();
    fs::write(repo.join("package.json"), "{}").unwrap();
    fs::write(repo.join("Cargo.toml"), "[package]").unwrap();
    fs::write(
        repo.join(".gitignore"),
        "node_modules/\ntarget/\n.env\nunknown/\n",
    )
    .unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "fixture"]);
    for name in [
        "bundled",
        "secrets",
        "unknown",
        "protected",
        "fresh",
        "tracked",
    ] {
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                name,
                f.0.join(name).to_str().unwrap(),
            ],
        );
        f.file(
            &format!("{name}/node_modules/generated.js"),
            "generated dependency",
        );
    }
    f.file("bundled/target/CACHEDIR.TAG", "generated build marker");
    f.file("bundled/target/debug/build", "generated build output");
    let outside = f.file("outside/data", "must survive");
    symlink(&outside, f.0.join("bundled/node_modules/external-link")).unwrap();
    f.file("secrets/.env", "private data");
    f.file("unknown/unknown/file", "unrecognized ignored data");
    f.file("protected/node_modules/.dev-sweep-keep", "");
    f.file("tracked/node_modules/tracked-source", "tracked data");
    git(
        &f.0.join("tracked"),
        &["add", "-f", "node_modules/tracked-source"],
    );
    git(
        &f.0.join("tracked"),
        &["commit", "-m", "tracked artifact data"],
    );
    git(&repo, &["merge", "--ff-only", "tracked"]);
    age(&f.0);
    fs::write(
        f.0.join("fresh/node_modules/generated.js"),
        "fresh dependency",
    )
    .unwrap();
    for name in ["secrets", "unknown", "protected", "fresh", "tracked"] {
        let output = f.run(&[
            "remove-worktree",
            f.0.join(name).to_str().unwrap(),
            "--apply",
        ]);
        assert!(!output.status.success(), "must keep {name}");
        assert!(f.0.join(name).exists());
    }
    let bundled = f.0.join("bundled");
    success(&f.run(&["remove-worktree", bundled.to_str().unwrap()]));
    assert!(
        bundled.join("node_modules/generated.js").exists(),
        "preview must keep dependencies"
    );
    success(&f.run(&["remove-worktree", bundled.to_str().unwrap(), "--apply"]));
    assert!(!bundled.exists());
    assert_eq!(fs::read_to_string(outside).unwrap(), "must survive");
    assert!(repo.join("source").exists());
    git(&repo, &["show-ref", "--verify", "refs/heads/bundled"]);
}

#[test]
fn broad_scans_preserve_global_node_paths_and_clean_project_dependencies() {
    let f = Fixture::new();
    let home = f.0.join("home");
    let protected = [
        ".npm/project",
        ".bun/install/cache/project",
        ".local/share/pnpm/store/project",
        "custom-npm/project",
        "custom-pnpm/project",
        "custom-bun/project",
    ];
    for project in protected.iter().copied().chain(["projects/local"]) {
        f.file(&format!("home/{project}/package.json"), "{}");
        f.file(
            &format!("home/{project}/node_modules/dependency"),
            "generated",
        );
    }
    age(&home);
    let output = Command::new(env!("CARGO_BIN_EXE_dev-sweep"))
        .args([
            "clean",
            "--apply",
            "--no-docker",
            "--no-global-caches",
            "--root",
        ])
        .arg(&home)
        .arg("--config")
        .arg(f.0.join("config"))
        .env("HOME", &home)
        .env("npm_config_cache", home.join("custom-npm"))
        .env("PNPM_HOME", home.join("custom-pnpm"))
        .env("BUN_INSTALL", home.join("custom-bun"))
        .current_dir(std::env::temp_dir())
        .output()
        .unwrap();
    success(&output);
    for project in protected {
        assert!(home.join(project).join("node_modules/dependency").exists());
    }
    assert!(!home.join("projects/local/node_modules").exists());
    assert!(home.join("projects/local/package.json").exists());
}
