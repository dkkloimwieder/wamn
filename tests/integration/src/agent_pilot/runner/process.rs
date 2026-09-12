//! Observe and stop only processes and storage owned by the selected run.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::process::{Child, Command};

use super::{PORTS, Run, directory, executable, git, output, string};
use crate::agent_pilot::{GradeFailure, failure, read_json, read_lines, text, write, write_json};

pub(super) fn append(path: &Path) -> anyhow::Result<File> {
    Ok(OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?)
}

pub(super) fn find_executable(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|part| part.join(name))
        .find(|path| executable(path))
}

pub(super) async fn listening(port: u16) -> anyhow::Result<bool> {
    Ok(
        !output(Command::new("ss").args(["-Hltn", &format!("sport = :{port}")]))
            .await?
            .is_empty(),
    )
}

fn signal(pid: i32, signal: i32) -> bool {
    // Callers supply a process or group selected from this run's child or owned paths.
    unsafe { libc::kill(pid, signal) == 0 }
}

fn alive(pid: i32) -> bool {
    if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
        return stat
            .rsplit_once(") ")
            .is_some_and(|(_, fields)| !fields.starts_with('Z'));
    }
    false
}

fn fingerprint(root: &Path) -> anyhow::Result<String> {
    fn visit(root: &Path, hash: &mut Sha256) -> anyhow::Result<()> {
        let mut entries = fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            if ["target", ".git", "node_modules"]
                .iter()
                .any(|name| entry.file_name() == *name)
            {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                visit(&entry.path(), hash)?;
            } else if kind.is_file() {
                let metadata = entry.metadata()?;
                hash.update(format!(
                    "{}.{:09} {} {}\n",
                    metadata.mtime(),
                    metadata.mtime_nsec(),
                    metadata.len(),
                    entry.path().display()
                ));
            }
        }
        Ok(())
    }
    let mut hash = Sha256::new();
    visit(root, &mut hash)?;
    Ok(hex::encode(hash.finalize()))
}

async fn stop(child: &mut Child) -> anyhow::Result<()> {
    if let Some(pid) = child.id() {
        let pid = i32::try_from(pid)?;
        if !signal(-pid, libc::SIGTERM) {
            signal(pid, libc::SIGTERM);
        }
        match tokio::time::timeout(Duration::from_secs(15), child.wait()).await {
            Ok(result) => {
                result?;
            }
            Err(_) => {
                if !signal(-pid, libc::SIGKILL) {
                    signal(pid, libc::SIGKILL);
                }
                child.wait().await?;
            }
        }
    }
    Ok(())
}

fn timeout_reason(
    elapsed: Duration,
    last_line: Duration,
    last_change: Duration,
) -> Option<&'static str> {
    if elapsed > Duration::from_secs(5400) {
        Some("run-cap")
    } else if elapsed.saturating_sub(last_line) > Duration::from_secs(1200) {
        Some("step-timeout")
    } else if elapsed.saturating_sub(last_line) > Duration::from_secs(300)
        && elapsed.saturating_sub(last_change) > Duration::from_secs(300)
    {
        Some("idle-timeout")
    } else {
        None
    }
}

async fn watch(child: &mut Child, run: &Path) -> anyhow::Result<&'static str> {
    let start = Instant::now();
    let mut last_line = Duration::ZERO;
    let mut last_change = Duration::ZERO;
    let mut last_size = fs::metadata(run.join("transcript.jsonl"))?.len();
    let mut last_tree = fingerprint(&run.join("worktree"))?;
    loop {
        if child.try_wait()?.is_some() {
            return Ok("completed");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
        let now = start.elapsed();
        let size = fs::metadata(run.join("transcript.jsonl"))?.len();
        if size != last_size {
            last_size = size;
            last_line = now;
        }
        let tree = fingerprint(&run.join("worktree"))?;
        if tree != last_tree {
            last_tree = tree;
            last_change = now;
        }
        if let Some(reason) = timeout_reason(now, last_line, last_change) {
            stop(child).await?;
            return Ok(reason);
        }
    }
}

fn first_value(rows: &[Value], field: &str) -> String {
    fn find(value: &Value, field: &str) -> Option<String> {
        match value {
            Value::Object(values) => values
                .get(field)
                .filter(|value| !value.is_null())
                .map(|value| text(value).to_owned())
                .or_else(|| values.values().find_map(|value| find(value, field))),
            Value::Array(values) => values.iter().find_map(|value| find(value, field)),
            _ => None,
        }
    }
    rows.iter()
        .find_map(|value| find(value, field))
        .unwrap_or_default()
}

fn first_green(run: &Path, rows: &[Value], started: &str) -> anyhow::Result<i64> {
    for row in rows {
        let prefix = format!("{}-", text(&row["n"]));
        let mut entries = fs::read_dir(run.join("dev-logs"))?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with(&prefix) || !name.ends_with(".out") {
                continue;
            }
            let output = fs::read_to_string(entry.path())?;
            if super::super::loop_result(&output)["stages"] == 12 {
                let at = DateTime::parse_from_rfc3339(text(&row["ts"]))?;
                let started = DateTime::parse_from_rfc3339(started)?;
                return Ok((at.timestamp() - started.timestamp()) / 60);
            }
        }
    }
    Ok(0)
}

fn owned_processes(run: &Path) -> anyhow::Result<Vec<i32>> {
    let worktree = run.join("worktree").to_string_lossy().into_owned();
    let environment = run.join("env").to_string_lossy().into_owned();
    let mut pids = Vec::new();
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        if pid == std::process::id() as i32 {
            continue;
        }
        if let Ok(bytes) = fs::read(entry.path().join("cmdline")) {
            let command = String::from_utf8_lossy(&bytes);
            if command.contains(&worktree) || command.contains(&environment) {
                pids.push(pid);
            }
        }
    }
    Ok(pids)
}

impl Run {
    fn driver_path(&self) -> anyhow::Result<std::ffi::OsString> {
        let mut paths = Vec::new();
        let mut index = 0;
        for part in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
            if executable(&part.join("bd")) {
                let shadow = self.directory.join(format!("bin/shadow-{index}"));
                directory(&shadow)?;
                for entry in fs::read_dir(&part)? {
                    let entry = entry?;
                    if entry.file_name() == "bd"
                        || entry.file_name().to_string_lossy().starts_with('.')
                    {
                        continue;
                    }
                    if executable(&entry.path()) || entry.file_type()?.is_symlink() {
                        let target = shadow.join(entry.file_name());
                        if target.symlink_metadata().is_ok() {
                            fs::remove_file(&target)?;
                        }
                        symlink(entry.path(), target)?;
                    }
                }
                paths.push(shadow);
                index += 1;
            } else {
                paths.push(part);
            }
        }
        paths.insert(0, self.target.join("debug"));
        paths.insert(0, self.directory.join("bin"));
        Ok(std::env::join_paths(paths)?)
    }

    pub(super) async fn launch(&mut self) -> Result<u8, GradeFailure> {
        self.read_manifest()?;
        if !self.directory.join("env/dev.json").is_file() {
            return Err(failure(10, "no environment; run up first"));
        }
        let mut run = read_json(&self.directory.join("run.json"))?;
        let agent = text(&run["agent"]).to_owned();
        self.resolve_target(text(&run["commit"])).await?;
        if !executable(&self.target.join("debug/wamn")) {
            return Err(failure(
                10,
                format!(
                    "no built wamn at {}",
                    self.target.join("debug/wamn").display()
                ),
            ));
        }
        let brief = fs::read_to_string(self.directory.join("fixture/BRIEF.md"))?;
        let (binary, args) = match agent.as_str() {
            "claude" => (
                PathBuf::from("claude"),
                vec![
                    "--print".to_owned(),
                    "--output-format".to_owned(),
                    "stream-json".to_owned(),
                    "--verbose".to_owned(),
                    "--permission-mode".to_owned(),
                    "bypassPermissions".to_owned(),
                    brief,
                ],
            ),
            "codex" => (
                PathBuf::from("codex"),
                vec![
                    "exec".to_owned(),
                    "--dangerously-bypass-approvals-and-sandbox".to_owned(),
                    "--json".to_owned(),
                    brief,
                ],
            ),
            "stub" => (
                self.tree.join("tools/agent-pilot-stub-driver"),
                vec!["--stub-mode".to_owned(), self.args.stub_mode.clone()],
            ),
            _ => return Err(failure(10, format!("unknown recorded agent {agent}"))),
        };
        if !executable(&binary) && find_executable(&binary.to_string_lossy()).is_none() {
            return Err(failure(10, format!("the {agent} driver is not on PATH")));
        }
        let started = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        write(&self.directory.join("transcript.jsonl"), b"")?;
        let mut child = Command::new(&binary)
            .args(args)
            .current_dir(self.directory.join("worktree"))
            .env_remove("CARGO_TARGET_DIR")
            .env("PATH", self.driver_path()?)
            .env("WAMN_PILOT_REAL_WAMN", self.target.join("debug/wamn"))
            .env("WAMN_DEV_CONFIG", self.directory.join("env/dev.json"))
            .env(
                "WAMN_ROUTE_HOST",
                text(&self.task["identity"]["route_host"]),
            )
            .env(
                "WAMN_ROUTE_CALLER_PAT_FILE",
                self.directory.join("env/route-caller-pat.json"),
            )
            .env("WAMN_PILOT_RUN_DIR", &self.directory)
            .env("WAMN_PILOT_TASK_DIR", self.directory.join("fixture"))
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "remote.origin.pushurl")
            .env("GIT_CONFIG_VALUE_0", self.directory.join("no-push"))
            .stdout(Stdio::from(append(
                &self.directory.join("transcript.jsonl"),
            )?))
            .stderr(Stdio::from(append(&self.directory.join("env.log"))?))
            .process_group(0)
            .kill_on_drop(true)
            .spawn()?;
        let reason = watch(&mut child, &self.directory).await?;
        let status = child.wait().await?;
        use std::os::unix::process::ExitStatusExt as _;
        let exit = status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
        let ended = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let transcript = read_lines(&self.directory.join("transcript.jsonl")).unwrap_or_default();
        let driver = json!({"agent":agent,"binary":binary,"started":started,"ended":ended,"exit":exit,"reason":reason,
            "model_id":first_value(&transcript,"model"),"session_id":first_value(&transcript,"session_id"),"output_style":first_value(&transcript,"output_style")});
        write_json(&self.directory.join("driver.json"), &driver)?;
        let starts = read_lines(&self.directory.join("verbs-started.jsonl"))?;
        let ends = read_lines(&self.directory.join("verbs.jsonl"))?;
        let runs = starts.iter().filter(|row| row["argv"][0] == "dev").count();
        let failed = ends
            .iter()
            .filter(|row| row["argv"][0] == "dev" && row["exit"] != 0)
            .count();
        let holds = starts
            .iter()
            .filter(|row| {
                row["argv"][0] == "dev"
                    && row["argv"]
                        .as_array()
                        .is_some_and(|args| args.contains(&json!("--hold")))
            })
            .count();
        run["launch"] = driver;
        run["verbs"] = json!({"wamn_dev_runs":runs,"wamn_dev_failed":failed,"wamn_dev_hold_runs":holds,"first_green_minutes":first_green(&self.directory,&starts,&started)?});
        write_json(&self.directory.join("run.json"), &run)?;
        eprintln!("[pilot] launch ended: {reason}");
        Ok(if reason == "completed" { 0 } else { 20 })
    }

    pub(super) async fn down(&self) -> Result<u8, GradeFailure> {
        if !self.directory.is_dir() {
            eprintln!(
                "[pilot] no run directory for {}; nothing to tear down",
                self.args.run
            );
            return Ok(0);
        }
        let mut residue = Vec::new();
        if let Ok(pid) = fs::read_to_string(self.directory.join("env/standup.pid")) {
            if let Ok(pid) = pid.trim().parse::<i32>() {
                if alive(pid) && !owned_processes(&self.directory)?.contains(&pid) {
                    residue.push(format!("standup PID {pid} no longer belongs to this run"));
                } else if alive(pid) {
                    signal(pid, libc::SIGINT);
                    for _ in 0..30 {
                        if !alive(pid) {
                            break;
                        }
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                    if owned_processes(&self.directory)?.contains(&pid) {
                        signal(pid, libc::SIGKILL);
                    }
                }
            }
        }
        // The caller's process group is never a cleanup target.
        let own_group = unsafe { libc::getpgrp() };
        let mut groups = BTreeSet::new();
        for pid in owned_processes(&self.directory)? {
            let group = unsafe { libc::getpgid(pid) };
            if group > 0 && group != own_group {
                groups.insert(group);
            }
        }
        for group in &groups {
            signal(-group, libc::SIGTERM);
        }
        if !groups.is_empty() {
            for _ in 0..20 {
                if owned_processes(&self.directory)?.is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            let remaining = owned_processes(&self.directory)?;
            for group in &groups {
                // Query current members again before sending the final group signal.
                if remaining
                    .iter()
                    .any(|pid| unsafe { libc::getpgid(*pid) } == *group)
                {
                    signal(-group, libc::SIGKILL);
                }
            }
        }
        residue.extend(
            owned_processes(&self.directory)?
                .into_iter()
                .map(|pid| format!("process {pid}")),
        );
        let _ = self.logged(&mut self.lifecycle("down")).await;
        if self.args.discard_worktree && self.directory.join("worktree").is_dir() {
            let _ = self
                .logged(
                    git(&self.tree)
                        .args(["worktree", "remove", "--force"])
                        .arg(self.directory.join("worktree")),
                )
                .await;
        }
        match string(&mut self.lifecycle("containers")).await {
            Ok(labelled) if !labelled.is_empty() => residue.push(format!("containers {labelled}")),
            Ok(_) => (),
            Err(error) => residue.push(format!("container inspection failed: {error}")),
        }
        for port in PORTS {
            if listening(port).await? {
                residue.push(format!("listener on {port}"));
            }
        }
        if self.directory.join("run.json").is_file() {
            let mut run = read_json(&self.directory.join("run.json"))?;
            run["down"] = json!({"ok":residue.is_empty(),"residue":residue});
            write_json(&self.directory.join("run.json"), &run)?;
        }
        if !residue.is_empty() {
            eprintln!("agent-pilot-run: teardown residue: {}", residue.join(" "));
            return Ok(40);
        }
        self.reclaim().await?;
        eprintln!("[pilot] down ok");
        Ok(0)
    }

    async fn reclaim(&self) -> anyhow::Result<()> {
        let promoted = self
            .tree
            .join("evidence/experiments/agent-authoring")
            .join(&self.key)
            .join("run.json");
        if !promoted.metadata().is_ok_and(|metadata| metadata.len() > 0) {
            eprintln!(
                "[pilot] keeping {} and its build target: no promoted evidence at {}",
                self.directory.display(),
                promoted.display()
            );
            return Ok(());
        }
        anyhow::ensure!(
            self.directory.parent() == Some(self.pilot.join("runs").as_path()),
            "refusing to reclaim {}",
            self.directory.display()
        );
        let target = read_json(&self.directory.join("run.json"))
            .ok()
            .and_then(|run| run["build"]["target"].as_str().map(PathBuf::from));
        if self.directory.join("worktree").is_dir() {
            let _ = self
                .logged(
                    git(&self.tree)
                        .args(["worktree", "remove", "--force"])
                        .arg(self.directory.join("worktree")),
                )
                .await;
        }
        fs::remove_dir_all(&self.directory)?;
        let Some(target) = target else {
            return Ok(());
        };
        if target.parent() != Some(self.pilot.as_path())
            || !target
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("target-") && name.len() > 7)
            || !target.is_dir()
        {
            return Ok(());
        }
        for entry in fs::read_dir(self.pilot.join("runs"))? {
            let path = entry?.path().join("run.json");
            if read_json(&path).is_ok_and(|run| run["build"]["target"].as_str() == target.to_str())
            {
                eprintln!(
                    "[pilot] keeping {}: another run still names it",
                    target.display()
                );
                return Ok(());
            }
        }
        fs::remove_dir_all(&target)?;
        eprintln!("[pilot] reclaimed {}", target.display());
        Ok(())
    }
}
