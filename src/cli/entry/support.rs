use std::{fs, io::Read};

pub(crate) fn option(args: &[String], name: &str) -> Result<String, Box<dyn std::error::Error>> {
    let positions: Vec<_> = args
        .iter()
        .enumerate()
        .filter(|(_, arg)| *arg == name)
        .collect();
    if positions.len() != 1 {
        return Err(format!("requires exactly one {name}").into());
    }
    let index = positions[0].0;
    args.get(index + 1)
        .filter(|v| !v.starts_with('-'))
        .cloned()
        .ok_or_else(|| format!("requires value for {name}").into())
}

pub(crate) use crate::coordinator::operator_error as safe_error_stage;

pub(crate) fn random_id() -> Result<String, std::io::Error> {
    let mut bytes = [0u8; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
