mod arguments;
mod refusals;
mod success;

use super::{Fixture, wait_for};
use luthor::{config::Config, state::StateStore};
use serde_json::{Value, json};
use std::{
    fs,
    process::{Command, Output},
    thread,
    time::Duration,
};

fn fresh_os_run(mut run: impl FnMut(&AmendmentFixture) -> Output) -> (AmendmentFixture, Output) {
    for retry in 0..=20 {
        let case = AmendmentFixture::new();
        let out = run(&case);
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        if value["reason"] != "process_unavailable" {
            return (case, out);
        }
        assert!(!out.status.success());
        assert_eq!(value["status"], "held");
        assert_eq!(value["command"], "amend-undispatched");
        assert_eq!(case.f.count("intents", "supervisor_dispatch"), 0);
        assert!(!case.f.marker.exists());
        let audits = case.f.count("evidence", "initial_branch_removed");
        assert!(audits == 0 || audits == 1);
        case.assert_reserved_original();
        if audits == 1 {
            success::assert_replay(&case);
        }
        assert!(
            retry < 20,
            "real OS inspector remained unavailable in fresh fixtures"
        );
        thread::sleep(Duration::from_millis(50));
    }
    unreachable!()
}

struct AmendmentFixture {
    f: Fixture,
    corrected: Config,
}

impl AmendmentFixture {
    fn new() -> Self {
        let f = Fixture::new_with(true);
        let mut corrected = f.config.clone();
        corrected.initial.args.drain(2..4);
        fs::write(&f.config_path, serde_json::to_vec(&corrected).unwrap()).unwrap();
        f.correct_storage();
        Self { f, corrected }
    }

    fn args(&self) -> Vec<String> {
        let mut args = self.f.args();
        args[7] = "corrected-revision".into();
        args[11] = "native_initial_branch_is_conversation".into();
        args
    }

    fn command(&self, args: &[String]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_luthor"));
        command
            .arg("amend-undispatched")
            .args(args)
            .env("PATH", &self.f.path);
        command
    }

    fn run(&self, args: &[String]) -> Output {
        self.command(args).output().unwrap()
    }

    fn settled(&self) -> Output {
        for retry in 0..=20 {
            let out = self.run(&self.args());
            let value: Value = serde_json::from_slice(&out.stdout).unwrap();
            if value["reason"] != "process_unavailable" {
                return out;
            }
            assert!(!out.status.success());
            assert_eq!(
                self.f.count("evidence", "initial_branch_removed"),
                0,
                "never retry an authorization already consumed by a post-audit refusal"
            );
            assert_eq!(self.f.count("intents", "supervisor_dispatch"), 0);
            assert!(!self.f.marker.exists());
            assert!(retry < 20, "real OS inspector remained unavailable");
            thread::sleep(Duration::from_millis(50));
        }
        unreachable!()
    }

    fn held(&self, reason: &str) -> Value {
        let out = self.settled();
        assert!(!out.status.success());
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "luthor: amendment refused or held\n"
        );
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(
            value,
            json!({"command":"amend-undispatched", "task_id":self.f.task,
            "attempt_id":self.f.attempt,"status":"held","reason":reason})
        );
        value
    }

    fn assert_reserved_original(&self) {
        let store = StateStore::open(&self.f.config.state_root, 1).unwrap();
        assert_eq!(store.reservation_count().unwrap(), 1);
        assert_eq!(
            store.latest_attempt(&self.f.task).unwrap().as_deref(),
            Some(self.f.attempt.as_str())
        );
        assert_eq!(
            store.launch_intent(&self.f.attempt).unwrap().as_deref(),
            Some(self.f.saved.as_str())
        );
        for table in ["tasks", "attempts", "reservations"] {
            assert_eq!(
                self.f
                    .db()
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                1
            );
        }
        assert_eq!(self.f.count("evidence", "never_dispatched_authorized"), 0);
    }
}
