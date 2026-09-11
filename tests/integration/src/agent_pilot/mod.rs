//! Run and grade the retained agent-authoring experiment.

mod contracts;
mod grading;
mod live;
mod runner;
pub use runner::{RunArgs, run};
#[cfg(test)]
mod tests;

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::Args;
use serde_json::{Value, json};
use tokio::process::Command;

use grading::Grading;

/// Select the existing live, replay, placement, or contract grading mode.
#[derive(Debug, Args)]
pub struct GradeArgs {
    #[arg(long)]
    run: Option<String>,
    #[arg(long)]
    replay: Option<PathBuf>,
    #[arg(long)]
    placement: Option<PathBuf>,
    #[arg(long)]
    contract: Option<PathBuf>,
}

#[derive(Debug)]
struct GradeFailure {
    code: u8,
    source: anyhow::Error,
}

impl From<anyhow::Error> for GradeFailure {
    fn from(source: anyhow::Error) -> Self {
        Self { code: 10, source }
    }
}

impl From<serde_json::Error> for GradeFailure {
    fn from(source: serde_json::Error) -> Self {
        Self {
            code: 10,
            source: source.into(),
        }
    }
}

impl From<std::io::Error> for GradeFailure {
    fn from(source: std::io::Error) -> Self {
        Self {
            code: 10,
            source: source.into(),
        }
    }
}

fn failure(code: u8, message: impl std::fmt::Display) -> GradeFailure {
    GradeFailure {
        code,
        source: anyhow::anyhow!("{message}"),
    }
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

fn read_json(path: &Path) -> anyhow::Result<Value> {
    serde_json::from_slice(&fs::read(path).with_context(|| format!("read {}", path.display()))?)
        .with_context(|| format!("parse {}", path.display()))
}

fn read_lines(path: &Path) -> anyhow::Result<Vec<Value>> {
    fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).context("parse a recorded JSON line"))
        .collect()
}

fn write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)?;
    Ok(())
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> anyhow::Result<()> {
    write(path, &serde_json::to_vec_pretty(value)?)
}

fn home(variable: &str, suffix: &str) -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os(variable) {
        Ok(PathBuf::from(path))
    } else {
        Ok(std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is required")?
            .join(suffix))
    }
}

fn loop_result(output: &str) -> Value {
    let stages = output
        .lines()
        .filter_map(|line| line.strip_prefix("run completed: "))
        .flat_map(|line| line.split(','))
        .filter(|stage| !stage.is_empty())
        .count();
    let base = output
        .lines()
        .filter_map(|line| line.strip_prefix("run served: "))
        .filter_map(|line| line.split_once(" host=").map(|(url, _)| url))
        .next()
        .unwrap_or_default();
    json!({"pass":stages==12 && !base.is_empty(),"stages":stages,"base_url":base})
}

fn expand_run(value: &mut Value, run: &str) {
    match value {
        Value::String(value) => *value = value.replace("{{run}}", run),
        Value::Array(values) => {
            for value in values {
                expand_run(value, run)
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                expand_run(value, run)
            }
        }
        _ => (),
    }
}

#[derive(Debug)]
struct GradeContext {
    grading: Grading,
    task: Value,
    recorded: Value,
    brief: PathBuf,
    replay: bool,
    results: PathBuf,
    checklist: PathBuf,
}

impl GradeContext {
    fn load(args: &GradeArgs) -> Result<Self, GradeFailure> {
        let pilot = home("XDG_CACHE_HOME", ".cache")?.join("wamn-pilot");
        let grading_home = home("XDG_STATE_HOME", ".local/state")?.join("wamn-pilot-grading");
        let explicit = args
            .contract
            .as_ref()
            .or(args.placement.as_ref())
            .or(args.replay.as_ref());
        let directory = if let Some(path) = explicit {
            fs::canonicalize(path)
                .map_err(|_| failure(2, format!("{} is not a directory", path.display())))?
        } else {
            let run = args
                .run
                .as_deref()
                .filter(|run| !run.is_empty())
                .ok_or_else(|| {
                    failure(2, "--run, --replay, --placement or --contract is required")
                })?;
            let matches = fs::read_dir(pilot.join("runs"))
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.path().is_dir()
                        && entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with(&format!("{run}-"))
                })
                .map(|entry| entry.path())
                .collect::<Vec<_>>();
            if matches.len() != 1 {
                return Err(failure(
                    10,
                    format!("expected exactly one run directory for {run}"),
                ));
            }
            matches[0].clone()
        };
        if !directory.is_dir() {
            return Err(failure(2, "the grading input is not a directory"));
        }
        let manifest = directory.join("task.json");
        let task = read_json(&manifest)?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory.join("grade"))
            .map_err(anyhow::Error::from)?;
        let key = directory
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| failure(2, "the run directory has no name"))?;
        let number = key.split('-').next().unwrap_or(key);
        let mut grading = grading_home.join(number);
        if read_json(&grading.join("task.json"))
            .ok()
            .is_none_or(|task| task.get("grade").is_none())
        {
            grading = pilot.join("grading").join(number);
        }
        let grade_task = read_json(&grading.join("task.json"))
            .ok()
            .filter(|task| task.get("grade").is_some())
            .unwrap_or_else(|| task.clone());
        if grade_task.get("grade").is_none() {
            return Err(failure(
                10,
                format!(
                    "no grade block for {key} in {} or {}",
                    grading.display(),
                    manifest.display()
                ),
            ));
        }
        let resolve = |name: &str| {
            [
                grading.join(name),
                directory.join(name),
                directory.join("fixture").join(name),
            ]
            .into_iter()
            .find(|path| path.metadata().is_ok_and(|metadata| metadata.len() > 0))
            .unwrap_or_else(|| directory.join("fixture").join(name))
        };
        let steps = resolve(
            grade_task["grade"]["steps"]
                .as_str()
                .unwrap_or("steps.json"),
        );
        let steps = if steps.metadata().is_ok_and(|metadata| metadata.len() > 0) {
            let mut value = read_json(&steps)?;
            expand_run(&mut value, key);
            value.as_array().context("steps are an array")?.clone()
        } else {
            Vec::new()
        };
        let brief = resolve(
            grade_task["grade"]["brief"]
                .as_str()
                .unwrap_or("SCENARIO.md"),
        );
        let recorded = read_json(&directory.join("run.json")).unwrap_or_else(|_| json!({}));
        let overlay = text(&task["overlay_root"]).to_owned();
        let root = directory.join("worktree").join(&overlay);
        let replay = args.replay.is_some();
        let dry = args.placement.is_some() || args.contract.is_some();
        let results = directory.join("grade").join(if dry {
            "placement-results.json"
        } else if replay {
            "replay-results.json"
        } else {
            "results.json"
        });
        let checklist = directory.join(if replay {
            "checklist-replay.json"
        } else {
            "checklist.json"
        });
        Ok(Self {
            grading: Grading {
                directory,
                root,
                overlay,
                steps,
                results: Vec::new(),
            },
            task,
            recorded,
            brief,
            replay,
            results,
            checklist,
        })
    }
}

/// Return the pilot's existing exit code after writing its grading result.
pub async fn grade(args: GradeArgs) -> u8 {
    match grade_inner(args).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("agent-pilot-grade: {}", error.source);
            error.code
        }
    }
}

async fn grade_inner(args: GradeArgs) -> Result<u8, GradeFailure> {
    let mut context = GradeContext::load(&args)?;
    if args.contract.is_some() {
        if !context.brief.is_file() {
            return Err(failure(
                10,
                format!("no brief at {}", context.brief.display()),
            ));
        }
        let rows = contracts::grade(&context.grading.root, &context.brief)
            .map_err(|source| GradeFailure { code: 30, source })?;
        for row in &rows {
            println!("{}", serde_json::to_string(row)?);
        }
        return Ok(if rows.iter().all(|row| row["ok"] == true) {
            0
        } else {
            30
        });
    }
    if args.placement.is_some() {
        let records =
            read_lines(&context.grading.directory.join("grade/http.jsonl")).unwrap_or_default();
        let rows = context.grading.placement(&records)?;
        for row in &rows {
            println!("{}", serde_json::to_string(row)?);
        }
        write_json(&context.results, &context.grading.results)?;
        return Ok(0);
    }
    let mut held = if context.replay {
        None
    } else {
        Some(live::start(&context).await?)
    };
    let result = grade_started(&mut context).await;
    let stopped = match held.as_mut() {
        Some(held) => held.stop().await,
        None => Ok(()),
    };
    let mut checklist = result?;
    stopped?;
    let removed = if context.replay {
        read_json(&context.grading.directory.join("checklist.json"))
            .ok()
            .is_some_and(|value| value["teardown"]["verification_database_removed"] == true)
    } else {
        live::verification_removed(&context).await
    };
    checklist["teardown"] = json!({"verification_database_removed":removed});
    let passed = checklist["outcome"] == "PASS";
    write_json(&context.checklist, &checklist)?;
    eprintln!("[grade] {}", text(&checklist["outcome"]));
    Ok(if passed { 0 } else { 30 })
}

async fn grade_started(context: &mut GradeContext) -> Result<Value, GradeFailure> {
    let directory = &context.grading.directory;
    let output =
        fs::read_to_string(directory.join("grade/dev.out")).map_err(anyhow::Error::from)?;
    let loop_state = loop_result(&output);
    let mut tracker = String::new();
    if directory.join("worktree").is_dir() {
        let result = Command::new("git")
            .arg("-C")
            .arg(directory.join("worktree"))
            .args([
                "diff",
                "--name-only",
                &format!("{}..HEAD", text(&context.recorded["commit"])),
                "--",
                ".beads",
                ".claude",
                ".codex",
            ])
            .kill_on_drop(true)
            .output()
            .await
            .map_err(anyhow::Error::from)?;
        if result.status.success() {
            tracker = String::from_utf8_lossy(&result.stdout)
                .trim_end_matches('\n')
                .to_owned();
        }
    }
    let outside = context.recorded["git"]
        .get("outside_allowed_paths")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let paths = json!({"pass":outside==json!([]) && tracker.is_empty(),"outside":outside,"tracker":tracker});
    let contract = report_contract(context)?;
    let step_results = if context.replay {
        let records = read_lines(&directory.join("grade/http.jsonl")).unwrap_or_default();
        context.grading.replay(
            &records,
            loop_state["pass"] == true,
            text(&loop_state["base_url"]),
        )?
    } else {
        live::steps(
            context,
            loop_state["pass"] == true,
            text(&loop_state["base_url"]),
        )
        .await?
    };
    write_json(&context.results, &context.grading.results)?;
    let checks = checks(context, &step_results, loop_state["pass"] == true)?;
    let failure_at = |stage: &str| {
        output.lines().find_map(|line| {
            line.split_once(&format!(" at {stage}: "))
                .map(|(kind, message)| format!("{kind}: {message}"))
        })
    };
    let fences = json!({"capability-surface":{"source":"Admit","verdict":failure_at("Admit").unwrap_or_else(||"Admit passed".to_owned())},
        "additive-migration":{"source":"Migrate","verdict":failure_at("Migrate").unwrap_or_else(||"Migrate passed".to_owned())},
        "no-environment-data":{"source":"Admit","verdict":"unfenced"}});
    let mut failures = Vec::new();
    if loop_state["pass"] != true {
        failures.push("loop".to_owned());
    }
    if paths["pass"] != true {
        failures.push("paths".to_owned());
    }
    let missed = step_results
        .iter()
        .filter(|step| step["must"] == true && step["pass"] == false)
        .map(|step| text(&step["id"]))
        .collect::<Vec<_>>();
    if !missed.is_empty() {
        failures.push(format!("steps: {}", missed.join(",")));
    }
    let outcome = if failures.is_empty() {
        "PASS".to_owned()
    } else {
        format!("FAIL (items: {})", failures.join(" "))
    };
    Ok(
        json!({"loop":loop_state,"paths":paths,"contract":contract,"steps":step_results,"checks":checks,"fences":fences,
        "teardown":{"verification_database_removed":false},"outcome":outcome,
        "human":{"Q3":null,"Q7":null,"Q8":null,"Q9":null,"Q10.activated":null,"Q11":null,"E1":null,"E2":null,"E3":null,"E4":null,"E5":null,"E6":null,"E7":null,
        "H-1":null,"H-2":null,"H-3":null,"H-4":null,"H-5":null,"H-6":null,"S-1":null,"S-2":null,"S-3":null}}),
    )
}

fn report_contract(context: &GradeContext) -> anyhow::Result<Value> {
    if !context.brief.is_file() || !context.grading.root.is_dir() {
        return Ok(json!({"state":"not-checked","deviations":0}));
    }
    let rows = match contracts::grade(&context.grading.root, &context.brief) {
        Ok(rows) => rows,
        Err(error) => {
            eprintln!("[grade] {error}");
            write(&context.grading.directory.join("grade/contract.jsonl"), b"")?;
            return Ok(json!({"state":"deviates","deviations":0}));
        }
    };
    let mut bytes = Vec::new();
    for row in &rows {
        serde_json::to_writer(&mut bytes, row)?;
        bytes.push(b'\n');
    }
    write(
        &context.grading.directory.join("grade/contract.jsonl"),
        &bytes,
    )?;
    let misses = rows.iter().filter(|row| row["ok"] == false).count();
    Ok(json!({"state":if misses==0{"holds"}else{"deviates"},"deviations":misses}))
}

fn checks(context: &GradeContext, steps: &[Value], loop_pass: bool) -> anyhow::Result<Value> {
    let replay = steps
        .iter()
        .filter(|step| step["proves"] == "claim-replay")
        .collect::<Vec<_>>();
    let replay = if !loop_pass {
        "not run: the loop served no release"
    } else if replay.is_empty() {
        "no step declares claim-replay"
    } else if replay.iter().all(|step| step["pass"] == true) {
        "pass"
    } else {
        "fail"
    };
    let mut wrong = Vec::new();
    for path in contracts::files(&context.grading.root)? {
        if wrong.len() == 5 {
            break;
        }
        if let Ok(source) = fs::read_to_string(&path) {
            let version = source.match_indices("row_ver").any(|(at, _)| {
                source[at + 7..]
                    .chars()
                    .next()
                    .is_none_or(|next| !next.is_alphanumeric() && next != '_')
            });
            if source.contains("rowversion")
                || source.contains("version_row")
                || source.contains("rowVersion")
                || version
            {
                wrong.push(path.display().to_string());
            }
        }
    }
    let row_version = if wrong.is_empty() {
        "pass".to_owned()
    } else {
        format!("fail: {} ", wrong.join(" "))
    };
    let mut offenders = Vec::new();
    if let Ok(manifest) = read_json(&context.grading.root.join("wamn.json")) {
        let mut objects = Vec::new();
        contracts::visit_objects(&manifest, &mut objects);
        for name in objects.iter().filter_map(|value| value["name"].as_str()) {
            let valid = name
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_lowercase())
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
            let suffix = name.rsplit_once("_v").is_some_and(|(_, suffix)| {
                !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
            });
            if !valid || suffix {
                offenders.push(name.to_owned());
            }
        }
    }
    offenders.sort();
    offenders.dedup();
    Ok(
        json!({"claim-replay":replay,"row_version":row_version,"naming":if offenders.is_empty(){"pass".to_owned()}else{format!("fail: {}",offenders.join(", "))}}),
    )
}
