use super::support::Fixture;
use serde_json::{Value, json};

fn finish_attempt(f: &Fixture, id: &str, exit_code: i32) {
    f.db()
        .execute(
            "UPDATE attempts SET lifecycle='completed',outcome=?1 WHERE id=?2",
            [
                format!("exit_code=Some({exit_code});signal=None"),
                id.to_owned(),
            ],
        )
        .unwrap();
    f.db()
        .execute(
            "UPDATE reservations SET status='released' WHERE attempt_id=?1",
            [id],
        )
        .unwrap();
}

#[test]
fn newest_attempt_without_launch_retains_previous_session_but_not_previous_outcome() {
    let f = Fixture::new();
    f.task("task");
    f.attempt("task", "first", "first-session");
    finish_attempt(&f, "first", 17);
    f.attempt("task", "second", "second-session");
    finish_attempt(&f, "second", 17);
    // Reservation can precede the launch intent; reporting must not invent one.
    f.db()
        .execute(
            "INSERT INTO attempts(id,task_id,lifecycle) VALUES('third','task','launch_intended')",
            [],
        )
        .unwrap();
    f.db()
        .execute(
            "INSERT INTO reservations(attempt_id,task_id,status) VALUES('third','task','reserved')",
            [],
        )
        .unwrap();
    f.db()
        .execute("UPDATE attempts SET created_at='2000-01-01 00:00:00'", [])
        .unwrap();
    f.logs("first");
    let (stdout, _) = f.logs("third");
    std::fs::write(stdout, "current-output").unwrap();

    let shown = f.run(&["show", "task"]).unwrap();
    let ids: Vec<_> = shown["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].clone())
        .collect();
    assert_eq!(ids, [json!("first"), json!("second"), json!("third")]);
    assert_eq!(shown["session"], "second-session");
    assert_eq!(shown["latest_attempt_id"], "third");
    assert_eq!(shown["latest_attempt_outcome"], Value::Null);
    assert_eq!(
        shown["latest_attempt_outcome_unavailable_reason"],
        "attempt has no verified exit outcome"
    );
    assert_eq!(shown["reserved_slot"], true);
    assert_eq!(shown["observed_stdout_bytes"], 14);
    assert_eq!(shown["observed_stderr_bytes"], 7);
    assert_eq!(
        shown["attempts"][0]["outcome"],
        "exit_code=Some(17);signal=None"
    );
    assert_eq!(shown["attempts"][2]["reservation"], "reserved");
    assert_eq!(
        f.db()
            .query_row("SELECT COUNT(*) FROM intents", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert!(!shown.to_string().contains("secret-prompt"));
}

#[test]
fn attempt_sessions_outcomes_and_output_remain_task_and_reservation_scoped() {
    let f = Fixture::new();
    f.task("held");
    f.task("finished");
    f.task("unlaunched");
    f.attempt("held", "held-attempt", "held-session");
    f.attempt("finished", "finished-attempt", "finished-session");
    finish_attempt(&f, "finished-attempt", 0);
    f.logs("held-attempt");
    f.logs("finished-attempt");

    let held = f.run(&["show", "held"]).unwrap();
    assert_eq!(held["session"], "held-session");
    assert_eq!(held["latest_attempt_id"], "held-attempt");
    assert_eq!(held["latest_attempt_outcome"], Value::Null);
    assert_eq!(held["reserved_slot"], true);
    assert_eq!(held["observed_stdout_bytes"], 5);
    let finished = f.run(&["show", "finished"]).unwrap();
    assert_eq!(finished["session"], "finished-session");
    assert_eq!(finished["latest_attempt_id"], "finished-attempt");
    assert_eq!(
        finished["latest_attempt_outcome"],
        "exit_code=Some(0);signal=None"
    );
    assert_eq!(
        finished["latest_attempt_outcome_unavailable_reason"],
        Value::Null
    );
    assert_eq!(finished["reserved_slot"], false);
    assert_eq!(
        finished["output_age_unavailable_reason"],
        "no reserved attempt"
    );
    assert_eq!(finished["observed_stdout_bytes"], Value::Null);
    assert_eq!(finished["observed_stderr_bytes"], Value::Null);
    let unlaunched = f.run(&["show", "unlaunched"]).unwrap();
    assert_eq!(unlaunched["session"], Value::Null);
    assert_eq!(unlaunched["latest_attempt_id"], Value::Null);
    assert_eq!(unlaunched["latest_attempt_outcome"], Value::Null);
    assert_eq!(unlaunched["attempts"], json!([]));
    assert_eq!(unlaunched["reserved_slot"], false);
    assert_eq!(unlaunched["observed_stdout_bytes"], Value::Null);
    assert_eq!(held["attempts"].as_array().unwrap().len(), 1);
    assert_eq!(finished["attempts"].as_array().unwrap().len(), 1);
    assert!(!held.to_string().contains("finished-session"));
    assert!(!finished.to_string().contains("held-session"));
    assert!(!unlaunched.to_string().contains("secret-prompt"));
    assert_eq!(
        f.db()
            .query_row(
                "SELECT status FROM reservations WHERE task_id='held'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "reserved"
    );
    assert_eq!(
        f.db()
            .query_row(
                "SELECT status FROM reservations WHERE task_id='finished'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "released"
    );
}
