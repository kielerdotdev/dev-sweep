//! Opt-in agent cleanup. Snapshots and the durable journal precede removal.
use super::*;

type Result<T> = std::result::Result<T, ReapError>;
#[derive(Debug, thiserror::Error)]
pub enum ReapError {
    #[error("{0}")]
    Refused(String),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Time(#[from] chrono::ParseError),
    #[error(transparent)]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error(transparent)]
    Integer(#[from] std::num::ParseIntError),
    #[error(transparent)]
    IntegerRange(#[from] std::num::TryFromIntError),
    #[error(transparent)]
    Boolean(#[from] std::str::ParseBoolError),
    #[error("{0}")]
    Command(#[from] Box<dyn std::error::Error>),
}
impl From<&str> for ReapError {
    fn from(s: &str) -> Self {
        Self::Refused(s.to_owned())
    }
}
impl From<String> for ReapError {
    fn from(s: String) -> Self {
        Self::Refused(s)
    }
}
fn busy() -> Result<Vec<PathBuf>> {
    let mut paths = busy_paths(false)?;
    paths.push(env::current_dir()?);
    paths.push(env::current_exe()?);
    Ok(paths)
}

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::os::unix::fs::OpenOptionsExt;

#[derive(Clone)]
pub struct Settings {
    enabled: bool,
    requires_push: bool,
    roots: Vec<PathBuf>,
    custom_roots: bool,
    state: PathBuf,
    after: u64,
    orphan: u64,
    artifact: u64,
    volume: u64,
    file_mb: u64,
    total_mb: u64,
}
impl Settings {
    pub fn new(home: &Path) -> Self {
        Self {
            enabled: false,
            requires_push: true,
            custom_roots: false,
            roots: vec![home.join(".t3/worktrees"), home.join(".codex/worktrees")],
            state: home.join(".t3/userdata/statev2.sqlite"),
            after: 12,
            orphan: 3,
            artifact: 2,
            volume: 24,
            file_mb: 50,
            total_mb: 200,
        }
    }
    pub fn set(&mut self, key: &str, value: &str, home: &Path) -> Result<()> {
        match key {
            "reap" => self.enabled = value.parse()?,
            "reap_requires_push" => self.requires_push = value.parse()?,
            "agent_root" => {
                let path = expand(value, home)?;
                if !self.custom_roots {
                    self.roots.clear();
                    self.custom_roots = true;
                }
                self.roots.push(path);
            }
            "t3_state" => self.state = expand(value, home)?,
            _ => {
                let n: u64 = value.parse()?;
                if n == 0 || n > 1_000_000 {
                    return Err("reap limits must be between 1 and 1000000".into());
                }
                match key {
                    "reap_after_hours" => self.after = n,
                    "orphan_idle_days" => self.orphan = n,
                    "artifact_idle_days" => self.artifact = n,
                    "volume_after_hours" => self.volume = n,
                    "snapshot_max_file_mb" => self.file_mb = n,
                    "snapshot_max_total_mb" => self.total_mb = n,
                    _ => return Err("unknown reap setting".into()),
                }
            }
        }
        Ok(())
    }
    pub fn write(&self, file: &mut fs::File) -> Result<()> {
        writeln!(file, "reap={}\nreap_requires_push={}\nt3_state={}\nreap_after_hours={}\norphan_idle_days={}\nartifact_idle_days={}\nvolume_after_hours={}\nsnapshot_max_file_mb={}\nsnapshot_max_total_mb={}", self.enabled, self.requires_push, self.state.display(), self.after, self.orphan, self.artifact, self.volume, self.file_mb, self.total_mb)?;
        for root in &self.roots {
            writeln!(file, "agent_root={}", root.display())?;
        }
        Ok(())
    }
}

#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Settlement {
    Settled,
    Active,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Payload {
    worktree_path: Option<PathBuf>,
    settled_at: Option<String>,
    settled_override: Option<Settlement>,
}
struct Thread {
    id: String,
    finished: Option<DateTime<Utc>>,
}
fn timestamp(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}
struct ThreadRow {
    id: String,
    archived: Option<String>,
    deleted: Option<String>,
    updated: String,
    json: String,
}
fn threads(path: &Path) -> Result<BTreeMap<PathBuf, Vec<Thread>>> {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(Duration::from_millis(500))?;
    let mut query = db.prepare("SELECT thread_id, archived_at, deleted_at, updated_at, payload_json FROM orchestration_v2_projection_threads")?;
    let rows = query.query_map([], |r| {
        Ok(ThreadRow {
            id: r.get(0)?,
            archived: r.get(1)?,
            deleted: r.get(2)?,
            updated: r.get(3)?,
            json: r.get(4)?,
        })
    })?;
    let mut result: BTreeMap<PathBuf, Vec<Thread>> = BTreeMap::new();
    for row in rows {
        let ThreadRow {
            id,
            archived,
            deleted,
            updated,
            json,
        } = row?;
        let payload: Payload = serde_json::from_str(&json)?;
        if let Some(path) = payload.worktree_path {
            let path = match fs::canonicalize(&path) {
                Ok(p) => p,
                Err(e) if e.kind() == io::ErrorKind::NotFound => path,
                Err(e) => return Err(e.into()),
            };
            let settled = if payload.settled_override == Some(Settlement::Settled) {
                payload.settled_at
            } else {
                None
            };
            let finished = archived
                .iter()
                .chain(deleted.iter())
                .chain(settled.iter())
                .map(|s| timestamp(s))
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .max();
            // An edit after settlement also resets the grace period.
            let finished = finished
                .map(|t| timestamp(&updated).map(|u| t.max(u)))
                .transpose()?;
            result
                .entry(path)
                .or_default()
                .push(Thread { id, finished });
        }
    }
    Ok(result)
}
fn text(path: &Path, args: &[&str]) -> Result<String> {
    let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    Ok(String::from_utf8(git(path, &args)?.stdout)?
        .trim()
        .to_owned())
}
fn optional_ref(path: &Path, args: &[&str]) -> Result<Option<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()?;
    if output.status.success() {
        Ok(Some(String::from_utf8(output.stdout)?.trim().to_owned()))
    } else if output.status.code() == Some(1) {
        Ok(None)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned().into())
    }
}
fn old_git(path: &Path, cutoff: SystemTime) -> Result<()> {
    let dir = PathBuf::from(text(path, &["rev-parse", "--absolute-git-dir"])?);
    for name in ["index", "HEAD", "logs/HEAD"] {
        match fs::metadata(dir.join(name)) {
            Ok(m) if m.modified()? >= cutoff => return Err("recent Git activity".into()),
            Ok(_) => (),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
/// The worktree holds no work that exists only here: nothing uncommitted or untracked, and
/// HEAD is on a remote-tracking ref or every change it carries is already in a remote's
/// main branch (squash merges and cherry-picks count, by patch or by resulting content).
fn pushed(path: &Path) -> Result<()> {
    if !text(path, &["status", "--porcelain=v1", "--untracked-files=all"])?.is_empty() {
        return Err("uncommitted or untracked changes not pushed".into());
    }
    let head = optional_ref(path, &["rev-parse", "--verify", "--quiet", "HEAD"])?
        .ok_or("no commits to push")?;
    if !text(
        path,
        &[
            "for-each-ref",
            "--contains",
            &head,
            "--format=%(refname)",
            "refs/remotes/",
        ],
    )?
    .is_empty()
    {
        return Ok(());
    }
    let refs = text(
        path,
        &["for-each-ref", "--format=%(refname)", "refs/remotes/"],
    )?;
    for upstream in refs
        .lines()
        .filter(|r| r.ends_with("/main") || r.ends_with("/master"))
    {
        let cherry = text(path, &["cherry", upstream, &head])?;
        if !cherry.lines().any(|l| l.starts_with('+')) {
            return Ok(());
        }
        let base = text(path, &["merge-base", upstream, &head])?;
        let changed = text(path, &["diff", "--name-only", &base, &head])?;
        if changed.is_empty() {
            continue;
        }
        let mut args = vec!["diff", "--quiet", upstream, head.as_str(), "--"];
        args.extend(changed.lines());
        // `git diff --quiet` exits 0 for no difference and 1 for a difference.
        if optional_ref(path, &args)?.is_some() {
            return Ok(());
        }
    }
    Err("HEAD not pushed".into())
}
struct Files {
    ignored: Vec<PathBuf>,
}
fn snapshot_files(path: &Path, config: &Config) -> Result<Files> {
    if path.join(".gitmodules").exists()
        || !text(path, &["ls-files", "--stage"])?
            .lines()
            .all(|l| !l.starts_with("160000 "))
    {
        return Err("contains submodules".into());
    }
    let flags = git(
        path,
        &[OsStr::new("ls-files"), OsStr::new("-v"), OsStr::new("-z")],
    )?;
    if flags.stdout.split(|b| *b == 0).any(|f| {
        f.first()
            .is_some_and(|b| b.is_ascii_lowercase() || *b == b'S')
    }) {
        return Err("sparse or assume-unchanged index entries cannot be snapshotted safely".into());
    }
    let mut pending = vec![path.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let p = entry.path();
            if entry.file_name() == ".git" {
                if dir != path {
                    return Err("contains nested Git repository".into());
                }
                continue;
            }
            if config.protects(&p) {
                return Err("contains protected path".into());
            }
            if entry.file_type()?.is_dir() {
                pending.push(p);
            }
        }
    }
    let mut ignored = Vec::new();
    let mut total = 0_u64;
    for (args, force) in [
        (
            vec!["ls-files", "--others", "--exclude-standard", "-z"],
            false,
        ),
        (
            vec![
                "ls-files",
                "--others",
                "--ignored",
                "--exclude-standard",
                "-z",
            ],
            true,
        ),
    ] {
        let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        for name in git(path, &args)?
            .stdout
            .split(|b| *b == 0)
            .filter(|b| !b.is_empty())
        {
            let relative = PathBuf::from(OsString::from_vec(name.to_vec()));
            let p = path.join(&relative);
            if p.ancestors()
                .take_while(|p| *p != path)
                .any(|p| artifact_kind(p).is_some())
            {
                continue;
            }
            let m = fs::symlink_metadata(&p)?;
            if m.is_file() {
                total = total.saturating_add(m.len());
                if m.len() > config.reap.file_mb * 1024 * 1024
                    || total > config.reap.total_mb * 1024 * 1024
                {
                    return Err("snapshot non-artifact file size limit".into());
                }
            }
            if force {
                ignored.push(relative);
            }
        }
    }
    Ok(Files { ignored })
}
fn eligible(
    tree: &Worktree,
    trees: &[Worktree],
    config: &Config,
    busy: &[PathBuf],
    states: &BTreeMap<PathBuf, Vec<Thread>>,
) -> Result<(Stats, Vec<String>, Files)> {
    let tree = trees
        .iter()
        .find(|current| current.path == tree.path)
        .ok_or("worktree registration disappeared")?;
    let path = &tree.path;
    if tree.bare || trees.first().is_some_and(|t| t.path == *path) {
        return Err("main/bare worktree".into());
    }
    if tree.locked {
        return Err("locked".into());
    }
    if !config.contains(path) || config.excludes.iter().any(|p| p.starts_with(path)) {
        return Err("protected or outside roots".into());
    }
    if fs::canonicalize(path)? != *path {
        return Err("symlink path".into());
    }
    if busy.iter().any(|p| p.starts_with(path)) {
        return Err("in use by a process".into());
    }
    let ids = if let Some(threads) = states.get(path) {
        let cutoff = Utc::now() - chrono::Duration::hours(i64::try_from(config.reap.after)?);
        if threads
            .iter()
            .any(|t| t.finished.is_none_or(|time| time >= cutoff))
        {
            return Err("active thread or settlement grace period".into());
        }
        threads.iter().map(|t| t.id.clone()).collect()
    } else {
        let mut idle = config.clone();
        idle.days = config.reap.orphan;
        if !is_stale(&stats(path, &idle, true)?, idle.cutoff()) {
            return Err("orphan has recent filesystem activity".into());
        }
        old_git(path, idle.cutoff())?;
        Vec::new()
    };
    if config.reap.requires_push {
        pushed(path)?;
    }
    let info = stats_inner(path, config, true, false)?;
    Ok((info, ids, snapshot_files(path, config)?))
}

#[derive(Serialize, Deserialize)]
struct Record {
    path: PathBuf,
    common_dir: PathBuf,
    branch: Option<String>,
    head: Option<String>,
    snapshot_ref: String,
    index_tree: String,
    thread_ids: Vec<String>,
    freed_bytes: u64,
    time: DateTime<Utc>,
}
fn journal(home: &Path) -> PathBuf {
    home.join(".local/state/dev-sweep/reaped.jsonl")
}
fn index_git(path: &Path, index: &Path, args: &[&OsStr]) -> Result<Output> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_INDEX_FILE", index)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_AUTHOR_NAME", "dev-sweep")
        .env("GIT_AUTHOR_EMAIL", "dev-sweep@localhost")
        .env("GIT_COMMITTER_NAME", "dev-sweep")
        .env("GIT_COMMITTER_EMAIL", "dev-sweep@localhost")
        .output()?;
    if !output.status.success() {
        return Err(format!("snapshot git: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    Ok(output)
}
fn snapshot(
    path: &Path,
    home: &Path,
    ids: Vec<String>,
    info: &Stats,
    files: Files,
) -> Result<Record> {
    let gitdir = PathBuf::from(text(path, &["rev-parse", "--absolute-git-dir"])?);
    let common_dir = fs::canonicalize(path.join(text(path, &["rev-parse", "--git-common-dir"])?))?;
    let temp = tempfile::Builder::new()
        .prefix("dev-sweep-index-")
        .tempdir_in(&gitdir)?;
    let index = temp.path().join("index");
    match fs::copy(gitdir.join("index"), &index) {
        Ok(_) => (),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            index_git(
                path,
                &index,
                &[OsStr::new("read-tree"), OsStr::new("--empty")],
            )?;
        }
        Err(e) => return Err(e.into()),
    }
    let index_tree =
        String::from_utf8(index_git(path, &index, &[OsStr::new("write-tree")])?.stdout)?
            .trim()
            .to_owned();
    index_git(
        path,
        &index,
        &[
            OsStr::new("add"),
            OsStr::new("-A"),
            OsStr::new("--"),
            OsStr::new("."),
        ],
    )?;
    for file in files.ignored {
        index_git(
            path,
            &index,
            &[
                OsStr::new("--literal-pathspecs"),
                OsStr::new("add"),
                OsStr::new("-f"),
                OsStr::new("--"),
                file.as_os_str(),
            ],
        )?;
    }
    let tree = String::from_utf8(index_git(path, &index, &[OsStr::new("write-tree")])?.stdout)?
        .trim()
        .to_owned();
    let head = optional_ref(path, &["rev-parse", "--verify", "--quiet", "HEAD"])?;
    let branch = optional_ref(path, &["symbolic-ref", "-q", "--short", "HEAD"])?;
    let message = format!(
        "dev-sweep WIP\npath: {path:?}\nbranch: {branch:?}\nHEAD: {head:?}\nthreads: {ids:?}"
    );
    let mut args = vec![
        OsStr::new("commit-tree"),
        OsStr::new(&tree),
        OsStr::new("-m"),
        OsStr::new(&message),
    ];
    if let Some(head) = &head {
        args.extend([OsStr::new("-p"), OsStr::new(head)]);
    }
    let commit = String::from_utf8(index_git(path, &index, &args)?.stdout)?
        .trim()
        .to_owned();
    let project = path
        .parent()
        .and_then(Path::file_name)
        .ok_or("missing project")?
        .to_string_lossy();
    let name = path
        .file_name()
        .ok_or("missing worktree name")?
        .to_string_lossy();
    let base = format!("refs/dev-sweep/wip/{project}/{name}");
    let mut reference = base.clone();
    if optional_ref(path, &["show-ref", "--verify", "--quiet", &base])?.is_some() {
        reference = format!(
            "{base}-{}",
            Utc::now()
                .timestamp_nanos_opt()
                .ok_or("timestamp overflow")?
        );
    }
    text(
        path,
        &[
            "update-ref",
            &reference,
            &commit,
            "0000000000000000000000000000000000000000",
        ],
    )?;
    // Keep the original index tree reachable too, including staged-only content.
    text(
        path,
        &[
            "update-ref",
            &reference.replacen("/wip/", "/index/", 1),
            &index_tree,
            "0000000000000000000000000000000000000000",
        ],
    )?;
    let record = Record {
        path: path.to_owned(),
        common_dir,
        branch,
        head,
        snapshot_ref: reference,
        index_tree,
        thread_ids: ids,
        freed_bytes: info.bytes,
        time: Utc::now(),
    };
    let log = journal(home);
    fs::create_dir_all(log.parent().ok_or("invalid journal")?)?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(log)?;
    let mut line = serde_json::to_vec(&record)?;
    line.push(b'\n');
    file.write_all(&line)?;
    file.sync_all()?;
    Ok(record)
}

// Compare a second temporary index, never refresh the worktree's real index.
fn verify_snapshot(record: &Record, config: &Config) -> Result<()> {
    let path = &record.path;
    let gitdir = PathBuf::from(text(path, &["rev-parse", "--absolute-git-dir"])?);
    let temp = tempfile::Builder::new()
        .prefix("dev-sweep-verify-")
        .tempdir_in(&gitdir)?;
    let index = temp.path().join("index");
    match fs::copy(gitdir.join("index"), &index) {
        Ok(_) => (),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            index_git(
                path,
                &index,
                &[OsStr::new("read-tree"), OsStr::new("--empty")],
            )?;
        }
        Err(e) => return Err(e.into()),
    }
    let current = String::from_utf8(index_git(path, &index, &[OsStr::new("write-tree")])?.stdout)?;
    if current.trim() != record.index_tree
        || optional_ref(path, &["rev-parse", "--verify", "--quiet", "HEAD"])? != record.head
        || optional_ref(path, &["symbolic-ref", "-q", "--short", "HEAD"])? != record.branch
    {
        return Err("HEAD, branch or index changed during snapshot; worktree retained".into());
    }
    index_git(
        path,
        &index,
        &[OsStr::new("read-tree"), OsStr::new(&record.snapshot_ref)],
    )?;
    let files = snapshot_files(path, config)?;
    index_git(
        path,
        &index,
        &[
            OsStr::new("add"),
            OsStr::new("-A"),
            OsStr::new("--"),
            OsStr::new("."),
        ],
    )?;
    for file in files.ignored {
        index_git(
            path,
            &index,
            &[
                OsStr::new("--literal-pathspecs"),
                OsStr::new("add"),
                OsStr::new("-f"),
                OsStr::new("--"),
                file.as_os_str(),
            ],
        )?;
    }
    let current = String::from_utf8(index_git(path, &index, &[OsStr::new("write-tree")])?.stdout)?;
    let expected = text(
        path,
        &["rev-parse", &format!("{}^{{tree}}", record.snapshot_ref)],
    )?;
    if current.trim() != expected {
        return Err("files changed during snapshot; worktree retained".into());
    }
    Ok(())
}

pub fn restore(home: &Path, path: &Path, apply: bool) -> Result<()> {
    let records = fs::read_to_string(journal(home))?;
    let record = records
        .lines()
        .map(serde_json::from_str::<Record>)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .rev()
        .find(|r| r.path == path)
        .ok_or("no snapshot for path")?;
    if path.exists() {
        return Err("restore path already exists".into());
    }
    println!(
        "{} restore {:?} from {}",
        if apply { "Will" } else { "Would" },
        path,
        record.snapshot_ref
    );
    if !apply {
        return Ok(());
    }
    let repo = &record.common_dir;
    let repo_git = |args: &[&OsStr]| git(repo, args);
    let branch_available = if let Some(branch) = &record.branch {
        let output = repo_git(&[
            OsStr::new("worktree"),
            OsStr::new("list"),
            OsStr::new("--porcelain"),
        ])?;
        let occupied = String::from_utf8(output.stdout)?
            .lines()
            .any(|l| l == format!("branch refs/heads/{branch}"));
        !occupied
            && optional_ref(
                repo,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{branch}"),
                ],
            )? == record.head
    } else {
        false
    };
    let head = record.head.as_deref();
    if let Some(head) = head {
        if branch_available {
            repo_git(&[
                OsStr::new("worktree"),
                OsStr::new("add"),
                OsStr::new("--"),
                path.as_os_str(),
                OsStr::new(record.branch.as_deref().ok_or("missing branch")?),
            ])?;
        } else {
            repo_git(&[
                OsStr::new("worktree"),
                OsStr::new("add"),
                OsStr::new("--detach"),
                OsStr::new("--"),
                path.as_os_str(),
                OsStr::new(head),
            ])?;
        }
    } else {
        let branch = record
            .branch
            .as_deref()
            .ok_or("unborn snapshot has no branch")?;
        if optional_ref(
            repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        )?
        .is_some()
        {
            return Err("unborn branch now exists".into());
        }
        repo_git(&[
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--orphan"),
            OsStr::new("-b"),
            OsStr::new(branch),
            OsStr::new("--"),
            path.as_os_str(),
        ])?;
    }
    // Populate all snapshot files, then restore the original staging boundary.
    text(path, &["read-tree", "--reset", "-u", &record.snapshot_ref])?;
    text(path, &["read-tree", &record.index_tree])?;
    Ok(())
}

/// Re-checks one worktree against live state, snapshots it, verifies the snapshot and removes it.
/// Any failure leaves the worktree in place; the caller reports it and moves on.
fn reap_one(
    tree: &Worktree,
    scope: &Config,
    config: &Config,
    home: &Path,
    docker: Option<&mut Docker>,
) -> Result<()> {
    let current = if config.reap.state.exists() {
        threads(&config.reap.state)?
    } else {
        BTreeMap::new()
    };
    let (info, ids, files) = eligible(tree, &worktrees(&tree.path)?, scope, &busy()?, &current)?;
    let record = snapshot(&tree.path, home, ids, &info, files)?;
    verify_snapshot(&record, scope)?;
    if busy()?.iter().any(|p| p.starts_with(&tree.path)) {
        return Err("worktree became busy after snapshot; retained".into());
    }
    if let Some(d) = docker {
        d.remove_worktree(&tree.path)?;
    }
    git(
        &record.common_dir,
        &[
            OsStr::new("worktree"),
            OsStr::new("remove"),
            OsStr::new("--force"),
            OsStr::new("--"),
            tree.path.as_os_str(),
        ],
    )?;
    text(&record.common_dir, &["worktree", "prune"])?;
    Ok(())
}

pub fn run(config: &Config, home: &Path, apply: bool) -> Result<()> {
    if apply && !config.reap.enabled {
        return Err("reap --apply requires reap=true in config".into());
    }
    let mut scope = config.clone();
    scope.roots = config
        .reap
        .roots
        .iter()
        .filter(|p| p.exists())
        .map(fs::canonicalize)
        .collect::<io::Result<_>>()?;
    // A missing database means T3 is not used here, so the orphan rule applies. A database
    // that exists but cannot be read is unknown state: reaping worktrees then is unsafe.
    let states = if config.reap.state.exists() {
        threads(&config.reap.state)
            .map_err(|e| eprintln!("T3 state unreadable, worktrees retained: {e}"))
            .ok()
    } else {
        Some(BTreeMap::new())
    };
    let mut docker = if config.docker {
        Docker::load().map(Some).unwrap_or_else(|e| {
            eprintln!("Docker skipped: {e}");
            None
        })
    } else {
        None
    };
    // Inventory protected worktrees too, so every retained checkout gets a reason.
    let mut pending = scope.roots.clone();
    let mut repos = Vec::new();
    while let Some(path) = pending.pop() {
        if path.join(".git").exists() {
            repos.push(path);
            continue;
        }
        let entries = match fs::read_dir(&path) {
            Ok(e) => e,
            Err(e) => {
                println!("SKIP {path:?}: {e}");
                continue;
            }
        };
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    let initial_busy = busy()?;
    let mut seen = HashSet::new();
    let mut reaped_paths = Vec::new();
    let mut count = 0;
    let mut skipped = 0;
    let mut bytes: u64 = 0;
    for repo in repos {
        let trees = match worktrees(&repo) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("SKIP {repo:?}: {e}");
                continue;
            }
        };
        for tree in &trees {
            if !scope.roots.iter().any(|p| tree.path.starts_with(p))
                || !seen.insert(tree.path.clone())
            {
                continue;
            }
            let check = states
                .as_ref()
                .ok_or_else(|| ReapError::from("T3 state unreadable"))
                .and_then(|states| eligible(tree, &trees, &scope, &initial_busy, states))
                .and_then(|v| {
                    if let Some(d) = &docker {
                        d.check_worktree(&tree.path)?;
                    } else if config.docker {
                        return Err("Docker state unknown".into());
                    }
                    Ok(v)
                });
            match check {
                Ok((info, ids, _files)) => {
                    println!(
                        "{} {} {:?} ({} threads)",
                        if apply { "REAP" } else { "WOULD REAP" },
                        human_size(info.bytes),
                        tree.path,
                        ids.len()
                    );
                    if !apply {
                        if let Some(d) = &docker {
                            d.preview_worktree(&tree.path);
                        }
                    }
                    if apply {
                        if let Err(e) = reap_one(tree, &scope, config, home, docker.as_mut()) {
                            skipped += 1;
                            println!("SKIP {:?}: {e}", tree.path);
                            continue;
                        }
                    }
                    reaped_paths.push(tree.path.clone());
                    count += 1;
                    bytes = bytes.saturating_add(info.bytes);
                }
                Err(e) => {
                    skipped += 1;
                    println!("SKIP {:?}: {e}", tree.path);
                }
            }
        }
    }
    println!(
        "Reap: {count} worktrees, {} estimated, {skipped} skipped",
        human_size(bytes)
    );
    // Main checkouts keep the ordinary age; agent linked checkouts use the short age.
    let mut artifact_scope = config.clone();
    artifact_scope.roots.extend(scope.roots.clone());
    let (artifacts, _, _) = find_artifacts(&artifact_scope)?;
    let mut caches = 0;
    let mut cache_bytes: u64 = 0;
    for path in artifacts {
        if reaped_paths.iter().any(|p| path.starts_with(p)) {
            continue;
        }
        let mut idle = artifact_scope.clone();
        if let Some(root) = path.ancestors().find(|p| p.join(".git").is_file()) {
            if scope.roots.iter().any(|p| root.starts_with(p))
                && worktrees(root)?.first().is_some_and(|t| t.path != root)
            {
                idle.days = config.reap.artifact;
            }
        }
        match artifact_stats(&path, &idle, &busy()?) {
            Ok(info) if is_stale(&info, idle.cutoff()) => {
                println!(
                    "{} artifact {} {path:?}",
                    if apply { "REMOVE" } else { "WOULD REMOVE" },
                    human_size(info.bytes)
                );
                if apply {
                    let current = artifact_stats(&path, &idle, &busy()?)?;
                    if !is_stale(&current, idle.cutoff()) {
                        println!("SKIP {path:?}: artifact became active");
                        continue;
                    }
                    fs::remove_dir_all(&path)?;
                }
                caches += 1;
                cache_bytes = cache_bytes.saturating_add(info.bytes);
            }
            Ok(_) => (),
            Err(e) => println!("SKIP artifact {path:?}: {e}"),
        }
    }
    println!(
        "Artifacts: {caches} directories, {} estimated",
        human_size(cache_bytes)
    );
    if let Some(docker) = &mut docker {
        docker.cleanup(config.reap.volume, apply)?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Volume {
    name: String,
    created_at: String,
    labels: Option<BTreeMap<String, String>>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Container {
    id: String,
    config: ContainerConfig,
    state: ContainerState,
    mounts: Vec<Mount>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ContainerConfig {
    labels: Option<BTreeMap<String, String>>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ContainerState {
    running: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Mount {
    name: Option<String>,
}
struct Docker {
    containers: Vec<Container>,
    volumes: Vec<Volume>,
}
fn docker(args: &[&str]) -> Result<Vec<u8>> {
    let args = args.iter().map(OsString::from).collect::<Vec<_>>();
    let output = stores::capture("docker", &args, 10)?;
    if !output.status.success() {
        return Err(format!("docker: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    Ok(output.stdout)
}
fn inspect<T: serde::de::DeserializeOwned>(kind: &str, ids: &[String]) -> Result<Vec<T>> {
    let mut result = Vec::new();
    for chunk in ids.chunks(100) {
        let mut args = vec![kind, "inspect"];
        args.extend(chunk.iter().map(String::as_str));
        result.extend(serde_json::from_slice::<Vec<T>>(&docker(&args)?)?);
    }
    Ok(result)
}
fn label<'a>(labels: &'a Option<BTreeMap<String, String>>, key: &str) -> Option<&'a str> {
    labels.as_ref()?.get(key).map(String::as_str)
}
fn missing_owner(labels: &Option<BTreeMap<String, String>>) -> bool {
    label(labels, "dev-sweep.owner").is_some_and(|p| {
        Path::new(p).is_absolute()
            && matches!(fs::symlink_metadata(p), Err(e) if e.kind() == io::ErrorKind::NotFound)
    })
}
fn anonymous_old(v: &Volume, hours: u64) -> bool {
    v.name.len() == 64
        && v.name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && v.labels
            .as_ref()
            .is_none_or(|labels| labels.keys().all(|k| k == "com.docker.volume.anonymous"))
        && timestamp(&v.created_at).is_ok_and(|t| {
            Utc::now().signed_duration_since(t).num_seconds()
                > i64::try_from(hours.saturating_mul(3600)).unwrap_or(i64::MAX)
        })
}
/// Compose's default project name: the directory name, lowercased, keeping `[a-z0-9_-]`.
fn default_compose_project(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect()
}
impl Docker {
    fn load() -> Result<Self> {
        let ids = |args: &[&str]| -> Result<Vec<String>> {
            Ok(String::from_utf8(docker(args)?)?
                .lines()
                .map(str::to_owned)
                .collect())
        };
        let containers = inspect("container", &ids(&["ps", "-aq"])?)?;
        let volumes = inspect("volume", &ids(&["volume", "ls", "-q"])?)?;
        Ok(Self {
            containers,
            volumes,
        })
    }
    fn compose_inside(c: &Container, path: &Path) -> bool {
        label(&c.config.labels, "com.docker.compose.project.working_dir").is_some_and(|p| {
            Path::new(p).is_absolute()
                && (Path::new(p).starts_with(path)
                    || fs::canonicalize(p).is_ok_and(|p| p.starts_with(path)))
        })
    }
    fn check_worktree(&self, path: &Path) -> Result<()> {
        if self.containers.iter().any(|c| {
            c.state.running
                && (Self::compose_inside(c, path)
                    || label(&c.config.labels, "dev-sweep.owner")
                        .is_some_and(|p| Path::new(p).starts_with(path)))
        }) {
            return Err("running Docker container".into());
        }
        Ok(())
    }
    fn referenced(&self, name: &str) -> bool {
        self.containers
            .iter()
            .any(|c| c.mounts.iter().any(|m| m.name.as_deref() == Some(name)))
    }
    /// Named volumes owned by this worktree's compose project and mounted by its containers.
    /// Compose names a project after its directory unless the file sets `name:`; a fixed name
    /// may be shared with the main checkout, so only the directory-derived project qualifies.
    fn owned_volumes(&self, path: &Path) -> HashSet<String> {
        let project = default_compose_project(path);
        self.containers
            .iter()
            .filter(|c| Self::compose_inside(c, path))
            .flat_map(|c| c.mounts.iter().filter_map(|m| m.name.clone()))
            .filter(|name| {
                self.volumes.iter().any(|v| {
                    v.name == *name
                        && label(&v.labels, "com.docker.compose.project") == Some(project.as_str())
                })
            })
            .collect()
    }
    fn preview_worktree(&self, path: &Path) {
        for c in &self.containers {
            if Self::compose_inside(c, path) && !c.state.running {
                println!(
                    "WOULD REMOVE compose container {} with anonymous volumes",
                    c.id
                );
            }
        }
        for name in self.owned_volumes(path) {
            println!("WOULD REMOVE compose volume {name} if unused after container removal");
        }
    }
    fn remove_worktree(&mut self, path: &Path) -> Result<()> {
        *self = Self::load()?;
        self.check_worktree(path)?;
        let owned = self.owned_volumes(path);
        for c in &self.containers {
            if Self::compose_inside(c, path) {
                docker(&["container", "rm", "-v", "--", &c.id])?;
            }
        }
        *self = Self::load()?;
        for name in &owned {
            if !self.referenced(name) {
                stores::remove_volume(name, true)?;
            }
        }
        *self = Self::load()?;
        Ok(())
    }
    fn cleanup(&mut self, hours: u64, apply: bool) -> Result<()> {
        let mut containers = 0;
        let mut volumes = 0;
        for c in &self.containers {
            if missing_owner(&c.config.labels) {
                if c.state.running {
                    println!("SKIP container {}: running", c.id);
                    continue;
                }
                println!(
                    "{} orphan container {}",
                    if apply { "REMOVE" } else { "WOULD REMOVE" },
                    c.id
                );
                if apply {
                    docker(&["container", "rm", "-v", "--", &c.id])?;
                }
                containers += 1;
            }
        }
        if apply {
            *self = Self::load()?;
        }
        for v in &self.volumes {
            if !self.referenced(&v.name) && (anonymous_old(v, hours) || missing_owner(&v.labels)) {
                println!(
                    "{} volume {}",
                    if apply { "REMOVE" } else { "WOULD REMOVE" },
                    v.name
                );
                if apply {
                    stores::remove_volume(&v.name, true)?;
                }
                volumes += 1;
            }
        }
        println!("Docker: {containers} stopped owner containers, {volumes} dangling volumes (sizes not queried)");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn docker_fixtures_select_only_old_unlabelled_anonymous_or_missing_owners() {
        let hex = "a".repeat(64);
        let fixture = format!(
            r#"[{{"Name":"{hex}","CreatedAt":"2000-01-01T00:00:00Z","Labels":null}},{{"Name":"database","CreatedAt":"2000-01-01T00:00:00Z","Labels":{{}}}},{{"Name":"{hex}","CreatedAt":"2000-01-01T00:00:00Z","Labels":{{"keep":"yes"}}}}]"#
        );
        let volumes: Vec<Volume> = serde_json::from_str(&fixture).unwrap();
        assert!(anonymous_old(&volumes[0], 24));
        assert!(!anonymous_old(&volumes[1], 24));
        assert!(!anonymous_old(&volumes[2], 24));
        let marked: Volume = serde_json::from_str(&format!(
            r#"{{"Name":"{hex}","CreatedAt":"2000-01-01T00:00:00Z","Labels":{{"com.docker.volume.anonymous":""}}}}"#
        ))
        .unwrap();
        assert!(anonymous_old(&marked, 24));
        let containers: Vec<Container> = serde_json::from_str(r#"[{"Id":"running","Config":{"Labels":{"com.docker.compose.project.working_dir":"/agent/lane/app"}},"State":{"Running":true},"Mounts":[{"Name":"database"}]}]"#).unwrap();
        let d = Docker {
            containers,
            volumes,
        };
        assert!(d.check_worktree(Path::new("/agent/lane")).is_err());
        assert!(d.check_worktree(Path::new("/agent/lane2")).is_ok());
        assert!(d.referenced("database"));
        assert!(!missing_owner(&Some(BTreeMap::from([(
            "dev-sweep.owner".into(),
            "relative".into()
        )]))));
        let tmp = tempfile::tempdir().unwrap();
        assert!(missing_owner(&Some(BTreeMap::from([(
            "dev-sweep.owner".into(),
            tmp.path().join("missing").to_str().unwrap().into()
        )]))));
    }
    #[test]
    fn worktree_reap_only_claims_its_own_compose_project_volumes() {
        let containers: Vec<Container> = serde_json::from_str(r#"[
            {"Id":"own","Config":{"Labels":{"com.docker.compose.project.working_dir":"/agent/T3-ab_c","com.docker.compose.project":"t3-ab_c"}},"State":{"Running":false},"Mounts":[{"Name":"t3-ab_c_db"},{"Name":"shared_db"}]}
        ]"#).unwrap();
        let volumes: Vec<Volume> = serde_json::from_str(r#"[
            {"Name":"t3-ab_c_db","CreatedAt":"2000-01-01T00:00:00Z","Labels":{"com.docker.compose.project":"t3-ab_c"}},
            {"Name":"shared_db","CreatedAt":"2000-01-01T00:00:00Z","Labels":{"com.docker.compose.project":"boomerplay"}},
            {"Name":"other_db","CreatedAt":"2000-01-01T00:00:00Z","Labels":{"com.docker.compose.project":"t3-ab_c"}}
        ]"#).unwrap();
        let d = Docker {
            containers,
            volumes,
        };
        assert_eq!(
            d.owned_volumes(Path::new("/agent/T3-ab_c")),
            HashSet::from(["t3-ab_c_db".to_owned()])
        );
    }
    #[test]
    fn vanished_stats_entry_is_not_an_io_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Config::defaults(tmp.path());
        let s = stats_inner(&tmp.path().join("vanished"), &config, false, false);
        // A missing root is still an error; only entries vanishing during traversal are ignored.
        assert!(s.is_err());
    }
}
