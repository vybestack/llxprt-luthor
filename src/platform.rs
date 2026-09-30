use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxProcessStat {
    pub state: char,
    pub start_time_ticks: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProcessStatError {
    #[error("process stat is malformed")]
    Malformed,
}

/// Parse Linux `/proc/<pid>/stat`; the command name is parenthesized and may
/// itself contain spaces or closing parentheses.
pub fn parse_linux_process_stat(stat: &str) -> Result<LinuxProcessStat, ProcessStatError> {
    let (_, fields) = stat.rsplit_once(')').ok_or(ProcessStatError::Malformed)?;
    let fields = fields.split_whitespace().collect::<Vec<_>>();
    let state = fields
        .first()
        .and_then(|value| value.chars().next())
        .ok_or(ProcessStatError::Malformed)?;
    let start_time_ticks = fields
        .get(19)
        .filter(|value| !value.is_empty())
        .ok_or(ProcessStatError::Malformed)?;
    Ok(LinuxProcessStat {
        state,
        start_time_ticks: (*start_time_ticks).to_owned(),
    })
}

#[cfg(target_os = "linux")]
pub fn observe_linux_process(pid: u32) -> Result<LinuxProcessStat, std::io::Error> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    parse_linux_process_stat(&stat)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "malformed process stat"))
}
