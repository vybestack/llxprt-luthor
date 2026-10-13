use super::{
    health::{FailedJob, PrHealth},
    types::{ErrorCategory, LookupError, error},
};
use serde_json::Value;

const QUERY: &str = "query($owner:String!,$repo:String!,$number:Int!,$cursor:String){repository(owner:$owner,name:$repo){pullRequest(number:$number){databaseId headRefOid mergeable commits(last:1){nodes{commit{oid statusCheckRollup{contexts(first:100,after:$cursor){nodes{__typename ... on CheckRun{databaseId name status conclusion detailsUrl checkSuite{databaseId workflowRun{databaseId runAttempt}} isRequired(pullRequestNumber:$number)} ... on StatusContext{context state isRequired(pullRequestNumber:$number)}} pageInfo{hasNextPage endCursor}}}}}}}}}";

pub(crate) fn observe(
    repository: &str,
    number: u64,
    mut api: impl FnMut(&[&str]) -> Result<Value, LookupError>,
) -> Result<PrHealth, LookupError> {
    let (owner, repo) = repository.split_once('/').ok_or_else(malformed)?;
    let mut cursor = None;
    let mut health = None;
    let mut contexts = Vec::new();
    for _ in 0..100 {
        let response = read_page(&mut api, owner, repo, number, cursor.as_deref())?;
        if response.get("errors").is_some() {
            return Err(malformed());
        }
        let pr = response
            .pointer("/data/repository/pullRequest")
            .ok_or_else(malformed)?;
        let snapshot = snapshot(pr)?;
        if health.as_ref().is_some_and(|prior: &PrHealth| {
            prior.id != snapshot.id
                || prior.head != snapshot.head
                || prior.conflicting != snapshot.conflicting
        }) {
            return Err(malformed());
        }
        let current = health.get_or_insert(snapshot);
        let commits = pr
            .pointer("/commits/nodes")
            .and_then(Value::as_array)
            .ok_or_else(malformed)?;
        let [commit] = commits.as_slice() else {
            return Err(malformed());
        };
        if commit.pointer("/commit/oid").and_then(Value::as_str) != Some(&current.head) {
            return Err(malformed());
        }
        let rollup = commit
            .pointer("/commit/statusCheckRollup")
            .ok_or_else(malformed)?;
        // GitHub represents a head with no checks by a null rollup.
        if rollup.is_null() {
            if cursor.is_some() {
                return Err(malformed());
            }
            return Ok(current.clone());
        }
        collect_contexts(&mut contexts, rollup)?;
        match next_cursor(rollup)? {
            None => {
                append_failures(current, &contexts, repository, &mut api)?;
                return Ok(current.clone());
            }
            Some(next) if cursor.as_ref() != Some(&next) => cursor = Some(next),
            _ => return Err(malformed()),
        }
    }
    Err(malformed())
}

fn read_page(
    api: &mut impl FnMut(&[&str]) -> Result<Value, LookupError>,
    owner: &str,
    repo: &str,
    number: u64,
    cursor: Option<&str>,
) -> Result<Value, LookupError> {
    let owner = format!("owner={owner}");
    let repo = format!("repo={repo}");
    let number = format!("number={number}");
    let mut args = vec![
        "api", "graphql", "-f", "query=", "-f", &owner, "-f", &repo, "-F", &number,
    ];
    let query = format!("query={QUERY}");
    args[3] = &query;
    let cursor = cursor.map(|value| format!("cursor={value}"));
    if let Some(cursor) = &cursor {
        args.extend(["-f", cursor]);
    }
    api(&args)
}

fn snapshot(pr: &Value) -> Result<PrHealth, LookupError> {
    let id = pr["databaseId"]
        .as_u64()
        .filter(|id| *id > 0)
        .ok_or_else(malformed)?;
    let head = pr["headRefOid"]
        .as_str()
        .filter(|sha| sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(malformed)?;
    let conflicting = match pr["mergeable"].as_str() {
        Some("CONFLICTING") => true,
        Some("MERGEABLE" | "UNKNOWN") => false,
        _ => return Err(malformed()),
    };
    Ok(PrHealth {
        id,
        head: head.into(),
        conflicting,
        failed_required_checks: Vec::new(),
        failed_jobs: Vec::new(),
        required_jobs: Vec::new(),
        required_checks_pending: false,
    })
}

fn append_failures(
    health: &mut PrHealth,
    nodes: &[Value],
    repository: &str,
    api: &mut impl FnMut(&[&str]) -> Result<Value, LookupError>,
) -> Result<(), LookupError> {
    for node in latest_contexts(nodes)? {
        let required = node["isRequired"].as_bool().ok_or_else(malformed)?;
        let (name, failed) = match node["__typename"].as_str() {
            Some("CheckRun") => (node["name"].as_str(), check_failed(node)?),
            Some("StatusContext") => (node["context"].as_str(), status_failed(node)?),
            _ => return Err(malformed()),
        };
        let name = name
            .filter(|name| !name.is_empty() && name.len() <= 128)
            .ok_or_else(malformed)?;
        if required
            && (node["status"]
                .as_str()
                .is_some_and(|status| status != "COMPLETED")
                || matches!(node["state"].as_str(), Some("PENDING" | "EXPECTED")))
        {
            health.required_checks_pending = true;
        }
        if required
            && node["__typename"] == "CheckRun"
            && node["status"] == "COMPLETED"
            && !node
                .pointer("/checkSuite/workflowRun")
                .is_none_or(Value::is_null)
        {
            let job = parse_job(node, name)?;
            if !current_job(&job, &health.head, repository, api)? {
                health.required_checks_pending = true;
                continue;
            }
            if failed {
                health.failed_jobs.push(job.clone());
            }
            health.required_jobs.push(job);
        }
        if required && failed {
            health.failed_required_checks.push(name.into());
        }
    }
    Ok(())
}

fn check_failed(node: &Value) -> Result<bool, LookupError> {
    match node["status"].as_str() {
        Some("QUEUED" | "IN_PROGRESS" | "WAITING" | "REQUESTED" | "PENDING")
            if node["conclusion"].is_null() =>
        {
            Ok(false)
        }
        Some("COMPLETED") => match node["conclusion"].as_str() {
            Some("SUCCESS" | "NEUTRAL" | "SKIPPED") => Ok(false),
            Some(
                "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE"
                | "STALE",
            ) => Ok(true),
            _ => Err(malformed()),
        },
        _ => Err(malformed()),
    }
}

fn status_failed(node: &Value) -> Result<bool, LookupError> {
    match node["state"].as_str() {
        Some("SUCCESS" | "PENDING" | "EXPECTED") => Ok(false),
        Some("ERROR" | "FAILURE") => Ok(true),
        _ => Err(malformed()),
    }
}

fn next_cursor(rollup: &Value) -> Result<Option<String>, LookupError> {
    match rollup
        .pointer("/contexts/pageInfo/hasNextPage")
        .and_then(Value::as_bool)
    {
        Some(false) => Ok(None),
        Some(true) => rollup
            .pointer("/contexts/pageInfo/endCursor")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|s| Some(s.into()))
            .ok_or_else(malformed),
        None => Err(malformed()),
    }
}

fn malformed() -> LookupError {
    error(ErrorCategory::Malformed, "invalid-pr-health", None)
}

fn parse_job(node: &Value, name: &str) -> Result<FailedJob, LookupError> {
    let run_id = node
        .pointer("/checkSuite/workflowRun/databaseId")
        .and_then(Value::as_u64)
        .filter(|id| *id > 0)
        .ok_or_else(malformed)?;
    let run_attempt = node
        .pointer("/checkSuite/workflowRun/runAttempt")
        .and_then(Value::as_u64)
        .filter(|id| *id > 0)
        .ok_or_else(malformed)?;
    let url = node["detailsUrl"].as_str().ok_or_else(malformed)?;
    let marker = format!("/actions/runs/{run_id}/job/");
    let job_id = url
        .split_once(&marker)
        .and_then(|(_, suffix)| suffix.split('?').next())
        .and_then(|id| id.parse::<u64>().ok())
        .filter(|id| *id > 0)
        .ok_or_else(malformed)?;
    Ok(FailedJob {
        name: name.into(),
        run_id,
        run_attempt,
        job_id,
    })
}

fn collect_contexts(contexts: &mut Vec<Value>, rollup: &Value) -> Result<(), LookupError> {
    let nodes = rollup
        .pointer("/contexts/nodes")
        .and_then(Value::as_array)
        .ok_or_else(malformed)?;
    if nodes.len() > 100 {
        return Err(malformed());
    }
    contexts.extend(nodes.iter().cloned());
    Ok(())
}

// A rerun creates new CheckRun IDs; the suite's runAttempt also advances on
// OLD check runs. Never classify an old failed job as a fresh repeat failure.
fn latest_contexts(nodes: &[Value]) -> Result<Vec<&Value>, LookupError> {
    let mut checks = std::collections::BTreeMap::new();
    let mut statuses = Vec::new();
    for node in nodes {
        match node["__typename"].as_str() {
            Some("CheckRun") => {
                let id = node["databaseId"]
                    .as_u64()
                    .filter(|id| *id > 0)
                    .ok_or_else(malformed)?;
                let suite = node
                    .pointer("/checkSuite/databaseId")
                    .and_then(Value::as_u64)
                    .filter(|id| *id > 0)
                    .ok_or_else(malformed)?;
                let name = node["name"].as_str().ok_or_else(malformed)?;
                let entry = checks.entry((suite, name)).or_insert((id, node));
                if id > entry.0 {
                    *entry = (id, node);
                }
            }
            Some("StatusContext") => statuses.push(node),
            _ => return Err(malformed()),
        }
    }
    statuses.extend(checks.into_values().map(|(_, node)| node));
    Ok(statuses)
}

// Suite runAttempt is shared by old CheckRuns. Verify the immutable job's own
// attempt before calling a visible failure a completed rerun.
fn current_job(
    job: &FailedJob,
    head: &str,
    repository: &str,
    api: &mut impl FnMut(&[&str]) -> Result<Value, LookupError>,
) -> Result<bool, LookupError> {
    let detail = api(&[
        "api",
        &format!("repos/{repository}/actions/jobs/{}", job.job_id),
    ])?;
    let attempt = detail["run_attempt"]
        .as_u64()
        .filter(|a| *a > 0 && *a <= job.run_attempt)
        .ok_or_else(malformed)?;
    if detail["id"].as_u64() != Some(job.job_id)
        || detail["run_id"].as_u64() != Some(job.run_id)
        || detail["head_sha"].as_str() != Some(head)
        || detail["name"].as_str() != Some(&job.name)
        || detail["status"].as_str() != Some("completed")
    {
        return Err(malformed());
    }
    Ok(attempt == job.run_attempt)
}
