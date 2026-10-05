use super::database::StateStore;
use crate::model::StateError;
use rusqlite::params;

/// Nonterminal attempts must each be inspected on startup, including attempts
/// without a launch intent (an inconsistent durable state is not safe to skip).
pub fn pending_attempts(store: &StateStore) -> Result<Vec<(String, String)>, StateError> {
    let mut statement = store.connection.prepare(
        "SELECT a.task_id,a.id FROM attempts a WHERE NOT (
             a.lifecycle='telemetry_lost' AND a.outcome IS NULL
             AND EXISTS (SELECT 1 FROM tasks t WHERE t.id=a.task_id AND t.state='pr_complete')
             AND EXISTS (SELECT 1 FROM reservations r WHERE r.task_id=a.task_id AND r.attempt_id=a.id AND r.status='released')
             AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=a.task_id AND e.attempt_id=a.id AND e.kind='telemetry_lost'
               AND CASE WHEN json_valid(e.payload) THEN json_type(e.payload,'$.actor')='text' AND length(trim(json_extract(e.payload,'$.actor')))>0
                 AND json_type(e.payload,'$.reason')='text' AND length(trim(json_extract(e.payload,'$.reason')))>0
                 AND json_type(e.payload,'$.observed_at_unix_secs')='integer' AND json_extract(e.payload,'$.observed_at_unix_secs')>0
                 AND json_type(e.payload,'$.os_ids')='array' AND json_array_length(e.payload,'$.os_ids')>0
                 AND json_type(e.payload,'$.exit_code') IS NULL AND json_type(e.payload,'$.signal') IS NULL ELSE 0 END)=1
             AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=a.task_id AND e.attempt_id=a.id AND e.kind='exit_pr_lookup'
               AND CASE WHEN json_valid(e.payload) THEN json_extract(e.payload,'$.status.status')='open' ELSE 0 END)=1
             AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=a.task_id AND e.attempt_id=a.id AND e.kind='verified_open_pr'
               AND CASE WHEN json_valid(e.payload) THEN json_type(e.payload,'$.id')='integer' AND json_extract(e.payload,'$.id')>0 ELSE 0 END)=1
             AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=a.task_id AND e.attempt_id=a.id AND e.kind IN ('attempt_exit','telemetry_lost','exit_pr_lookup','verified_open_pr'))=3
             AND a.id=(SELECT id FROM attempts WHERE task_id=a.task_id ORDER BY rowid DESC LIMIT 1)
             AND NOT EXISTS (SELECT 1 FROM attempts a2 WHERE a2.task_id=a.task_id AND a2.id<>a.id AND (a2.lifecycle!='completed' OR a2.outcome IS NULL OR EXISTS (SELECT 1 FROM reservations r2 WHERE r2.task_id=a2.task_id AND r2.attempt_id=a2.id AND r2.status='reserved') OR (EXISTS (SELECT 1 FROM evidence e2 WHERE e2.task_id=a2.task_id AND e2.attempt_id=a2.id AND e2.kind='attempt_exit') AND NOT EXISTS (SELECT 1 FROM evidence e2 WHERE e2.task_id=a2.task_id AND e2.attempt_id=a2.id AND e2.kind=CASE WHEN (SELECT COUNT(*) FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit')=1 AND json_valid((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit')) AND json_type((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit'),'$.stop_signals')='array' AND json_array_length((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit'),'$.stop_signals')>0 THEN 'pause_pr_lookup' WHEN (SELECT COUNT(*) FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit')=1 AND json_valid((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit')) AND json_type((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit'),'$.stop_signals')='array' AND json_array_length((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit'),'$.stop_signals')=0 THEN 'exit_pr_lookup' END AND CASE WHEN json_valid(e2.payload) THEN json_extract(e2.payload,'$.status.status')='absent' ELSE 0 END))))
           ) AND (a.lifecycle!='completed' OR a.outcome IS NULL OR EXISTS
             (SELECT 1 FROM reservations r WHERE r.attempt_id=a.id AND r.status='reserved') OR
             (EXISTS (SELECT 1 FROM evidence e WHERE e.task_id=a.task_id AND e.attempt_id=a.id AND e.kind='attempt_exit')
              AND NOT EXISTS (SELECT 1 FROM evidence e WHERE e.task_id=a.task_id AND e.attempt_id=a.id AND e.kind=CASE WHEN EXISTS
                 (SELECT 1 FROM intents i WHERE i.task_id=a.task_id AND i.attempt_id=a.id AND i.kind='stop') THEN 'pause_pr_lookup' ELSE 'exit_pr_lookup' END)))
             ORDER BY a.rowid",
    )?;
    Ok(statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?)
}

/// A live worker may be admitted alongside another only while its exact
/// attempt still owns a reservation and has no recorded exit.
pub(crate) fn active_attempt_reservation(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<bool, StateError> {
    Ok(store.connection.query_row(
        "SELECT COUNT(*) FROM attempts a JOIN reservations r ON r.attempt_id=a.id
             JOIN tasks t ON t.id=a.task_id WHERE a.id=?1 AND a.task_id=?2
             AND a.lifecycle='launch_intended' AND a.outcome IS NULL
             AND r.task_id=?2 AND r.status='reserved' AND t.state='held'
             AND NOT EXISTS (SELECT 1 FROM evidence e WHERE e.attempt_id=a.id AND e.kind='attempt_exit')",
        params![attempt_id, task_id], |row| row.get::<_, i64>(0)
    )? == 1)
}

/// Persisted source operations without their proof block new selections.
/// Also include preparing tasks where the process died before the first intent.
pub fn unresolved_sources(store: &StateStore) -> Result<Vec<(String, String)>, StateError> {
    let mut statement = store.connection.prepare(
        "SELECT t.id, 'selection' FROM tasks t WHERE t.state='preparing'
             UNION ALL
             SELECT t.id, 'prelaunch' FROM tasks t WHERE t.state NOT IN ('preparing','completed')
               AND NOT EXISTS (SELECT 1 FROM attempts a WHERE a.task_id=t.id)
             UNION ALL
             SELECT i.task_id, i.kind FROM intents i JOIN tasks t ON t.id=i.task_id
             WHERE t.state!='completed' AND (
               (i.kind='claim_assignment' AND NOT EXISTS
                 (SELECT 1 FROM evidence e WHERE e.task_id=i.task_id AND e.kind='claim_verified'))
               OR (i.kind='worktree_create' AND NOT EXISTS
                 (SELECT 1 FROM evidence e WHERE e.task_id=i.task_id AND e.kind='worktree_created'))
               OR (i.kind='stop' AND NOT EXISTS
                 (SELECT 1 FROM evidence e WHERE e.task_id=i.task_id AND e.attempt_id=i.attempt_id AND e.kind='attempt_exit')
                 AND NOT (
                   t.state='pr_complete'
                   AND EXISTS (SELECT 1 FROM attempts a JOIN reservations r ON r.task_id=a.task_id AND r.attempt_id=a.id
                     WHERE a.task_id=i.task_id AND a.id=i.attempt_id AND a.lifecycle='telemetry_lost'
                       AND a.outcome IS NULL AND r.status='released'
                       AND a.id=(SELECT id FROM attempts WHERE task_id=i.task_id ORDER BY rowid DESC LIMIT 1))
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=i.task_id AND e.attempt_id=i.attempt_id
                     AND e.kind IN ('telemetry_lost','exit_pr_lookup','verified_open_pr'))=3
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=i.task_id AND e.attempt_id=i.attempt_id
                     AND e.kind='telemetry_lost' AND CASE WHEN json_valid(e.payload) THEN
                       json_type(e.payload,'$.actor')='text' AND length(trim(json_extract(e.payload,'$.actor')))>0
                       AND json_type(e.payload,'$.reason')='text' AND length(trim(json_extract(e.payload,'$.reason')))>0
                       AND json_type(e.payload,'$.observed_at_unix_secs')='integer' AND json_extract(e.payload,'$.observed_at_unix_secs')>0
                       AND json_type(e.payload,'$.os_ids')='array' AND json_array_length(e.payload,'$.os_ids')>0
                       AND json_type(e.payload,'$.exit_code') IS NULL AND json_type(e.payload,'$.signal') IS NULL ELSE 0 END)=1
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=i.task_id AND e.attempt_id=i.attempt_id
                     AND e.kind='exit_pr_lookup' AND CASE WHEN json_valid(e.payload) THEN
                       json_extract(e.payload,'$.status.status')='open'
                       AND json_type(e.payload,'$.observed_at_unix_secs')='integer'
                       AND json_extract(e.payload,'$.observed_at_unix_secs')>0 ELSE 0 END)=1
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=i.task_id AND e.attempt_id=i.attempt_id
                     AND e.kind='verified_open_pr' AND CASE WHEN json_valid(e.payload) THEN
                       json_type(e.payload,'$.id')='integer' AND json_extract(e.payload,'$.id')>0 ELSE 0 END)=1
                 ))
             ) ORDER BY 1,2",
    )?;
    Ok(statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?)
}

/// A selected task occupies a scheduling slot even if it is held before launch.
/// A verified exit releases its reservation but leaves the task held for PR proof.
pub fn ensure_dispatch_capacity(store: &StateStore) -> Result<(), StateError> {
    let capacity: usize = store.connection.query_row(
        "SELECT value FROM state_meta WHERE key='capacity'",
        [],
        |row| row.get(0),
    )?;
    let active_reservations: usize = store.connection.query_row(
        "SELECT COUNT(DISTINCT task_id) FROM reservations WHERE status='reserved'",
        [],
        |row| row.get(0),
    )?;
    let unresolved_tasks = unresolved_task_count(&store.connection)?;
    if active_reservations >= capacity || unresolved_tasks >= capacity {
        return Err(StateError::Capacity {
            reserved: unresolved_tasks.max(active_reservations),
            capacity,
        });
    }
    Ok(())
}

pub fn reserve(store: &mut StateStore, task_id: &str, attempt_id: &str) -> Result<(), StateError> {
    let tx = store.connection.transaction()?;
    let capacity: usize = tx.query_row(
        "SELECT value FROM state_meta WHERE key='capacity'",
        [],
        |r| r.get(0),
    )?;
    let reserved: usize = tx.query_row(
        "SELECT COUNT(*) FROM reservations WHERE status='reserved'",
        [],
        |r| r.get(0),
    )?;
    if reserved >= capacity {
        return Err(StateError::Capacity { reserved, capacity });
    }
    tx.execute(
        "INSERT INTO reservations(attempt_id,task_id,status) VALUES(?2,?1,'reserved')",
        params![task_id, attempt_id],
    )?;
    tx.execute(
        "INSERT INTO attempts(id,task_id,lifecycle) VALUES(?1,?2,'launch_intended')",
        params![attempt_id, task_id],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn release_reservation(store: &mut StateStore, attempt_id: &str) -> Result<usize, StateError> {
    let launch: i64 = store.connection.query_row(
        "SELECT COUNT(*) FROM intents WHERE attempt_id=?1 AND kind='launch'",
        [attempt_id],
        |row| row.get(0),
    )?;
    if launch != 0 {
        return Err(StateError::LaunchBlocked);
    }
    store.connection.execute(
        "UPDATE reservations SET status='released' WHERE attempt_id=?1 AND status='reserved'",
        [attempt_id],
    )?;
    Ok(store.connection.changes() as usize)
}

pub fn reservation_count(store: &StateStore) -> Result<usize, StateError> {
    Ok(store.connection.query_row(
        "SELECT COUNT(*) FROM reservations WHERE status='reserved'",
        [],
        |r| r.get(0),
    )?)
}

fn unresolved_task_count(connection: &rusqlite::Connection) -> Result<usize, StateError> {
    Ok(connection.query_row(
        "SELECT COUNT(DISTINCT t.id) FROM tasks t WHERE t.state!='completed'
             AND NOT (t.state='pr_complete'
               AND NOT EXISTS (SELECT 1 FROM reservations r WHERE r.task_id=t.id AND r.status='reserved')
               AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=t.id AND e.kind='verified_open_pr'
                 AND CASE WHEN json_valid(e.payload) THEN json_type(e.payload,'$.id')='integer' AND json_extract(e.payload,'$.id') > 0 ELSE 0 END)=1
               AND (EXISTS (SELECT 1 FROM evidence p JOIN attempts a ON a.task_id=p.task_id AND a.id=p.attempt_id
                 JOIN reservations r ON r.task_id=a.task_id AND r.attempt_id=a.id
                 WHERE p.task_id=t.id AND p.kind='verified_open_pr'
                   AND CASE WHEN json_valid(p.payload) THEN json_type(p.payload,'$.id')='integer' AND json_extract(p.payload,'$.id') > 0 ELSE 0 END
                   AND a.id=(SELECT id FROM attempts WHERE task_id=t.id ORDER BY rowid DESC LIMIT 1)
                   AND a.lifecycle='completed' AND a.outcome IS NOT NULL AND r.status='released'
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.attempt_id=a.id AND e.kind='attempt_exit')=1
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=t.id AND e.kind='claim_verified')=1
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=t.id AND e.kind='worktree_created')=1
                   AND (SELECT COUNT(*) FROM attempts a2 WHERE a2.task_id=t.id AND (a2.lifecycle!='completed' OR a2.outcome IS NULL))=0)
               OR EXISTS (SELECT 1 FROM evidence p JOIN attempts a ON a.task_id=p.task_id AND a.id=p.attempt_id
                 JOIN reservations r ON r.task_id=a.task_id AND r.attempt_id=a.id
                 WHERE p.task_id=t.id AND p.kind='verified_open_pr'
                   AND CASE WHEN json_valid(p.payload) THEN json_type(p.payload,'$.id')='integer' AND json_extract(p.payload,'$.id')>0 ELSE 0 END
                   AND a.id=(SELECT id FROM attempts WHERE task_id=t.id ORDER BY rowid DESC LIMIT 1)
                   AND a.lifecycle='telemetry_lost' AND a.outcome IS NULL AND r.status='released'
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=t.id AND e.attempt_id=a.id AND e.kind='telemetry_lost'
                     AND CASE WHEN json_valid(e.payload) THEN json_type(e.payload,'$.actor')='text' AND length(trim(json_extract(e.payload,'$.actor')))>0
                       AND json_type(e.payload,'$.reason')='text' AND length(trim(json_extract(e.payload,'$.reason')))>0
                       AND json_type(e.payload,'$.observed_at_unix_secs')='integer' AND json_extract(e.payload,'$.observed_at_unix_secs')>0
                       AND json_type(e.payload,'$.os_ids')='array' AND json_array_length(e.payload,'$.os_ids')>0
                       AND json_type(e.payload,'$.exit_code') IS NULL AND json_type(e.payload,'$.signal') IS NULL ELSE 0 END)=1
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=t.id AND e.attempt_id=a.id AND e.kind='exit_pr_lookup'
                     AND CASE WHEN json_valid(e.payload) THEN json_extract(e.payload,'$.status.status')='open' ELSE 0 END)=1
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=t.id AND e.attempt_id=a.id AND e.kind IN ('attempt_exit','telemetry_lost','exit_pr_lookup','verified_open_pr'))=3
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=t.id AND e.attempt_id=a.id AND e.kind='verified_open_pr'
                     AND CASE WHEN json_valid(e.payload) THEN json_type(e.payload,'$.id')='integer' AND json_extract(e.payload,'$.id')>0 ELSE 0 END)=1
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=t.id AND e.kind='claim_verified')=1
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=t.id AND e.kind='worktree_created')=1
                   AND NOT EXISTS (SELECT 1 FROM attempts a2 WHERE a2.task_id=t.id AND a2.id<>a.id AND (a2.lifecycle!='completed' OR a2.outcome IS NULL OR EXISTS (SELECT 1 FROM reservations r2 WHERE r2.task_id=a2.task_id AND r2.attempt_id=a2.id AND r2.status='reserved') OR (EXISTS (SELECT 1 FROM evidence e2 WHERE e2.task_id=a2.task_id AND e2.attempt_id=a2.id AND e2.kind='attempt_exit') AND NOT EXISTS (SELECT 1 FROM evidence e2 WHERE e2.task_id=a2.task_id AND e2.attempt_id=a2.id AND e2.kind=CASE WHEN (SELECT COUNT(*) FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit')=1 AND json_valid((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit')) AND json_type((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit'),'$.stop_signals')='array' AND json_array_length((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit'),'$.stop_signals')>0 THEN 'pause_pr_lookup' WHEN (SELECT COUNT(*) FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit')=1 AND json_valid((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit')) AND json_type((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit'),'$.stop_signals')='array' AND json_array_length((SELECT payload FROM evidence WHERE task_id=a2.task_id AND attempt_id=a2.id AND kind='attempt_exit'),'$.stop_signals')=0 THEN 'exit_pr_lookup' END AND CASE WHEN json_valid(e2.payload) THEN json_extract(e2.payload,'$.status.status')='absent' ELSE 0 END)))))))
             AND NOT (t.state='paused'
               AND EXISTS (SELECT 1 FROM evidence e WHERE e.task_id=t.id AND e.attempt_id=(SELECT id FROM attempts WHERE task_id=t.id ORDER BY rowid DESC LIMIT 1) AND e.kind='pause_pr_lookup' AND CASE WHEN json_valid(e.payload) THEN json_extract(e.payload,'$.status.status')='absent' ELSE 0 END)
               AND NOT EXISTS (SELECT 1 FROM reservations r WHERE r.task_id=t.id AND r.status='reserved')
               AND EXISTS (SELECT 1 FROM attempts a JOIN reservations r ON r.attempt_id=a.id
                 WHERE a.task_id=t.id AND a.lifecycle='completed' AND a.outcome IS NOT NULL
                   AND r.task_id=t.id AND r.status='released'
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.attempt_id=a.id AND e.kind='attempt_exit')=1
                   AND (SELECT COUNT(*) FROM intents i WHERE i.task_id=t.id AND i.attempt_id=a.id AND i.kind='stop')=1
                   AND (SELECT COUNT(*) FROM intents i WHERE i.task_id=t.id AND i.attempt_id=a.id AND i.kind='launch')=1
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=t.id AND e.kind='claim_verified')=1
                   AND (SELECT COUNT(*) FROM evidence e WHERE e.task_id=t.id AND e.kind='worktree_created')=1
                   AND (SELECT COUNT(*) FROM attempts a2 WHERE a2.task_id=t.id AND (a2.lifecycle!='completed' OR a2.outcome IS NULL))=0)
             OR (t.state='attention'
               AND NOT EXISTS (SELECT 1 FROM reservations r WHERE r.task_id=t.id AND r.status='reserved')
               AND EXISTS (SELECT 1 FROM attempts a JOIN reservations r ON r.attempt_id=a.id AND r.task_id=t.id
                 JOIN evidence e ON e.task_id=t.id AND e.attempt_id=a.id AND e.kind='attempt_exit'
                 JOIN evidence p ON p.task_id=t.id AND p.attempt_id=a.id AND p.kind='exit_pr_lookup'
                 WHERE a.task_id=t.id AND a.id=(SELECT id FROM attempts WHERE task_id=t.id ORDER BY rowid DESC LIMIT 1)
                   AND a.lifecycle='completed' AND a.outcome IS NOT NULL AND r.status='released'
                   AND CASE WHEN json_valid(p.payload) THEN json_extract(p.payload,'$.status.status')='absent' ELSE 0 END
                   AND (SELECT COUNT(*) FROM evidence WHERE attempt_id=a.id AND kind='attempt_exit')=1
                   AND p.rowid=(SELECT MAX(p2.rowid) FROM evidence p2 WHERE p2.task_id=t.id AND p2.attempt_id=a.id AND p2.kind='exit_pr_lookup')
                   AND CASE WHEN json_valid(e.payload) THEN json_array_length(e.payload,'$.stop_signals')=0 ELSE 0 END
                   AND (SELECT COUNT(*) FROM intents WHERE task_id=t.id AND attempt_id=a.id AND kind='launch')=1
                   AND (SELECT COUNT(*) FROM evidence WHERE task_id=t.id AND kind='claim_verified')=1
                   AND (SELECT COUNT(*) FROM evidence WHERE task_id=t.id AND kind='worktree_created')=1
                   AND (SELECT COUNT(*) FROM attempts WHERE task_id=t.id AND (lifecycle!='completed' OR outcome IS NULL))=0)))",
        [],
        |row| row.get(0),
    )?)
}
