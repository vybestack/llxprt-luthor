//! Deterministic permission/platform failures cannot be induced portably with
//! real processes. Inject only the read-only OS observations, not the journal.
use super::{Probe, require_absent_with};
use crate::model::StateError;
use rusqlite::Connection;
use serde_json::json;

const BOOT: &str = "11111111-2222-3333-4444-555555555555";
const OTHER_BOOT: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const PID: u32 = 42;

struct Observed {
    boot: Option<&'static str>,
    start: Option<&'static str>,
    pid_absent: bool,
    group_absent: bool,
    leader: bool,
}

impl Observed {
    fn live(start: Option<&'static str>) -> Self {
        Self {
            boot: Some(BOOT),
            start,
            pid_absent: false,
            group_absent: false,
            leader: true,
        }
    }
}

impl Probe for Observed {
    fn identity(&self, pid: u32) -> Option<(String, String)> {
        if pid == std::process::id() {
            return self.boot.map(|boot| (boot.into(), "self".into()));
        }
        assert_eq!(pid, PID);
        self.start.map(|start| (BOOT.into(), start.into()))
    }

    fn absent(&self, _: u32, group: bool) -> bool {
        if group {
            self.group_absent
        } else {
            self.pid_absent
        }
    }

    fn group_leader(&self, _: u32) -> bool {
        self.leader
    }
}

fn terminal(kind: &str) -> Connection {
    assert_ne!(PID, std::process::id());
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE tasks(id TEXT,state TEXT); INSERT INTO tasks VALUES('task','pr_merged');
        CREATE TABLE evidence(sequence INTEGER PRIMARY KEY,task_id TEXT,attempt_id TEXT,kind TEXT,payload TEXT);").unwrap();
    db.execute("INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task','old-attempt',?1,?2)",
        rusqlite::params![kind, json!({"pid":PID,"group_id":PID,"boot_identity":BOOT,"start_identity":"registered-start"}).to_string()]).unwrap();
    db
}

#[test]
fn terminal_unreadable_start_identity_blocks_every_registration_kind() {
    for kind in ["child_registered", "supervisor_ready", "tracked_descendant"] {
        let db = terminal(kind);
        for group_absent in [false, true] {
            let mut observed = Observed::live(None);
            observed.group_absent = group_absent;
            assert!(matches!(
                require_absent_with(&db, "task", None, &observed),
                Err(StateError::LaunchBlocked)
            ));
        }
    }
}

#[test]
fn matching_identity_blocks_but_reused_leader_proves_a_recycled_group() {
    for kind in ["child_registered", "supervisor_ready", "tracked_descendant"] {
        let db = terminal(kind);
        assert!(matches!(
            require_absent_with(&db, "task", None, &Observed::live(Some("registered-start"))),
            Err(StateError::LaunchBlocked)
        ));
        require_absent_with(&db, "task", None, &Observed::live(Some("new-start"))).unwrap();
    }
}

#[test]
fn surviving_members_block_without_proof_of_a_recycled_group_leader() {
    let db = terminal("child_registered");
    for start in [None, Some("new-start")] {
        let mut observed = Observed::live(start);
        observed.leader = false;
        observed.pid_absent = start.is_none();
        assert!(matches!(
            require_absent_with(&db, "task", None, &observed),
            Err(StateError::LaunchBlocked)
        ));
        observed.group_absent = true;
        require_absent_with(&db, "task", None, &observed).unwrap();
    }
    db.execute(
        "UPDATE evidence SET payload=json_set(payload,'$.group_id',43)",
        [],
    )
    .unwrap();
    assert!(matches!(
        require_absent_with(&db, "task", None, &Observed::live(Some("new-start"))),
        Err(StateError::LaunchBlocked)
    ));
}

#[test]
fn previous_boot_proves_all_members_gone_but_unreadable_boot_does_not() {
    let db = terminal("child_registered");
    let mut observed = Observed::live(None);
    observed.boot = Some(OTHER_BOOT);
    require_absent_with(&db, "task", None, &observed).unwrap();
    observed.boot = None;
    assert!(matches!(
        require_absent_with(&db, "task", None, &observed),
        Err(StateError::LaunchBlocked)
    ));
    observed.pid_absent = true;
    observed.group_absent = true;
    require_absent_with(&db, "task", None, &observed).unwrap();
}

#[test]
fn historical_or_malformed_boots_cannot_prove_reboot() {
    for boot in [
        "boot",
        "{ sec = 123, usec = 456 }",
        "00000000-0000-0000-0000-000000000000",
    ] {
        let db = terminal("child_registered");
        db.execute(
            "UPDATE evidence SET payload=json_set(payload,'$.boot_identity',?1)",
            [boot],
        )
        .unwrap();
        let observed = Observed::live(Some("registered-start"));
        assert!(matches!(
            require_absent_with(&db, "task", None, &observed),
            Err(StateError::LaunchBlocked)
        ));
    }
}

#[test]
fn only_exact_counted_attempt_is_excluded_not_other_or_task_scoped_evidence() {
    let db = terminal("supervisor_ready");
    let observed = Observed::live(Some("registered-start"));
    require_absent_with(&db, "task", Some("old-attempt"), &observed).unwrap();
    assert!(matches!(
        require_absent_with(&db, "task", Some("running-attempt"), &observed),
        Err(StateError::LaunchBlocked)
    ));
    db.execute("UPDATE evidence SET attempt_id=NULL", [])
        .unwrap();
    assert!(matches!(
        require_absent_with(&db, "task", Some("running-attempt"), &observed),
        Err(StateError::LaunchBlocked)
    ));
}

#[test]
fn invalid_identifiers_and_payloads_are_not_absence_proofs() {
    for field in ["pid", "group_id"] {
        for value in [0_u64, u32::MAX as u64] {
            let db = terminal("child_registered");
            db.execute(
                &format!("UPDATE evidence SET payload=json_set(payload,'$.{field}',?1)"),
                [value],
            )
            .unwrap();
            assert!(matches!(
                require_absent_with(&db, "task", None, &Observed::live(Some("new-start"))),
                Err(StateError::LaunchBlocked)
            ));
        }
    }
    let db = terminal("child_registered");
    db.execute("UPDATE evidence SET payload='malformed'", [])
        .unwrap();
    assert!(matches!(
        require_absent_with(&db, "task", None, &Observed::live(Some("new-start"))),
        Err(StateError::Serialization(_))
    ));
}

#[test]
fn boot_uuid_letter_case_is_not_a_different_boot() {
    let db = terminal("child_registered");
    // The OS writes normalized UUIDs, but case alone must never prove a reboot.
    let mut observed = Observed::live(None);
    observed.boot = Some("AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE");
    db.execute(
        "UPDATE evidence SET payload=json_set(payload,'$.boot_identity',?1)",
        [OTHER_BOOT],
    )
    .unwrap();
    // No readable process start: only the self boot observation is available.
    assert!(matches!(
        require_absent_with(&db, "task", None, &observed),
        Err(StateError::LaunchBlocked)
    ));
}

#[test]
fn changed_start_identity_proves_reuse_even_for_historical_boot_encoding() {
    let db = terminal("child_registered");
    db.execute("UPDATE evidence SET payload=json_set(payload,'$.boot_identity','{ sec = 123, usec = 456 }')", []).unwrap();
    require_absent_with(&db, "task", None, &Observed::live(Some("new-start"))).unwrap();
}
