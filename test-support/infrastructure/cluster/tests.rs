use super::*;
use std::os::unix::process::ExitStatusExt as _;
use std::sync::Mutex;

struct Fake {
    scenario: &'static str,
    calls: Mutex<Vec<Vec<String>>>,
    sleeps: Mutex<Vec<u64>>,
    attempts: Mutex<usize>,
    sidecar_failure: bool,
}

impl Fake {
    fn new(scenario: &'static str) -> Self {
        Self {
            scenario,
            calls: Mutex::default(),
            sleeps: Mutex::default(),
            attempts: Mutex::new(0),
            sidecar_failure: false,
        }
    }
}

#[async_trait]
impl Runner for Fake {
    async fn output(
        &self,
        command: &mut Command,
        _: Option<Input>,
        bound: Duration,
    ) -> anyhow::Result<Output> {
        let standard = command.as_std();
        let args = std::iter::once(standard.get_program())
            .chain(standard.get_args())
            .map(|s| s.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        self.calls.lock().unwrap().push(args.clone());
        let call = args.join(" ");
        let mut code = 0;
        let mut stdout = String::new();
        if call.starts_with("kind delete") {
            assert_eq!(bound, Duration::from_secs(120));
            let mut attempts = self.attempts.lock().unwrap();
            *attempts += 1;
            code = i32::from(
                self.scenario != "immediate" && !(self.scenario == "retry" && *attempts > 1),
            );
        } else if call.starts_with("docker info") {
        } else if call == "kind get clusters" {
            assert_eq!(bound, Duration::from_secs(5));
            match self.scenario {
                "leftovers" => stdout = "wamn-test\n".into(),
                "inspection" => code = 1,
                _ => {}
            }
        } else if call.starts_with("docker ps -a") {
            assert!(call.contains("label=io.x-k8s.kind.cluster=wamn-test"));
            match self.scenario {
                "leftovers" | "container-only" => stdout = "wamn-test-control-plane\n".into(),
                "inspection" => code = 1,
                _ => {}
            }
        } else if call.starts_with("docker container ls") {
            stdout = "wamn-test-postgres\nwamn-test-nats\nwamn-test-registry\nwamn-test-minio\nwamn-test-demo-proxy\nunrelated\n".into();
        } else if call.starts_with("docker rm") {
            code = i32::from(self.sidecar_failure);
        } else if call.starts_with("docker image ls") {
            stdout = "example:wamn-test-receiving\nexample:wamn-test-wms\nexample:other\n".into();
        } else if !call.starts_with("docker volume") && !call.starts_with("docker image rm") {
            panic!("unexpected fixture command: {call}");
        }
        Ok(Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.into_bytes(),
            stderr: Vec::new(),
        })
    }
    async fn pause(&self, duration: Duration) {
        self.sleeps.lock().unwrap().push(duration.as_secs());
    }
}

async fn deletion(scenario: &'static str, attempts: usize, leftovers: &[&str], failed: bool) {
    let fake = Fake::new(scenario);
    let result = delete(&fake, "wamn-test", Path::new("/private")).await;
    assert_eq!(*fake.attempts.lock().unwrap(), attempts);
    assert_eq!(result.leftovers, leftovers);
    assert_eq!(result.result().is_err(), failed);
    assert_eq!(
        *fake.sleeps.lock().unwrap(),
        match attempts {
            1 => vec![],
            2 => vec![2],
            _ => vec![2, 4],
        }
    );
    assert!(
        fake.calls.lock().unwrap()[0]
            .ends_with(&["--kubeconfig".into(), "/private/kubeconfig".into()])
    );
}

#[tokio::test]
async fn immediate_delete() {
    deletion("immediate", 1, &[], false).await;
}
#[tokio::test]
async fn retry_delete() {
    deletion("retry", 2, &[], false).await;
}
#[tokio::test]
async fn leftovers_are_reported() {
    deletion(
        "leftovers",
        3,
        &["wamn-test", "wamn-test-control-plane"],
        false,
    )
    .await;
}
#[tokio::test]
async fn already_gone_after_failed_deletes() {
    deletion("gone", 3, &[], false).await;
}
#[tokio::test]
async fn container_only_leftovers() {
    deletion("container-only", 3, &["wamn-test-control-plane"], false).await;
}
#[tokio::test]
async fn inspection_failure_is_fatal() {
    deletion("inspection", 3, &[], true).await;
}

#[tokio::test]
async fn sidecars_and_kept_resources_are_removed_despite_leftovers() {
    for scenario in ["immediate", "leftovers"] {
        let scratch = crate::scratch::ScratchRoot::create().unwrap();
        let fake = Fake::new(scenario);
        let result = cleanup(&fake, "wamn-test", scratch.path()).await;
        result.result().unwrap();
        assert_eq!(
            fs::read_to_string(scratch.path().join("kind-teardown-leftovers")).unwrap(),
            result.leftovers.join("\n")
        );
        let calls = fake
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.join(" "))
            .collect::<Vec<_>>();
        for suffix in ["postgres", "nats", "registry", "minio", "demo-proxy"] {
            assert!(calls.contains(&format!("docker rm --force --volumes wamn-test-{suffix}")));
        }
        assert!(calls.contains(&"docker volume rm wamn-test-registry-data".into()));
        for app in ["receiving", "wms"] {
            assert!(calls.contains(&format!("docker image rm example:wamn-test-{app}")));
        }
        assert!(
            !calls
                .iter()
                .any(|c| c.contains("rm unrelated") || c.contains("rm example:other"))
        );
    }
}

#[tokio::test]
async fn sidecar_failure_is_fatal_and_does_not_skip_other_cleanup() {
    let scratch = crate::scratch::ScratchRoot::create().unwrap();
    let mut fake = Fake::new("leftovers");
    fake.sidecar_failure = true;
    let result = cleanup(&fake, "wamn-test", scratch.path()).await;
    assert!(result.result().is_err());
    assert_eq!(result.failures.len(), 5);
    assert!(
        fake.calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.join(" ") == "docker image rm example:wamn-test-wms")
    );
}

#[tokio::test]
async fn private_directory_is_removed_after_leftover_only_cleanup() {
    let scratch = crate::scratch::ScratchRoot::create().unwrap();
    let path = scratch.path().to_owned();
    fs::write(path.join("private-key"), "secret").unwrap();
    cleanup(&Fake::new("leftovers"), "wamn-test", &path)
        .await
        .result()
        .unwrap();
    drop(scratch);
    assert!(!path.exists());
}

#[tokio::test]
async fn frozen_cluster_is_refused_before_any_command() {
    let fake = Fake::new("immediate");
    assert!(
        delete(&fake, "wamn", Path::new("/private"))
            .await
            .result()
            .is_err()
    );
    assert!(fake.calls.lock().unwrap().is_empty());
    assert!(Cluster::new("wamn", Path::new("/private")).is_err());
}

#[test]
fn pinned_reference_removes_only_the_image_tag() {
    assert_eq!(
        digest_reference("localhost:5000/host:dev@sha256:abc").unwrap(),
        "localhost:5000/host@sha256:abc"
    );
    assert_eq!(
        digest_reference("localhost:5000/host@sha256:abc").unwrap(),
        "localhost:5000/host@sha256:abc"
    );
    assert!(digest_reference("host:dev").is_err());
}

type ImageCall = (Vec<String>, Option<Vec<u8>>, Duration);

struct ImageRunner {
    calls: Mutex<Vec<ImageCall>>,
    inspections: Mutex<usize>,
    never_ready: bool,
}

impl ImageRunner {
    fn new(never_ready: bool) -> Self {
        Self {
            calls: Mutex::default(),
            inspections: Mutex::new(0),
            never_ready,
        }
    }
}

#[async_trait]
impl Runner for ImageRunner {
    async fn output(
        &self,
        command: &mut Command,
        input: Option<Input>,
        bound: Duration,
    ) -> anyhow::Result<Output> {
        let command = command.as_std();
        let args = std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|s| s.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let call = args.join(" ");
        let input = input.map(|input| match input {
            Input::Bytes(bytes) => bytes,
            Input::File(path) => fs::read(path).unwrap(),
        });
        self.calls.lock().unwrap().push((args, input, bound));
        let mut code = 0;
        let stdout = if call == "kind get nodes --name wamn-test" {
            "node-a\nnode-b\n"
        } else if call.contains("crictl inspecti") {
            let mut count = self.inspections.lock().unwrap();
            *count += 1;
            code = i32::from(self.never_ready || *count == 1);
            "image identity"
        } else {
            "diagnostic images"
        };
        Ok(Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    }
    async fn pause(&self, duration: Duration) {
        assert_eq!(duration, Duration::from_secs(1));
    }
}

#[tokio::test]
async fn candidate_archive_imports_exact_bytes_on_every_node_without_credentials() {
    let root = crate::scratch::ScratchRoot::create().unwrap();
    let archive = root.path().join("node-image-abc.tar");
    fs::write(&archive, b"pinned image bytes").unwrap();
    let runner = ImageRunner::new(false);
    import_images(
        &runner,
        "wamn-test",
        root.path(),
        &["private.example/host@sha256:abc"],
    )
    .await
    .unwrap();
    assert!(!archive.exists());
    let calls = runner.calls.lock().unwrap();
    let imports = calls
        .iter()
        .filter(|(c, _, _)| c.iter().any(|a| a == "import"))
        .collect::<Vec<_>>();
    assert_eq!(imports.len(), 2);
    for (args, input, bound) in imports {
        assert!(args.ends_with(&["--platform".into(), "linux/amd64".into(), "-".into()]));
        assert_eq!(input.as_deref(), Some(&b"pinned image bytes"[..]));
        assert_eq!(*bound, Duration::from_secs(180));
        assert!(
            !args
                .iter()
                .any(|arg| arg.contains("password") || arg.contains("auth"))
        );
    }
    assert_eq!(*runner.inspections.lock().unwrap(), 3);
    assert!(root.path().join("node-image-waits.log").exists());
}

#[tokio::test]
async fn candidate_loopback_pull_uses_hosts_directory_and_digest_without_tag() {
    let root = crate::scratch::ScratchRoot::create().unwrap();
    fs::write(
        root.path().join("native-registry-authority"),
        "localhost:5000",
    )
    .unwrap();
    fs::write(
        root.path().join("native-registry-hosts.toml"),
        "public transport",
    )
    .unwrap();
    let runner = ImageRunner::new(false);
    import_images(
        &runner,
        "wamn-test",
        root.path(),
        &["localhost:5000/host:dev@sha256:abc"],
    )
    .await
    .unwrap();
    let calls = runner.calls.lock().unwrap();
    for node in ["node-a", "node-b"] {
        assert!(
            calls
                .iter()
                .any(|(args, _, _)| args.join(" ").contains(&format!(
                    "{node}:/etc/containerd/certs.d/localhost:5000/hosts.toml"
                )))
        );
        let (args, input, bound) = calls
            .iter()
            .find(|(args, _, _)| {
                args.join(" ")
                    .starts_with(&format!("docker exec {node} ctr"))
            })
            .unwrap();
        assert!(args.ends_with(&[
            "linux/amd64".into(),
            "localhost:5000/host@sha256:abc".into()
        ]));
        assert!(args.iter().any(|arg| arg == "--hosts-dir"));
        assert!(input.is_none());
        assert_eq!(*bound, Duration::from_secs(180));
    }
    assert!(
        !calls
            .iter()
            .any(|(args, _, _)| args.iter().any(|arg| arg == "import"))
    );
}

#[tokio::test]
async fn missing_candidate_image_records_both_node_views_after_bounded_retry() {
    let root = crate::scratch::ScratchRoot::create().unwrap();
    fs::write(root.path().join("node-image-abc.tar"), b"pinned bytes").unwrap();
    let runner = ImageRunner::new(true);
    assert!(
        import_images(
            &runner,
            "wamn-test",
            root.path(),
            &["private.example/host@sha256:abc"]
        )
        .await
        .is_err()
    );
    assert_eq!(*runner.inspections.lock().unwrap(), 10);
    for suffix in ["cri-images.json", "containerd-images.txt"] {
        assert_eq!(
            fs::read(root.path().join(format!("failure-node-a-{suffix}"))).unwrap(),
            b"diagnostic images"
        );
    }
}

#[tokio::test]
async fn preexisting_cluster_is_never_claimed_or_deleted() {
    let root = crate::scratch::ScratchRoot::create().unwrap();
    let fake = Arc::new(Fake::new("leftovers"));
    let mut fixture = Cluster::new("wamn-test", root.path()).unwrap();
    fixture.runner = Arc::<Fake>::clone(&fake);
    assert!(fixture.create(root.path()).await.is_err());
    assert!(!fixture.owned());
    assert_eq!(fixture.cleanup_blocking(), Teardown::default());
    drop(fixture);
    assert!(
        !fake
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|args| args.join(" ").starts_with("kind delete"))
    );
}

#[test]
fn failed_final_cleanup_is_not_repeated_when_the_owner_drops() {
    let root = crate::scratch::ScratchRoot::create().unwrap();
    let fake = Arc::new(Fake::new("inspection"));
    let mut fixture = Cluster::new("wamn-test", root.path()).unwrap();
    fixture.runner = Arc::<Fake>::clone(&fake);
    fixture.owned = true;
    assert!(fixture.cleanup_blocking().result().is_err());
    drop(fixture);
    assert_eq!(*fake.attempts.lock().unwrap(), 3);
}
