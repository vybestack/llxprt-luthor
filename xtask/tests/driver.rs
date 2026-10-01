use std::path::Path;
use xtask::contracts::{Contract, verify};
use xtask::driver::{CommandSpec, Outcome, Runner, plan, run_commands};

#[derive(Default)]
struct Fake {
    seen: Vec<CommandSpec>,
    fail: Option<usize>,
}
impl Runner for Fake {
    fn run(&mut self, command: &CommandSpec) -> Result<Outcome, String> {
        self.seen.push(command.clone());
        Ok(Outcome {
            code: if self.fail == Some(self.seen.len()) {
                37
            } else {
                0
            },
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

#[test]
fn stage_inventory_arguments_directory_and_no_recursive_execution() {
    let root = Path::new("/fixture");
    let commands = plan(root);
    let args: Vec<Vec<&str>> = commands
        .iter()
        .map(|c| c.args.iter().map(String::as_str).collect())
        .collect();
    assert_eq!(
        args,
        vec![
            vec!["fmt", "--all", "--", "--check"],
            vec![
                "test",
                "-p",
                "xtask",
                "--all-targets",
                "--offline",
                "--locked"
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
                "clippy::type_complexity"
            ],
            vec![
                "build",
                "--workspace",
                "--all-targets",
                "--all-features",
                "--offline",
                "--locked"
            ],
            vec![
                "test",
                "--workspace",
                "--all-targets",
                "--all-features",
                "--offline",
                "--locked",
                "--",
                "--test-threads=1"
            ],
            vec![
                "test",
                "--workspace",
                "--doc",
                "--all-features",
                "--offline",
                "--locked"
            ],
        ]
    );
    assert!(
        commands
            .iter()
            .all(|c| c.cwd == root && c.program == "cargo")
    );
    let mut fake = Fake::default();
    assert_eq!(run_commands(&commands, &mut fake).unwrap(), 0);
    assert_eq!(fake.seen, commands);
}

#[test]
fn failed_stage_preserves_exit_and_stops_instead_of_skipping_to_green() {
    let commands = plan(Path::new("/fixture"));
    let mut fake = Fake {
        fail: Some(2),
        ..Fake::default()
    };
    assert_eq!(run_commands(&commands, &mut fake).unwrap(), 37);
    assert_eq!(fake.seen.len(), 2);
}

#[test]
fn required_contract_must_really_execute_and_pass() {
    let contract = Contract {
        scenario: "no unsafe launch".into(),
        target: "supervisor".into(),
        test: "registered_before_launch".into(),
    };
    let passed = "running 1 test\ntest registered_before_launch ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n";
    assert!(
        verify(
            &contract,
            &Outcome {
                code: 0,
                stdout: passed.into(),
                stderr: String::new()
            }
        )
        .is_ok()
    );
    for (code, output) in [
        (0, "running 0 tests\n"),
        (0, "test renamed ... ok\n"),
        (0, "test registered_before_launch ... ignored\n"),
        (1, passed),
        (0, "test registered_before_launch ... FAILED\n"),
        (0, "test registered_before_launch ... ok\n"),
    ] {
        let error = verify(
            &contract,
            &Outcome {
                code,
                stdout: output.into(),
                stderr: String::new(),
            },
        )
        .unwrap_err();
        assert!(error.contains("no unsafe launch"));
    }
}

#[test]
fn exact_contract_can_filter_other_tests_but_not_the_required_test() {
    let contract = Contract {
        scenario: "safety".into(),
        target: "supervisor".into(),
        test: "required".into(),
    };
    let output = "test required ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 65 filtered out; finished in 0.01s\n";
    assert!(
        verify(
            &contract,
            &Outcome {
                code: 0,
                stdout: output.into(),
                stderr: String::new()
            }
        )
        .is_ok()
    );
}

#[test]
fn workflow_requires_both_platforms_same_command_and_fail_on_skips() {
    let yaml = include_str!("../../.github/workflows/ci.yml");
    assert!(yaml.contains("os: [ubuntu-latest, macos-latest]"));
    assert!(yaml.contains("run: cargo xtask ci"));
    assert!(yaml.contains("if: ${{ always() }}"));
    assert!(yaml.contains("needs: [gates]"));
    assert!(yaml.contains("run: test \"$GATES_RESULT\" = success"));
    assert!(!yaml.contains("continue-on-error"));
    for alternate in [
        "cargo test",
        "cargo clippy",
        "cargo fmt",
        "cargo build",
        "cargo xtask policy",
    ] {
        assert!(!yaml.contains(alternate));
    }
}
