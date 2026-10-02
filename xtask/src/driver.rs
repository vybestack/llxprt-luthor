use std::{
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}
#[derive(Debug)]
pub struct Outcome {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}
pub trait Runner {
    fn run(&mut self, command: &CommandSpec) -> Result<Outcome, String>;
}
pub struct Processes;

impl Runner for Processes {
    fn run(&mut self, spec: &CommandSpec) -> Result<Outcome, String> {
        eprintln!("gate: {} {}", spec.program, spec.args.join(" "));
        let output = Command::new(&spec.program)
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .output()
            .map_err(|e| format!("{}: {e}", spec.program))?;
        let stdout = String::from_utf8(output.stdout)
            .map_err(|e| format!("invalid subprocess stdout: {e}"))?;
        let stderr = String::from_utf8(output.stderr)
            .map_err(|e| format!("invalid subprocess stderr: {e}"))?;
        print!("{stdout}");
        eprint!("{stderr}");
        Ok(Outcome {
            code: output.status.code().unwrap_or(1),
            stdout,
            stderr,
        })
    }
}

pub fn cargo(root: &Path, args: &[&str]) -> CommandSpec {
    CommandSpec {
        program: "cargo".into(),
        args: args.iter().map(|s| (*s).into()).collect(),
        cwd: root.into(),
    }
}

pub fn plan(root: &Path) -> Vec<CommandSpec> {
    [
        vec!["fmt", "--all", "--", "--check"],
        vec![
            "test",
            "-p",
            "xtask",
            "--all-targets",
            "--offline",
            "--locked",
        ],
        vec![
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--offline",
            "--locked",
            "--",
            "-D",
            "warnings",
            "-D",
            "clippy::cognitive_complexity",
            "-D",
            "clippy::type_complexity",
        ],
        vec![
            "build",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--offline",
            "--locked",
        ],
        vec![
            "test",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--offline",
            "--locked",
            "--",
            "--test-threads=1",
        ],
        vec![
            "test",
            "--workspace",
            "--doc",
            "--all-features",
            "--offline",
            "--locked",
        ],
    ]
    .iter()
    .map(|args| cargo(root, args))
    .collect()
}

pub fn run_commands(commands: &[CommandSpec], runner: &mut impl Runner) -> Result<i32, String> {
    for command in commands {
        let result = runner.run(command)?;
        if result.code != 0 {
            return Ok(result.code);
        }
    }
    Ok(0)
}
