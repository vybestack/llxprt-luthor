use super::{error::SupervisorError, storage::private_file};
use crate::model::LaunchPlan;
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Child,
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
};

pub(crate) struct LogCapture {
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
    stdout_file: File,
    stderr_file: File,
    out_sync: File,
    err_sync: File,
}

pub(crate) struct LogDrains {
    pub receiver: Receiver<(&'static str, std::io::Result<u64>)>,
    out_thread: JoinHandle<()>,
    err_thread: JoinHandle<()>,
}

impl LogCapture {
    pub fn open(root: &Path, plan: &LaunchPlan) -> Result<Self, SupervisorError> {
        fs::create_dir_all(root)?;
        let stdout_path = root.join(format!("{}.stdout.log", plan.attempt_id));
        let stderr_path = root.join(format!("{}.stderr.log", plan.attempt_id));
        let stdout_file = private_file(&stdout_path)?;
        let stderr_file = private_file(&stderr_path)?;
        let out_sync = stdout_file.try_clone()?;
        let err_sync = stderr_file.try_clone()?;
        Ok(Self {
            stdout_path,
            stderr_path,
            stdout_file,
            stderr_file,
            out_sync,
            err_sync,
        })
    }

    pub fn start<O, E>(
        self,
        child: &mut Child,
        writers: impl FnOnce(File, File) -> (O, E),
    ) -> (PathBuf, PathBuf, LogDrains)
    where
        O: Write + Send + 'static,
        E: Write + Send + 'static,
    {
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let (mut out_file, mut err_file) = writers(self.stdout_file, self.stderr_file);
        let (sender, receiver) = mpsc::channel();
        let out_sender = sender.clone();
        let out_thread = thread::spawn(move || {
            let result = drain_worker_log(stdout, &mut out_file, &self.out_sync);
            let _ = out_sender.send(("stdout", result));
        });
        let err_thread = thread::spawn(move || {
            let result = drain_worker_log(stderr, &mut err_file, &self.err_sync);
            let _ = sender.send(("stderr", result));
        });
        (
            self.stdout_path,
            self.stderr_path,
            LogDrains {
                receiver,
                out_thread,
                err_thread,
            },
        )
    }
}

impl LogDrains {
    pub fn join(self) -> Result<(), SupervisorError> {
        self.out_thread
            .join()
            .map_err(|_| SupervisorError::ExecutionUnavailable)?;
        self.err_thread
            .join()
            .map_err(|_| SupervisorError::ExecutionUnavailable)?;
        Ok(())
    }
}

fn drain_worker_log(
    reader: impl Read,
    mut writer: impl Write,
    sync: &File,
) -> std::io::Result<u64> {
    let bytes = std::io::copy(&mut std::io::BufReader::new(reader), &mut writer)?;
    writer.flush()?;
    sync.sync_all()?;
    Ok(bytes)
}
