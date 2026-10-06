use super::Fixture;

#[test]
fn fixture_diagnostic_record_is_bounded_and_owner_conflict_holds_continuation() {
    let f = Fixture::new();
    f.correct_storage();
    let record = "fixture process_probe stage=proc_cwd pid=1292 uid=1001 state=S errno=13 code=- comm=Runner.Worker starttime=123456 Uid=1001,1001,1001,1001 Gid=1001,1001,1001,1001 TracerPid=0 NoNewPrivs=0 Seccomp=2 exe=Runner.Worker stat_errno=0 comm_errno=0 status_errno=0 exe_errno=13 stat_after_errno=0 identity_errno=0\n";
    assert!(record.len() < 1024);
    std::fs::write(f.dir.path().join("process-probe"), record).unwrap();
    let transported = f.process_diagnostic();
    assert_eq!(transported, record);
    assert!(transported.starts_with("fixture process_probe stage="));
    assert!(!transported.contains(&f.task));
    assert!(!transported.contains('/'));
    assert_eq!(transported.lines().count(), 1);

    let _owner = luthor::WorktreeOwner::acquire_existing(&f.config.state_root, &f.task).unwrap();
    let before = f.rows(&["tasks", "attempts", "reservations", "intents"]);
    let held_reasons = f.count("evidence", "held_reason");
    let out = f.run(&f.args());
    assert!(!out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["status"], "held");
    assert_eq!(value["reason"], "launch_failed");
    assert_eq!(
        f.rows(&["tasks", "attempts", "reservations", "intents"]),
        before
    );
    assert_eq!(f.count("evidence", "held_reason"), held_reasons + 1);
    assert_eq!(f.count("evidence", "never_dispatched_authorized"), 0);
    assert_eq!(f.count("intents", "supervisor_dispatch"), 0);
    assert!(!f.marker.exists());
}

#[test]
fn extended_linux_metadata_reaches_fixture_parent_failure_message() {
    let f = Fixture::new();
    let record = "fixture process_probe stage=proc_cwd pid=1292 uid=1001 state=S errno=13 code=- comm=Runner.Worker starttime=123456 Uid=1001,1001,1001,1001 Gid=1001,1001,1001,1001 TracerPid=0 NoNewPrivs=0 Seccomp=2 exe=Runner.Worker stat_errno=0 comm_errno=0 status_errno=0 exe_errno=13 stat_after_errno=0 identity_errno=0\n";
    assert!(record.len() > 160);
    std::fs::write(f.dir.path().join("process-probe"), record).unwrap();
    let transported = f.process_diagnostic();
    assert_eq!(transported, record);
    let panic = std::panic::catch_unwind(|| panic!("inspector unavailable: {transported}"));
    let message = panic.unwrap_err().downcast::<String>().unwrap();
    assert!(message.contains("exe_errno=13 stat_after_errno=0 identity_errno=0"));
    assert!(message.contains(record));
}

#[cfg(target_os = "linux")]
#[test]
fn protected_same_uid_process_does_not_block_saved_continuation() {
    const CHILD_ENV: &str = "LUTHOR_TEST_PROTECTED_PROC_CWD_CHILD_26";
    if std::env::var_os(CHILD_ENV).is_some() {
        protected_child();
        return;
    }
    let mut child = ProtectedChild::start(CHILD_ENV);
    assert_eq!(process_uid(child.pid()), unsafe { libc::getuid() });
    let cwd = format!("/proc/{}/cwd", child.pid());
    assert_eq!(
        std::fs::read_link(cwd).unwrap_err().raw_os_error(),
        Some(libc::EACCES),
        "protected same-UID process must deny reading /proc/<pid>/cwd"
    );
    run_saved_continuation();
    child.stop();
}

#[cfg(target_os = "linux")]
fn protected_child() {
    let result = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) };
    assert_eq!(
        result,
        0,
        "PR_SET_DUMPABLE failed: {}",
        std::io::Error::last_os_error()
    );
    println!("protected-child-ready:{}", std::process::id());
    use std::io::Read;
    let mut byte = [0];
    let _ = std::io::stdin().read(&mut byte);
}

#[cfg(target_os = "linux")]
fn process_uid(pid: u32) -> u32 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    status
        .lines()
        .find_map(|line| {
            line.strip_prefix("Uid:")?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
        .unwrap()
}

#[cfg(target_os = "linux")]
fn run_saved_continuation() {
    let f = Fixture::new();
    f.correct_storage();
    let out = f.run(&f.args());
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let receipt_path = f
        .config
        .state_root
        .join(format!("attempts/{}.receipt.json", f.attempt));
    super::wait_for(&receipt_path);
    let receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&std::fs::read(receipt_path).unwrap()).unwrap();
    assert_eq!(receipt.exit_code, Some(0));
    let marker = std::fs::read_to_string(&f.marker).unwrap();
    let mut expected = format!("{}\n", f.plan.worktree.display());
    expected.push_str(&f.plan.args.join("\n"));
    expected.push('\n');
    assert_eq!(marker, expected);
}

#[cfg(target_os = "linux")]
struct ProtectedChild {
    process: std::process::Child,
}

#[cfg(target_os = "linux")]
impl ProtectedChild {
    fn start(env_name: &str) -> Self {
        use std::io::{BufRead, BufReader};
        let mut process = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "continuation_cli_support::diagnostics::protected_same_uid_process_does_not_block_saved_continuation",
                "--nocapture",
            ])
            .env(env_name, "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .unwrap();
        let mut child = Self { process };
        let pid = child.pid();
        let stdout = child.process.stdout.take().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line).is_err() {
                    return;
                }
            }
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let line = match receiver.recv_timeout(remaining) {
                Ok(Ok(line)) => line,
                Ok(Err(error)) => panic!("failed reading protected child output: {error}"),
                Err(error) => {
                    let _ = child.process.kill();
                    let _ = child.process.wait();
                    panic!("protected child readiness timed out: {error}");
                }
            };
            if line.ends_with(&format!("protected-child-ready:{pid}")) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "protected child did not signal readiness; last output: {line:?}"
            );
        }
        child
    }

    fn pid(&self) -> u32 {
        self.process.id()
    }

    fn stop(&mut self) {
        drop(self.process.stdin.take());
        self.process.wait().unwrap();
    }
}

#[cfg(target_os = "linux")]
impl Drop for ProtectedChild {
    fn drop(&mut self) {
        drop(self.process.stdin.take());
        if self.process.try_wait().unwrap().is_none() {
            let _ = self.process.kill();
            let _ = self.process.wait();
        }
    }
}
