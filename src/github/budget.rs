//! Cycle-local accounting shared by all gh adapters, including retry attempts.
//! A reserve suppresses only optional sweeps, never command transport or writes.
use crate::config::GithubPolling;
use serde::{Deserialize, Serialize};
mod response;
use std::{
    collections::HashSet,
    sync::Mutex,
    thread::{self, ThreadId},
};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ApiUsage {
    pub rest: u64,
    pub graphql: u64,
    pub graphql_points: u64,
    pub core_remaining: Option<u64>,
    pub core_reset: Option<u64>,
    pub graphql_remaining: Option<u64>,
    pub graphql_reset: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct BudgetExhaustion {
    pub resource: &'static str,
    pub reset_at_unix_secs: Option<u64>,
}

impl ApiUsage {
    /// The observation stops indicating exhaustion once its reset time passes.
    pub fn exhausted(&self, now: u64) -> Vec<BudgetExhaustion> {
        [
            ("core", self.core_remaining, self.core_reset),
            ("graphql", self.graphql_remaining, self.graphql_reset),
        ]
        .into_iter()
        .filter(|(_, remaining, reset)| *remaining == Some(0) && reset.is_none_or(|at| at > now))
        .map(|(resource, _, reset_at_unix_secs)| BudgetExhaustion {
            resource,
            reset_at_unix_secs,
        })
        .collect()
    }
}

/// A scheduling deferral, not a task failure or a transport veto.
pub(crate) fn deferral_reason() -> Option<String> {
    with_cycle(|cycle| {
        let c = cycle?;
        if optional_allowed_for(c) {
            return None;
        }
        let reason = if c.usage.core_remaining == Some(0) || c.usage.graphql_remaining == Some(0) {
            "github_budget_exhausted"
        } else {
            "github_budget_reserved"
        };
        Some(format!(
            "{reason}: core_remaining={:?} core_reset={:?} graphql_remaining={:?} graphql_reset={:?}; PR maintenance deferred",
            c.usage.core_remaining,
            c.usage.core_reset,
            c.usage.graphql_remaining,
            c.usage.graphql_reset,
        ))
    })
}

pub(crate) fn task_deferral_reason(task: &str) -> Option<String> {
    if !task_allowed(task) {
        return None;
    }
    deferral_reason()
}

fn optional_allowed_for(c: &Cycle) -> bool {
    c.usage.core_remaining.is_none_or(|n| n > c.policy.reserve)
        && c.usage
            .graphql_remaining
            .is_none_or(|n| n > c.policy.reserve.saturating_add(c.graphql_peak_cost))
}

struct Cycle {
    policy: GithubPolling,
    usage: ApiUsage,
    deferred: HashSet<String>,
    graphql_peak_cost: u64,
}
// Thread-keyed scopes isolate concurrent callers without a process-global
// policy leak. No network or filesystem work is performed while locked.
static CYCLES: Mutex<Vec<(ThreadId, Cycle)>> = Mutex::new(Vec::new());

fn with_cycle<T>(f: impl FnOnce(Option<&mut Cycle>) -> T) -> T {
    let mut cycles = CYCLES.lock().expect("GitHub accounting lock poisoned");
    let id = thread::current().id();
    f(cycles
        .iter_mut()
        .find(|(key, _)| *key == id)
        .map(|(_, cycle)| cycle))
}

pub(crate) struct CycleGuard;
impl CycleGuard {
    pub(crate) fn start(policy: &GithubPolling) -> Self {
        let mut cycles = CYCLES.lock().expect("GitHub accounting lock poisoned");
        let id = thread::current().id();
        assert!(
            !cycles.iter().any(|(key, _)| *key == id),
            "nested GitHub cycle"
        );
        cycles.push((
            id,
            Cycle {
                policy: policy.clone(),
                usage: ApiUsage::default(),
                deferred: HashSet::new(),
                graphql_peak_cost: 0,
            },
        ));
        Self
    }
}
impl Drop for CycleGuard {
    fn drop(&mut self) {
        let id = thread::current().id();
        CYCLES
            .lock()
            .expect("GitHub accounting lock poisoned")
            .retain(|(key, _)| *key != id);
    }
}

/// Execute cycles own terminal PR settlement, including tracker obligations.
/// Read-only receipt reconciliation must hand terminal delivery to that pass
/// rather than retire it early and make the followup candidate disappear.
pub(crate) fn owns_terminal_settlement() -> bool {
    with_cycle(|cycle| cycle.is_some())
}

pub(crate) fn usage() -> ApiUsage {
    with_cycle(|cycle| cycle.map(|c| c.usage.clone()).unwrap_or_default())
}

pub(crate) fn interval() -> u64 {
    with_cycle(|cycle| cycle.map_or(0, |c| c.policy.interval_seconds))
}

pub(crate) fn optional_allowed() -> bool {
    with_cycle(|cycle| cycle.as_deref().is_none_or(optional_allowed_for))
}

pub(crate) fn command(args: &[&str]) {
    with_cycle(|cycle| {
        if let Some(c) = cycle {
            if args.contains(&"graphql") {
                c.usage.graphql += 1;
            } else {
                c.usage.rest += 1;
            }
        }
    });
}

pub(crate) fn headers(bytes: &[u8], graphql: bool) -> std::io::Result<()> {
    let text = String::from_utf8_lossy(bytes);
    let mut remaining = None;
    let mut reset = None;
    let mut resource = None;
    for line in text.lines().take_while(|line| !line.is_empty()) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        match name.to_ascii_lowercase().as_str() {
            "x-ratelimit-remaining" => remaining = Some(parse_header(value)?),
            "x-ratelimit-reset" => reset = Some(parse_header(value)?),
            "x-ratelimit-resource" => resource = Some(value.trim()),
            _ => {}
        }
    }
    with_cycle(|cycle| {
        if let Some(c) = cycle {
            match resource.unwrap_or(if graphql { "graphql" } else { "core" }) {
                "core" => {
                    if remaining.is_some() {
                        c.usage.core_remaining = remaining;
                    }
                    if reset.is_some() {
                        c.usage.core_reset = reset;
                    }
                }
                "graphql" => {
                    if remaining.is_some() {
                        c.usage.graphql_remaining = remaining;
                    }
                    if reset.is_some() {
                        c.usage.graphql_reset = reset;
                    }
                }
                _ => {}
            }
        }
    });
    Ok(())
}

fn parse_header(value: &str) -> std::io::Result<u64> {
    value
        .trim()
        .parse()
        .map_err(|_| std::io::Error::other("GitHub quota header malformed"))
}

/// A terminal observation owns the task for the rest of this cycle, including
/// failed proof/close attempts. Do not duplicate it in merge, cleanup or repair.
pub(crate) fn defer_task(task: &str) {
    with_cycle(|cycle| {
        if let Some(c) = cycle {
            c.deferred.insert(task.into());
        }
    });
}
pub(crate) fn task_allowed(task: &str) -> bool {
    with_cycle(|cycle| cycle.is_none_or(|c| !c.deferred.contains(task)))
}

pub(crate) fn set_quota(value: &serde_json::Value) -> std::io::Result<()> {
    let field = |resource: &str, name: &str| {
        value["resources"][resource][name]
            .as_u64()
            .ok_or_else(|| std::io::Error::other("GitHub quota response malformed"))
    };
    let core = (field("core", "remaining")?, field("core", "reset")?);
    let graphql = (field("graphql", "remaining")?, field("graphql", "reset")?);
    with_cycle(|cycle| {
        if let Some(c) = cycle {
            c.usage.core_remaining = Some(core.0);
            c.usage.core_reset = Some(core.1);
            c.usage.graphql_remaining = Some(graphql.0);
            c.usage.graphql_reset = Some(graphql.1);
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests;

/// Account every response, including retries and failed GraphQL operations.
pub(crate) fn graphql_response(bytes: &[u8]) -> std::io::Result<()> {
    // Invalid JSON is diagnosed by the adapter, not reclassified as a quota error.
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return Ok(());
    };
    let quota = response::quota(&value)?;
    with_cycle(|cycle| {
        if let Some(c) = cycle {
            if let Some(quota) = quota {
                c.usage.graphql_points += quota.cost;
                c.graphql_peak_cost = c.graphql_peak_cost.max(quota.cost);
                c.usage.graphql_remaining = Some(quota.remaining);
                c.usage.graphql_reset = Some(quota.reset);
            }
            if response::exhausted(&value) {
                c.usage.graphql_remaining = Some(0);
            }
        }
    });
    Ok(())
}
