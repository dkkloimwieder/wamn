//! The run record of an environment upgrade and its resume rule
//! (docs/plan/upgrade-environment.md §2 and §4.2 stage 12).
//!
//! The record names every step of a run with its start and end in UTC, its
//! inputs and its result. It holds no credential: a caller never passes one as
//! an input. A second run with the same arguments reads the record and
//! resumes at the first stage that has no finished step.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context as _, bail, ensure};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

/// The stages of §4.2, in the order that the verb runs them.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Source,
    Build,
    Images,
    Guests,
    Preflight,
    Schema,
    Packages,
    Qualify,
    PublishAndSelect,
    Deploy,
    CheckAndRetire,
    Record,
}

impl Stage {
    pub const ALL: [Self; 12] = [
        Self::Source,
        Self::Build,
        Self::Images,
        Self::Guests,
        Self::Preflight,
        Self::Schema,
        Self::Packages,
        Self::Qualify,
        Self::PublishAndSelect,
        Self::Deploy,
        Self::CheckAndRetire,
        Self::Record,
    ];
}

/// The arguments of one run: one environment and one commit.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunArguments {
    pub org: String,
    pub project: String,
    pub environment: String,
    /// A full 40-hex commit. A branch name, a tag or a short hash is refused.
    pub commit: String,
}

impl RunArguments {
    pub fn new(org: &str, project: &str, environment: &str, commit: &str) -> anyhow::Result<Self> {
        ensure!(
            commit.len() == 40
                && commit
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "the commit {commit} is not a full 40-hex commit"
        );
        Ok(Self {
            org: org.to_owned(),
            project: project.to_owned(),
            environment: environment.to_owned(),
            commit: commit.to_owned(),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "result")]
pub enum StepResult {
    /// The step changed the state to the target.
    Done,
    /// The state was already the target, so the step did nothing.
    AlreadyDone,
    Failed {
        cause: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub stage: Stage,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub inputs: BTreeMap<String, String>,
    /// What the step made or read that a later stage uses, such as a pinned
    /// image reference.
    #[serde(default)]
    pub outputs: BTreeMap<String, String>,
    pub result: Option<StepResult>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunRecord {
    pub arguments: RunArguments,
    pub steps: Vec<Step>,
}

/// The step of a run that ended before the step finished.
const INTERRUPTED: &str = "the run ended before the step finished";

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

impl RunRecord {
    /// Opens the record at `path`, or starts a new one. A record of other
    /// arguments is refused. A step that a stopped run left open is closed
    /// as failed, so that its stage runs again.
    pub fn open(path: &Path, arguments: RunArguments) -> anyhow::Result<Self> {
        let mut record = match fs::read(path) {
            Ok(bytes) => {
                let record: Self = serde_json::from_slice(&bytes)
                    .with_context(|| format!("read the run record {}", path.display()))?;
                ensure!(
                    record.arguments == arguments,
                    "the run record {} belongs to other arguments: {:?}",
                    path.display(),
                    record.arguments
                );
                record
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self {
                arguments,
                steps: Vec::new(),
            },
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read the run record {}", path.display()));
            }
        };
        if let Some(step) = record.steps.last_mut().filter(|step| step.result.is_none()) {
            step.ended_at = Some(now());
            step.result = Some(StepResult::Failed {
                cause: INTERRUPTED.to_owned(),
            });
        }
        record.save(path)?;
        Ok(record)
    }

    /// The first stage that has no finished step, or none when the run is complete.
    pub fn next_stage(&self) -> Option<Stage> {
        Stage::ALL.into_iter().find(|stage| {
            !self.steps.iter().any(|step| {
                step.stage == *stage
                    && matches!(
                        step.result,
                        Some(StepResult::Done | StepResult::AlreadyDone)
                    )
            })
        })
    }

    /// Records the start of `stage`, which must be the next stage.
    pub fn start(
        &mut self,
        path: &Path,
        stage: Stage,
        inputs: BTreeMap<String, String>,
    ) -> anyhow::Result<()> {
        ensure!(
            self.next_stage() == Some(stage),
            "the stage {stage:?} is not the next stage {:?}",
            self.next_stage()
        );
        self.steps.push(Step {
            stage,
            started_at: now(),
            ended_at: None,
            inputs,
            outputs: BTreeMap::new(),
            result: None,
        });
        self.save(path)
    }

    /// Records the result and the outputs of the open step.
    pub fn finish(
        &mut self,
        path: &Path,
        result: StepResult,
        outputs: BTreeMap<String, String>,
    ) -> anyhow::Result<()> {
        let Some(step) = self.steps.last_mut().filter(|step| step.result.is_none()) else {
            bail!("the run record has no open step");
        };
        step.ended_at = Some(now());
        step.outputs = outputs;
        step.result = Some(result);
        self.save(path)
    }

    /// An output of the finished step of `stage`.
    pub fn output(&self, stage: Stage, name: &str) -> anyhow::Result<&str> {
        self.steps
            .iter()
            .rev()
            .find(|step| {
                step.stage == stage
                    && matches!(
                        step.result,
                        Some(StepResult::Done | StepResult::AlreadyDone)
                    )
            })
            .and_then(|step| step.outputs.get(name))
            .map(String::as_str)
            .with_context(|| format!("the stage {stage:?} recorded no output {name}"))
    }

    /// Writes the whole record to a new file and renames it over the old one,
    /// so that a stopped run leaves a complete record.
    fn save(&self, path: &Path) -> anyhow::Result<()> {
        let partial = path.with_extension("json.partial");
        fs::write(&partial, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("write {}", partial.display()))?;
        fs::rename(&partial, path)
            .with_context(|| format!("write the run record {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "0c4bf44eb0000000000000000000000000000000";

    fn arguments() -> RunArguments {
        RunArguments::new("dkk", "receiving", "dev", COMMIT).expect("arguments")
    }

    fn directory() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "wamn-run-record-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().expect("time")
        ));
        fs::create_dir(&path).expect("create test directory");
        path
    }

    #[test]
    fn only_a_full_commit_is_accepted() {
        for commit in ["main", "v1.0.0", "0c4bf44eb", &COMMIT.to_uppercase()] {
            assert!(
                RunArguments::new("dkk", "receiving", "dev", commit).is_err(),
                "{commit}"
            );
        }
    }

    #[test]
    fn a_second_run_resumes_at_the_first_unfinished_stage() {
        let directory = directory();
        let path = directory.join("run.json");
        let mut record = RunRecord::open(&path, arguments()).expect("new record");
        assert_eq!(record.next_stage(), Some(Stage::Source));
        let inputs = BTreeMap::from([("commit".to_owned(), COMMIT.to_owned())]);
        record.start(&path, Stage::Source, inputs).expect("start");
        record
            .finish(&path, StepResult::Done, BTreeMap::new())
            .expect("finish");
        record
            .start(&path, Stage::Build, BTreeMap::new())
            .expect("start");
        record
            .finish(&path, StepResult::AlreadyDone, BTreeMap::new())
            .expect("finish");
        record
            .start(&path, Stage::Images, BTreeMap::new())
            .expect("start");
        record
            .finish(
                &path,
                StepResult::Failed {
                    cause: "push refused".to_owned(),
                },
                BTreeMap::new(),
            )
            .expect("finish");
        record
            .start(&path, Stage::Images, BTreeMap::new())
            .expect("retry");
        assert!(
            record.start(&path, Stage::Guests, BTreeMap::new()).is_err(),
            "a stage starts only after the stages before it finish"
        );
        drop(record);

        // The first run stopped with the retry of Images open.
        let resumed = RunRecord::open(&path, arguments()).expect("resume");
        assert_eq!(resumed.next_stage(), Some(Stage::Images));
        assert_eq!(resumed.steps.len(), 4);
        assert_eq!(
            resumed.steps[3].result,
            Some(StepResult::Failed {
                cause: INTERRUPTED.to_owned()
            })
        );
        assert!(resumed.steps.iter().all(|step| step.ended_at.is_some()));

        let other = RunArguments::new("dkk", "wms", "dev", COMMIT).expect("arguments");
        let error = RunRecord::open(&path, other).expect_err("other arguments");
        assert!(
            format!("{error:#}").contains("belongs to other arguments"),
            "{error:#}"
        );
        fs::remove_dir_all(&directory).expect("remove test directory");
    }

    #[test]
    fn a_run_with_every_stage_finished_has_no_next_stage() {
        let directory = directory();
        let path = directory.join("run.json");
        let mut record = RunRecord::open(&path, arguments()).expect("new record");
        for stage in Stage::ALL {
            record.start(&path, stage, BTreeMap::new()).expect("start");
            record
                .finish(&path, StepResult::Done, BTreeMap::new())
                .expect("finish");
        }
        assert_eq!(record.next_stage(), None);
        fs::remove_dir_all(&directory).expect("remove test directory");
    }
}
