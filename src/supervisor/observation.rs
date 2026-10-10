use super::{
    error::SupervisorError,
    log_capture::LogDrains,
    log_failure::{record_log_failure, stop_failed_log_child},
    stop_control::handle_stop,
};
use crate::model::{ChildIdentity, LaunchPlan};
use std::{
    os::unix::net::UnixListener,
    path::Path,
    process::{Child, ExitStatus},
    thread,
    time::{Duration, Instant},
};

#[derive(Default)]
struct Observation {
    status: Option<ExitStatus>,
    stdout_bytes: Option<u64>,
    stderr_bytes: Option<u64>,
    exited_at: Option<Instant>,
    stop_signals: Vec<i32>,
}

pub(crate) struct CompletedWorker {
    pub status: ExitStatus,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub stop_signals: Vec<i32>,
}

pub(crate) fn observe_worker(
    child: &mut Child,
    registered: &ChildIdentity,
    drains: LogDrains,
    plan: &LaunchPlan,
    root: &Path,
    control: Option<&UnixListener>,
) -> Result<CompletedWorker, SupervisorError> {
    let mut observation = Observation::default();
    while !observation.complete() {
        observation.poll_exit(child)?;
        observation.receive_logs(child, registered, &drains, plan, root)?;
        if observation.complete() {
            break;
        }
        if observation
            .exited_at
            .is_some_and(|at| at.elapsed() > Duration::from_secs(2))
        {
            return Err(SupervisorError::ExecutionUnavailable);
        }
        if observation.status.is_none()
            && let Some(listener) = control
        {
            accept_stop(
                listener,
                plan,
                root,
                child,
                registered,
                &mut observation.stop_signals,
            )?;
        }
        thread::sleep(Duration::from_millis(20));
    }
    drains.join()?;
    Ok(CompletedWorker {
        status: observation.status.expect("worker exited"),
        stdout_bytes: observation.stdout_bytes.expect("stdout drained"),
        stderr_bytes: observation.stderr_bytes.expect("stderr drained"),
        stop_signals: observation.stop_signals,
    })
}

impl Observation {
    fn complete(&self) -> bool {
        self.status.is_some() && self.stdout_bytes.is_some() && self.stderr_bytes.is_some()
    }

    fn poll_exit(&mut self, child: &mut Child) -> Result<(), SupervisorError> {
        if self.status.is_none() {
            self.status = child.try_wait()?;
            if self.status.is_some() {
                self.exited_at = Some(Instant::now());
            }
        }
        Ok(())
    }

    fn receive_logs(
        &mut self,
        child: &mut Child,
        registered: &ChildIdentity,
        drains: &LogDrains,
        plan: &LaunchPlan,
        root: &Path,
    ) -> Result<(), SupervisorError> {
        while let Ok((stream, result)) = drains.receiver.try_recv() {
            let bytes = match result {
                Ok(bytes) => bytes,
                Err(error) => {
                    let stopped = stop_failed_log_child(child, registered);
                    let evidence = record_log_failure(plan, root, stream, &error.to_string());
                    stopped?;
                    evidence?;
                    return Err(SupervisorError::ExecutionUnavailable);
                }
            };
            match stream {
                "stdout" => self.stdout_bytes = Some(bytes),
                "stderr" => self.stderr_bytes = Some(bytes),
                _ => unreachable!(),
            }
        }
        Ok(())
    }
}

fn accept_stop(
    listener: &UnixListener,
    plan: &LaunchPlan,
    root: &Path,
    child: &mut Child,
    registered: &ChildIdentity,
    signals: &mut Vec<i32>,
) -> Result<(), SupervisorError> {
    match listener.accept() {
        Ok((mut stream, _)) => handle_stop(
            &mut stream,
            plan,
            root,
            child,
            &registered.boot_identity,
            &registered.start_identity,
            signals,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
        Err(error) => Err(error.into()),
    }
}
