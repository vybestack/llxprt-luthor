use super::support::random_id;
use crate::{
    config::Config,
    state::requests::{Request, RequestKind},
};
use serde_json::json;
use std::path::Path;

type Error = Box<dyn std::error::Error>;

pub(crate) fn retry(config: Config, args: &[String], wait_reason: &str) -> Result<(), Error> {
    submit(
        Request {
            task_id: args[0].clone(),
            previous_attempt_id: args[2].clone(),
            attempt_id: format!("attempt-{}", random_id()?),
            config,
            kind: RequestKind::Retry {
                revision: args[6].clone(),
                actor: args[8].clone(),
                reason: args[10].clone(),
                revalidate_terminal_exit: args
                    .iter()
                    .any(|arg| arg == "--revalidate-terminal-exit"),
            },
        },
        wait_reason,
    )
}

pub(crate) fn resume(config: Config, task: &str, wait_reason: &str) -> Result<(), Error> {
    submit(
        Request {
            task_id: task.into(),
            previous_attempt_id: String::new(),
            attempt_id: format!("attempt-{}", random_id()?),
            config,
            kind: RequestKind::Resume,
        },
        wait_reason,
    )
}

fn submit(request: Request, wait_reason: &str) -> Result<(), Error> {
    let task = request.task_id.clone();
    let id = crate::coordinator::requests::enqueue(request, Path::new("gh"), wait_reason)?;
    println!(
        "{}",
        json!({"task_id":task,"request_id":id,"attempt_id":id,
        "status":"queued","reason":format!("{wait_reason}; request queued")})
    );
    Ok(())
}
