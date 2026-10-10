use serde_json::Value;

pub(crate) struct CheckRunPage<'a> {
    pub(crate) runs: &'a [Value],
    pub(crate) total: usize,
}

impl<'a> CheckRunPage<'a> {
    pub(crate) fn parse(response: &'a Value, collected: usize) -> Option<Self> {
        let runs = response.get("check_runs")?.as_array()?;
        let total = response.get("total_count")?.as_u64()? as usize;
        if runs.len() > 100 || collected.saturating_add(runs.len()) > total {
            return None;
        }
        Some(Self { runs, total })
    }
}

pub(crate) struct CheckRun<'a> {
    name: &'a str,
    status: &'a str,
    conclusion: &'a str,
}

impl<'a> CheckRun<'a> {
    pub(crate) fn parse(run: &'a Value) -> Option<Self> {
        let name = run.get("name")?.as_str()?;
        let status = run.get("status")?.as_str()?;
        if !valid_name(name) || status.is_empty() || !valid_state(status) {
            return None;
        }
        let conclusion = run
            .get("conclusion")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        if !valid_state(conclusion) {
            return None;
        }
        Some(Self {
            name,
            status,
            conclusion,
        })
    }

    pub(crate) fn summary(&self) -> Value {
        Value::String(format!("{}:{}:{}", self.name, self.status, self.conclusion))
    }
}

pub(crate) fn append_statuses(result: &mut Vec<Value>, response: &Value) -> Option<()> {
    let statuses = response.get("statuses")?.as_array()?;
    if statuses.len() > 1000 {
        return None;
    }
    for item in statuses {
        let summary = status_summary(item)?;
        if !result
            .iter()
            .any(|existing| existing.as_str() == Some(&summary))
        {
            result.push(Value::String(summary));
        }
    }
    Some(())
}

fn status_summary(item: &Value) -> Option<String> {
    let context = item.get("context")?.as_str()?;
    let state = item.get("state")?.as_str()?;
    if !valid_name(context) || state.is_empty() || !valid_state(state) {
        return None;
    }
    Some(format!("{context}:{state}"))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' ')
}

fn valid_state(state: &str) -> bool {
    state.len() <= 32
        && state
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
}
