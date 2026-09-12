//! Own the pilot's isolated source tree, processes, and recorded run.

mod environment;
mod process;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::ffi::{CString, OsStr};
use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use anyhow::Context as _;
use clap::Args;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::process::Command;

use super::{GradeArgs, GradeFailure, failure, home, read_json, text, write, write_json};

const PORTS: [u16; 6] = [54332, 5004, 4224, 3201, 4319, 8088];
const GATE_DOCUMENT: &str = "docs/operations/build-and-test.md";
const PILOT_SECTION: &str = "[AGENT-PILOT]";
const REPLAY_RESULTS: &str = "evidence/perf/2026.09/consolidation-step3/pilot-recorded-replay-001";
const HARNESS_DOCUMENTS: [&str; 3] = [
    "docs/poc/agent-authoring-tooling-spec.md",
    "docs/poc/deterministic-testing-spec.md",
    "docs/operations/agent-pilot.md",
];

/// Select an existing pilot action and its recorded run.
#[derive(Debug, Args)]
pub struct RunArgs {
    #[arg(value_parser = ["up", "launch", "grade", "down", "all"])]
    verb: String,
    #[arg(long)]
    run: String,
    #[arg(long)]
    agent: Option<String>,
    #[arg(long)]
    task: Option<PathBuf>,
    #[arg(long, default_value = "dev-env", value_parser = ["dev-env", "dev-up"])]
    standup: String,
    #[arg(long)]
    commit: Option<String>,
    #[arg(long, default_value = "completed")]
    stub_mode: String,
    #[arg(long)]
    discard_worktree: bool,
    #[arg(long, hide = true)]
    repository: PathBuf,
}

#[derive(Debug)]
struct Run {
    args: RunArgs,
    tree: PathBuf,
    pilot: PathBuf,
    grading: PathBuf,
    directory: PathBuf,
    key: String,
    target: PathBuf,
    task: Value,
}

fn directory(path: &Path) -> anyhow::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    Ok(())
}

fn executable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn matches_pattern(path: &str, pattern: &str) -> bool {
    let (Ok(path), Ok(pattern)) = (CString::new(path), CString::new(pattern)) else {
        return false;
    };
    // Both inputs remain live C strings; flags preserve the shell's pathname-independent match.
    unsafe { libc::fnmatch(pattern.as_ptr(), path.as_ptr(), 0) == 0 }
}

async fn output(command: &mut Command) -> anyhow::Result<Vec<u8>> {
    let output = command.kill_on_drop(true).output().await?;
    anyhow::ensure!(
        output.status.success(),
        "command exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

async fn string(command: &mut Command) -> anyhow::Result<String> {
    Ok(String::from_utf8(output(command).await?)?
        .trim_end_matches('\n')
        .to_owned())
}

fn git(path: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(path);
    command
}

fn copy_tree(source: &Path, destination: &Path) -> anyhow::Result<()> {
    directory(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            symlink(fs::read_link(entry.path())?, target)?;
        } else if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    fs::set_permissions(destination, source.metadata()?.permissions())?;
    Ok(())
}

fn validate_steps(steps: &Value) -> Result<(), GradeFailure> {
    let steps = steps
        .as_array()
        .filter(|steps| !steps.is_empty())
        .ok_or_else(|| failure(2, "steps.json must be a non-empty array"))?;
    if !steps.iter().any(|step| step["must"] == true) {
        return Err(failure(
            2,
            "steps.json has no must step, so nothing decides PASS or FAIL",
        ));
    }
    let missing = steps
        .iter()
        .filter(|step| step["must"] == true && text(&step["invariant"]).is_empty())
        .map(|step| text(&step["id"]))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(failure(
            2,
            format!("must steps name no invariant: {}", missing.join(", ")),
        ));
    }
    let missing = steps
        .iter()
        .filter(|step| {
            step["must"] == true && step.get("sql").is_none() && step.get("concurrent").is_none()
        })
        .filter(|step| {
            ![
                "present",
                "value",
                "equals",
                "error_code",
                "count",
                "sorted_by",
            ]
            .iter()
            .any(|field| step["expect"].get(field).is_some())
        })
        .map(|step| text(&step["id"]))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(failure(
            2,
            format!(
                "must steps assert nothing about the answer content: {}",
                missing.join(", ")
            ),
        ));
    }
    let mut seen = BTreeSet::new();
    let duplicates = steps
        .iter()
        .filter_map(|step| {
            let id = text(&step["id"]);
            (!seen.insert(id)).then_some(id)
        })
        .collect::<BTreeSet<_>>();
    if !duplicates.is_empty() {
        return Err(failure(
            2,
            format!(
                "duplicate step ids: {}",
                duplicates.into_iter().collect::<Vec<_>>().join(", ")
            ),
        ));
    }
    Ok(())
}

fn strip_document_section(path: &Path, marker: &str) -> anyhow::Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }
    let mut cut = false;
    let mut skipping = false;
    let mut output = String::new();
    for line in fs::read_to_string(path)?.lines() {
        let heading = line.starts_with('#') && line.trim_start_matches('#').starts_with(' ');
        if heading && line.contains(marker) {
            skipping = true;
            cut = true;
            continue;
        }
        if skipping && heading {
            skipping = false;
        }
        if !skipping {
            output.push_str(line);
            output.push('\n');
        }
    }
    if cut {
        fs::write(path, output)?;
    }
    Ok(cut)
}

fn rubric_paths(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut paths = vec![root.to_owned()];
    if !root.is_dir() || root.symlink_metadata()?.file_type().is_symlink() {
        return Ok(paths);
    }
    for entry in fs::read_dir(root)? {
        paths.extend(rubric_paths(&entry?.path())?);
    }
    Ok(paths)
}

async fn tracked_hash(root: &Path) -> anyhow::Result<String> {
    let paths = output(git(root).args(["ls-files", "-z"])).await?;
    let mut hash = Sha256::new();
    for path in paths
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let file = root.join(OsStr::from_bytes(path));
        let metadata = fs::symlink_metadata(&file)?;
        hash.update(path);
        hash.update([0]);
        hash.update(metadata.permissions().mode().to_le_bytes());
        if metadata.file_type().is_symlink() {
            hash.update(fs::read_link(file)?.as_os_str().as_bytes());
        } else {
            hash.update(fs::read(file)?);
        }
        hash.update([0]);
    }
    Ok(hex::encode(hash.finalize()))
}

fn assert_rubric_unreachable(
    run: &Path,
    target: &Path,
    grading: &Path,
    user_home: &Path,
    steps_name: &str,
) -> anyhow::Result<()> {
    let mut found = Vec::new();
    for handed in [
        run.to_owned(),
        run.join("fixture"),
        run.join("bin"),
        run.join("env"),
        run.join("worktree"),
        run.join("no-push"),
        target.join("debug"),
    ] {
        if !handed.exists() {
            continue;
        }
        for path in rubric_paths(&handed)? {
            if path.file_name().is_some_and(|name| name == steps_name)
                && !path.components().any(|part| part.as_os_str() == "target")
            {
                found.push(path.display().to_string());
            }
            if path.file_name() == Some(OsStr::new("task.json"))
                && path.strip_prefix(&handed)?.components().count() <= 2
                && read_json(&path).is_ok_and(|manifest| manifest.get("grade").is_some())
            {
                found.push(format!("{}(grade)", path.display()));
            }
        }
    }
    let worktree = run.join("worktree");
    if fs::read_to_string(worktree.join(GATE_DOCUMENT))
        .unwrap_or_default()
        .lines()
        .any(|line| {
            let level = line.chars().take_while(|c| *c == '#').count();
            (1..=3).contains(&level)
                && line[level..].starts_with(' ')
                && line.contains(PILOT_SECTION)
        })
    {
        found.push(format!("{GATE_DOCUMENT}({PILOT_SECTION})"));
    }
    for path in HARNESS_DOCUMENTS {
        if worktree.join(path).is_file() {
            found.push(path.to_owned());
        }
    }
    if let Ok(entries) = fs::read_dir(worktree.join("tools")) {
        for entry in entries {
            let path = entry?.path();
            if path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("agent-pilot-"))
            {
                found.push(path.display().to_string());
            }
        }
    }
    if worktree.join("tests/integration/src/agent_pilot").exists() {
        found.push("tests/integration/src/agent_pilot".to_owned());
    }
    if worktree.join(REPLAY_RESULTS).exists() {
        found.push(REPLAY_RESULTS.to_owned());
    }
    let mut parent = run;
    while let Some(up) = parent.parent() {
        if up == user_home || up == Path::new("/") {
            break;
        }
        if grading.starts_with(up) {
            found.push(format!(
                "{}(walk-up from {})",
                grading.display(),
                up.display()
            ));
        }
        parent = up;
    }
    anyhow::ensure!(
        found.is_empty(),
        "the grading rubric is reachable from a path the agent is handed: {}",
        found.join(" ")
    );
    Ok(())
}

impl Run {
    async fn resolve(args: RunArgs) -> Result<Self, GradeFailure> {
        let bytes = args.run.as_bytes();
        if !(bytes.len() == 3 || bytes.len() == 4 && bytes[3].is_ascii_lowercase())
            || !bytes[..3].iter().all(u8::is_ascii_digit)
        {
            return Err(failure(
                2,
                "--run takes three digits and an optional rerun letter, like 001 or 001b",
            ));
        }
        let tree = fs::canonicalize(&args.repository)?;
        if string(git(&tree).args(["rev-parse", "--show-toplevel"])).await?
            != tree.to_string_lossy()
        {
            return Err(failure(
                10,
                format!("{} is not the root of a git worktree", tree.display()),
            ));
        }
        let pilot = home("XDG_CACHE_HOME", ".cache")?.join("wamn-pilot");
        let grading = home("XDG_STATE_HOME", ".local/state")?.join("wamn-pilot-grading");
        directory(&pilot.join("runs"))?;
        let key = if let (Some(agent), Some(task)) = (&args.agent, &args.task) {
            let task =
                fs::canonicalize(task).map_err(|_| failure(2, "--task is not a directory"))?;
            format!(
                "{}-{agent}-{}",
                args.run,
                task.file_name()
                    .context("task directory has a name")?
                    .to_string_lossy()
            )
        } else {
            let matches = fs::read_dir(pilot.join("runs"))?
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.path().is_dir()
                        && entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with(&format!("{}-", args.run))
                })
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [] if args.verb == "down" => args.run.clone(),
                [entry] => entry.file_name().to_string_lossy().into_owned(),
                [] => {
                    return Err(failure(
                        10,
                        format!("no run directory for --run {}; run up first", args.run),
                    ));
                }
                _ => {
                    return Err(failure(
                        10,
                        format!("more than one run directory for --run {}", args.run),
                    ));
                }
            }
        };
        let directory = pilot.join("runs").join(&key);
        Ok(Self {
            args,
            tree,
            pilot,
            grading,
            directory,
            key,
            target: PathBuf::new(),
            task: Value::Null,
        })
    }

    fn read_manifest(&mut self) -> anyhow::Result<()> {
        self.task = read_json(&self.directory.join("task.json"))?;
        Ok(())
    }

    async fn resolve_target(&mut self, commit: &str) -> anyhow::Result<()> {
        let short = string(git(&self.tree).args(["rev-parse", "--short", commit])).await?;
        self.target = self.pilot.join(format!("target-{short}"));
        Ok(())
    }

    async fn logged(&self, command: &mut Command) -> anyhow::Result<()> {
        let log = process::append(&self.directory.join("env.log"))?;
        let status = command
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .kill_on_drop(true)
            .status()
            .await?;
        anyhow::ensure!(
            status.success(),
            "command exited {status}; see {}",
            self.directory.join("env.log").display()
        );
        Ok(())
    }

    async fn up(&mut self) -> Result<(), GradeFailure> {
        let agent = self
            .args
            .agent
            .as_deref()
            .ok_or_else(|| failure(2, "--agent is required for up"))?;
        if !["claude", "codex", "stub"].contains(&agent) {
            return Err(failure(2, "--agent takes claude, codex or stub"));
        }
        let task_dir = fs::canonicalize(
            self.args
                .task
                .as_ref()
                .ok_or_else(|| failure(2, "--task is required for up"))?,
        )?;
        if self.directory.exists() {
            return Err(failure(
                2,
                format!(
                    "run {} already exists at {}",
                    self.args.run,
                    self.directory.display()
                ),
            ));
        }
        let mut commit = string(
            git(&self.tree).args(["rev-parse", self.args.commit.as_deref().unwrap_or("HEAD")]),
        )
        .await?;
        self.preflight().await?;
        let task = read_json(&task_dir.join("task.json"))
            .map_err(|source| GradeFailure { code: 2, source })?;
        self.validate_manifest(&task, &task_dir, &commit).await?;
        for path in ["env", "fixture", "bin", "dev-logs", "grade"] {
            directory(&self.directory.join(path))?;
        }
        let grade_dir = self.grading.join(&self.args.run);
        if grade_dir.exists() {
            fs::remove_dir_all(&grade_dir)?;
        }
        directory(&grade_dir)?;
        fs::set_permissions(&self.grading, fs::Permissions::from_mode(0o700))?;
        copy_tree(&task_dir, &self.directory.join("fixture"))?;
        let steps_name = task["grade"]["steps"].as_str().unwrap_or("steps.json");
        fs::copy(task_dir.join(steps_name), grade_dir.join(steps_name))?;
        fs::copy(task_dir.join("task.json"), grade_dir.join("task.json"))?;
        fs::remove_file(self.directory.join("fixture").join(steps_name))?;
        self.task = task.clone();
        self.task
            .as_object_mut()
            .context("task manifest is an object")?
            .remove("grade");
        write_json(&self.directory.join("fixture/task.json"), &self.task)?;
        write_json(&self.directory.join("task.json"), &self.task)?;
        for name in [
            "verbs.jsonl",
            "verbs-started.jsonl",
            "verbs.lock",
            "env.log",
        ] {
            write(&self.directory.join(name), b"")?;
        }
        write(&self.directory.join("verbs.counter"), b"0\n")?;
        self.logged(
            git(&self.tree)
                .args(["worktree", "add", "--detach"])
                .arg(self.directory.join("worktree"))
                .arg(&commit),
        )
        .await?;
        let worktree = self.directory.join("worktree");
        let mut removed = Vec::new();
        for path in [
            "evidence/experiments",
            "tests/integration/fixtures/agent-pilot",
            "tests/integration/src/agent_pilot",
            REPLAY_RESULTS,
        ]
        .into_iter()
        .chain(HARNESS_DOCUMENTS)
        {
            if worktree.join(path).exists() {
                removed.push(path.to_owned());
            }
        }
        removed.extend(
            string(git(&worktree).args(["ls-files", "tools/agent-pilot-*"]))
                .await?
                .lines()
                .map(str::to_owned),
        );
        let mut modified = Vec::new();
        if strip_document_section(&worktree.join(GATE_DOCUMENT), PILOT_SECTION)? {
            modified.push(GATE_DOCUMENT);
        }
        // Remove only the direct declarations for the experiment that this run withholds.
        for path in [
            "tests/integration/src/lib.rs",
            "tests/orchestrator/src/main.rs",
        ] {
            let original = fs::read_to_string(worktree.join(path))?;
            let mut filtered = String::new();
            let mut depth = 0usize;
            for line in original.lines() {
                let direct = line.contains("agent_pilot")
                    || line.contains("AgentPilotRun(")
                    || line.contains("AgentPilotGrade(");
                if direct || depth > 0 {
                    depth = (depth + line.matches('{').count())
                        .saturating_sub(line.matches('}').count());
                    continue;
                }
                filtered.push_str(line);
                filtered.push('\n');
            }
            if filtered != original {
                fs::write(worktree.join(path), filtered)?;
                modified.push(path);
            }
        }
        if !removed.is_empty() {
            self.logged(
                git(&worktree)
                    .args(["rm", "-r", "--quiet", "--"])
                    .args(&removed),
            )
            .await?;
        }
        if !modified.is_empty() {
            self.logged(git(&worktree).args(["add", "--"]).args(&modified))
                .await?;
        }
        if !removed.is_empty() || !modified.is_empty() {
            self.logged(git(&worktree).args([
                "-c",
                "user.name=pilot",
                "-c",
                "user.email=pilot@invalid",
                "commit",
                "--no-verify",
                "--quiet",
                "-m",
                "chore(pilot): the experiment is not part of the tree it measures",
            ]))
            .await?;
            commit = string(git(&worktree).args(["rev-parse", "HEAD"])).await?;
        }
        if !string(git(&worktree).args(["status", "--porcelain=v1", "--untracked-files=all"]))
            .await?
            .is_empty()
        {
            return Err(failure(10, "the run worktree is not clean"));
        }
        self.resolve_target(&commit).await?;
        assert_rubric_unreachable(
            &self.directory,
            &self.target,
            &self.grading,
            &PathBuf::from(std::env::var_os("HOME").context("HOME is required")?),
            steps_name,
        )?;
        let seconds = self.build().await?;
        self.record_skills()?;
        let baseline = self.pilot.join("skills-baseline.json");
        if baseline.is_file() {
            if read_json(&baseline)? != read_json(&self.directory.join("skills.json"))? {
                return Err(failure(
                    10,
                    "the skill list differs from the frozen baseline; arms would not compare",
                ));
            }
        } else {
            fs::copy(self.directory.join("skills.json"), baseline)?;
        }
        fs::copy(
            self.tree.join("tools/agent-pilot-wamn-shim"),
            self.directory.join("bin/wamn"),
        )?;
        fs::set_permissions(
            self.directory.join("bin/wamn"),
            fs::Permissions::from_mode(0o755),
        )?;
        symlink(
            self.target.join("debug/wamn-ctl-ops"),
            self.directory.join("bin/wamn-ctl-ops"),
        )?;
        if !executable(&self.directory.join("bin/wamn-ctl-ops")) {
            return Err(failure(10, "wamn-ctl-ops does not resolve"));
        }
        let broker = self.start_infrastructure().await?;
        self.standup(&broker).await?;
        let mut stages = 0;
        if let Some(baseline) = self.task["baseline"]["overlay_root"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            let result = Command::new(self.target.join("debug/wamn"))
                .current_dir(&worktree)
                .args(["dev", "--config"])
                .arg(self.directory.join("env/dev.json"))
                .arg("--overlay-root")
                .arg(worktree.join(baseline))
                .kill_on_drop(true)
                .output()
                .await?;
            let mut bytes = result.stdout;
            bytes.extend(result.stderr);
            write(&self.directory.join("baseline.out"), &bytes)?;
            stages = super::loop_result(&String::from_utf8_lossy(&bytes))["stages"]
                .as_u64()
                .unwrap_or(0);
            if stages != 12 {
                let _ = self.down().await;
                return Err(failure(
                    10,
                    format!("the baseline run reported {stages} stages, not twelve"),
                ));
            }
        }
        let load = fs::read_to_string("/proc/loadavg")?
            .split_whitespace()
            .take(3)
            .collect::<Vec<_>>()
            .join(" ");
        write_json(
            &self.directory.join("run.json"),
            &json!({"run":self.key,"commit":commit,"task":self.task["task"],"agent":self.args.agent,"standup":self.args.standup,
            "machine":{"load_at_launch":load,"cores":std::thread::available_parallelism()?.get()},"build":{"cached":seconds<30,"seconds":seconds,"target":self.target},
            "up":{"ok":true,"baseline_stages":stages},"launch":null,"verbs":null,"git":null,"down":null}),
        )?;
        eprintln!("[pilot] up ok: {}", self.directory.display());
        Ok(())
    }

    async fn validate_manifest(
        &self,
        task: &Value,
        task_dir: &Path,
        commit: &str,
    ) -> Result<(), GradeFailure> {
        for field in ["org", "project", "env", "schema", "tenant", "route_host"] {
            if text(&task["identity"][field]).is_empty() {
                return Err(failure(2, format!("task.json identity.{field} is empty")));
            }
        }
        let overlay = text(&task["overlay_root"]);
        if overlay.is_empty() {
            return Err(failure(2, "task.json overlay_root is empty"));
        }
        let allowed = task["allowed_paths"]
            .as_array()
            .filter(|paths| !paths.is_empty())
            .ok_or_else(|| failure(2, "task.json allowed_paths is empty"))?;
        if !allowed.iter().any(|pattern| {
            matches_pattern(overlay, text(pattern))
                || matches_pattern(&format!("{overlay}/x"), text(pattern))
        }) {
            return Err(failure(
                2,
                format!("allowed_paths does not cover overlay_root {overlay}"),
            ));
        }
        for source in task["package_sources"].as_array().into_iter().flatten() {
            output(git(&self.tree).args(["cat-file", "-e", &format!("{commit}:{}", text(source))]))
                .await
                .map_err(|_| {
                    failure(
                        2,
                        format!("package source {} is absent at {commit}", text(source)),
                    )
                })?;
        }
        for field in ["event_stream_replicas", "event_dup_window_secs"] {
            if task["environment"][field].as_u64().is_none() {
                return Err(failure(
                    2,
                    format!("task.json environment.{field} must declare an unsigned integer"),
                ));
            }
        }
        let steps = task_dir.join(task["grade"]["steps"].as_str().unwrap_or("steps.json"));
        validate_steps(&read_json(&steps).map_err(|source| GradeFailure { code: 2, source })?)
    }

    async fn preflight(&self) -> anyhow::Result<()> {
        for tool in [
            "docker",
            "cargo",
            "jq",
            "psql",
            "curl",
            "git",
            "openssl",
            "flock",
            "ss",
            "tee",
            "sha256sum",
            "bash",
        ] {
            anyhow::ensure!(
                process::find_executable(tool).is_some(),
                "{tool} is not on PATH"
            );
        }
        let version = string(Command::new("bash").args(["--version"])).await?;
        let number = version
            .lines()
            .next()
            .unwrap_or_default()
            .split("version ")
            .nth(1)
            .unwrap_or_default()
            .split('.')
            .take(2)
            .filter_map(|s| s.parse::<u64>().ok())
            .collect::<Vec<_>>();
        anyhow::ensure!(
            number.as_slice() >= [5, 1].as_slice(),
            "bash 5.1 or newer is required"
        );
        for port in PORTS {
            anyhow::ensure!(!process::listening(port).await?, "port {port} is in use");
        }
        anyhow::ensure!(
            string(git(&self.tree).args(["status", "--porcelain=v1", "--untracked-files=all"]))
                .await?
                .is_empty(),
            "the source tree is not clean"
        );
        for manifest in ["Cargo.toml", "apps/Cargo.toml"] {
            output(
                Command::new("cargo")
                    .arg("metadata")
                    .arg("--manifest-path")
                    .arg(self.tree.join(manifest))
                    .args(["--no-deps", "--offline", "--format-version", "1"]),
            )
            .await?;
        }
        Ok(())
    }

    async fn build(&self) -> anyhow::Result<u64> {
        let started = Instant::now();
        directory(&self.target)?;
        let root = self.directory.join("worktree");
        let before = tracked_hash(&root).await?;
        let head_before = string(git(&root).args(["rev-parse", "HEAD"])).await?;
        for (manifest, args) in [
            (
                "Cargo.toml",
                vec![
                    "-p",
                    "wamn-ctl",
                    "--features",
                    "ops",
                    "--bin",
                    "wamn",
                    "--bin",
                    "wamn-ctl-ops",
                ],
            ),
            ("Cargo.toml", vec!["-p", "wamn-host", "-p", "wamn-identity"]),
            ("Cargo.toml", vec!["-p", "wamn-scenario-worker"]),
            (
                "Cargo.toml",
                vec!["-p", "wamn-integration-tests", "--bin", "wamn-dev-env"],
            ),
            (
                "apps/Cargo.toml",
                vec!["-p", "http-route", "--target", "wasm32-wasip2"],
            ),
        ] {
            self.logged(
                Command::new("cargo")
                    .arg("build")
                    .arg("--manifest-path")
                    .arg(root.join(manifest))
                    .args(args)
                    .args(["--locked", "--offline"])
                    .env("RUSTC_WRAPPER", "")
                    .env("CARGO_TARGET_DIR", &self.target),
            )
            .await?;
        }
        let after = tracked_hash(&root).await?;
        let head_after = string(git(&root).args(["rev-parse", "HEAD"])).await?;
        anyhow::ensure!(
            before == after && head_before == head_after,
            "the run worktree changed while the run was building; the binaries no longer match the pinned commit"
        );
        for artifact in [
            "wamn",
            "wamn-ctl-ops",
            "wamn-identity",
            "wamn-host",
            "wamn-scenario-worker",
            "wamn-dev-env",
        ] {
            anyhow::ensure!(
                executable(&self.target.join("debug").join(artifact)),
                "{artifact} was not built"
            );
        }
        anyhow::ensure!(
            self.target
                .join("wasm32-wasip2/debug/http_route.wasm")
                .metadata()?
                .len()
                > 0,
            "the flow-http workload was not built"
        );
        let mut artifacts = Vec::new();
        for artifact in [
            "debug/wamn",
            "debug/wamn-ctl-ops",
            "debug/wamn-identity",
            "debug/wamn-host",
            "debug/wamn-scenario-worker",
            "debug/wamn-dev-env",
            "wasm32-wasip2/debug/http_route.wasm",
        ] {
            let bytes = fs::read(self.target.join(artifact))?;
            artifacts.push(json!({"path": artifact, "bytes": bytes.len(), "sha256": hex::encode(Sha256::digest(&bytes))}));
        }
        write_json(
            &self.directory.join("build.json"),
            &json!({"head_before":head_before,"head_after":head_after,"source_before":before,"source_after":after,"source_unchanged":true,"artifacts":artifacts}),
        )?;
        Ok(started.elapsed().as_secs())
    }

    fn record_skills(&self) -> anyhow::Result<()> {
        let user_home = PathBuf::from(std::env::var_os("HOME").context("HOME is required")?);
        let worktree = self.directory.join("worktree");
        let mut skills = Vec::new();
        for root in [
            user_home.join(".claude/skills"),
            user_home.join(".agents/skills"),
            user_home.join(".codex/skills"),
            worktree.join(".agents/skills"),
            worktree.join(".claude/skills"),
        ] {
            if !root.is_dir() {
                continue;
            }
            let mut files = super::contracts::files(&root)?;
            files.sort();
            for path in files
                .into_iter()
                .filter(|path| path.file_name() == Some(OsStr::new("SKILL.md")))
            {
                skills.push(json!({"name":path.parent().and_then(Path::file_name).context("skill has a directory")?.to_string_lossy(),"path":path.strip_prefix(&worktree).unwrap_or(&path),"sha256":hex::encode(Sha256::digest(fs::read(&path)?))}));
            }
        }
        write_json(&self.directory.join("skills.json"), &skills)?;
        let mut settings = Vec::new();
        for path in [
            user_home.join(".claude/settings.json"),
            user_home.join(".claude/settings.local.json"),
            user_home.join(".codex/config.toml"),
            worktree.join(".claude/settings.json"),
        ] {
            if path.is_file() {
                settings.push(json!({"path":path.strip_prefix(&user_home).map(|path|format!("~/{}",path.display())).unwrap_or_else(|_|path.display().to_string()),"sha256":hex::encode(Sha256::digest(fs::read(&path)?))}));
            }
        }
        write_json(&self.directory.join("settings.json"), &settings)
    }

    async fn final_git(&self) -> anyhow::Result<()> {
        let worktree = self.directory.join("worktree");
        if !worktree.is_dir() {
            return Ok(());
        }
        let mut run = read_json(&self.directory.join("run.json"))?;
        let commit = text(&run["commit"]);
        let mut records = Vec::new();
        for (name, args) in [
            ("final.diff", vec!["diff".to_owned(), commit.to_owned()]),
            (
                "final.status",
                vec![
                    "status".to_owned(),
                    "--porcelain=v1".to_owned(),
                    "--untracked-files=all".to_owned(),
                ],
            ),
            (
                "commits.log",
                vec![
                    "log".to_owned(),
                    "--oneline".to_owned(),
                    format!("{commit}..HEAD"),
                ],
            ),
        ] {
            let bytes = output(git(&worktree).args(args)).await.unwrap_or_default();
            write(&self.directory.join(name), &bytes)?;
            records.push(String::from_utf8(bytes)?);
        }
        let diff = string(git(&worktree).args(["diff", "--name-only", commit]))
            .await
            .unwrap_or_default();
        let changed = diff
            .lines()
            .chain(records[1].lines().filter_map(|line| line.get(3..)))
            .filter(|line| !line.is_empty())
            .collect::<BTreeSet<_>>();
        let allowed = self.task["allowed_paths"]
            .as_array()
            .context("task names allowed paths")?;
        let outside = changed
            .iter()
            .filter(|path| {
                !allowed
                    .iter()
                    .any(|pattern| matches_pattern(path, text(pattern)))
            })
            .copied()
            .collect::<Vec<_>>();
        run["git"] = json!({"commits":records[2].lines().count(),"files_changed":changed.len(),"outside_allowed_paths":outside});
        write_json(&self.directory.join("run.json"), &run)
    }
}

/// Execute the selected action and retain its existing exit-code categories.
pub async fn run(args: RunArgs) -> u8 {
    match run_inner(args).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("agent-pilot-run: {}", error.source);
            error.code
        }
    }
}

async fn run_inner(args: RunArgs) -> Result<u8, GradeFailure> {
    let mut run = Run::resolve(args).await?;
    let verb = run.args.verb.clone();
    match verb.as_str() {
        "up" => {
            run.up().await?;
            Ok(0)
        }
        "launch" => {
            run.read_manifest()?;
            run.launch().await
        }
        "grade" => {
            run.read_manifest()?;
            run.final_git().await?;
            Ok(
                if super::grade(GradeArgs {
                    run: Some(run.args.run.clone()),
                    replay: None,
                    placement: None,
                    contract: None,
                })
                .await
                    == 0
                {
                    0
                } else {
                    30
                },
            )
        }
        "down" => run.down().await,
        "all" => {
            run.up().await?;
            let action = async {
                let mut status = run.launch().await?;
                run.final_git().await?;
                if status == 0 {
                    status = if super::grade(GradeArgs {
                        run: Some(run.args.run.clone()),
                        replay: None,
                        placement: None,
                        contract: None,
                    })
                    .await
                        == 0
                    {
                        0
                    } else {
                        30
                    };
                }
                Ok::<_, GradeFailure>(status)
            }
            .await;
            let down = run.down().await?;
            if down != 0 {
                return Ok(down);
            }
            action
        }
        _ => Err(failure(2, "unknown pilot action")),
    }
}
