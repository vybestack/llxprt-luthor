use std::{collections::VecDeque, path::Path};
use xtask::{
    contracts::manifest,
    driver::{CommandSpec, Outcome, Runner},
};

struct Results {
    outcomes: VecDeque<Outcome>,
    commands: Vec<CommandSpec>,
}
impl Runner for Results {
    fn run(&mut self, command: &CommandSpec) -> Result<Outcome, String> {
        self.commands.push(command.clone());
        self.outcomes
            .pop_front()
            .ok_or("unexpected extra command".into())
    }
}
fn outcome(code: i32, stdout: &str) -> Outcome {
    Outcome {
        code,
        stdout: stdout.into(),
        stderr: String::new(),
    }
}

#[test]
fn aggregate_cannot_succeed_when_required_behavior_is_removed_renamed_ignored_filtered_skipped_or_failed()
 {
    let root = Path::new("/fixture");
    let contracts = manifest(r#"[{"scenario":"no worker before registration","target":"supervisor","test":"registered"}]"#).unwrap();
    for (code, text) in [
        (0, ""),
        (
            0,
            "test renamed ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;",
        ),
        (
            0,
            "test registered ... ignored\ntest result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out;",
        ),
        (
            0,
            "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out;",
        ),
        (17, "test registered ... FAILED"),
    ] {
        let stages = xtask::driver::plan(root);
        let mut outcomes: VecDeque<_> = stages.iter().map(|_| outcome(0, "")).collect();
        outcomes.push_back(outcome(code, text));
        let mut runner = Results {
            outcomes,
            commands: vec![],
        };
        assert_eq!(
            xtask::ci::run(root, &contracts, &mut runner).unwrap(),
            code.max(1)
        );
        assert_eq!(&runner.commands[..stages.len()], stages);
        assert!(
            runner
                .commands
                .last()
                .unwrap()
                .args
                .contains(&"registered".into())
        );
        assert!(runner.outcomes.is_empty());
    }
}

#[test]
fn aggregate_propagates_stage_failure_without_running_contracts_and_requires_successful_execution()
{
    let root = Path::new("/fixture");
    let contracts =
        manifest(r#"[{"scenario":"durable claim","target":"claim","test":"durable"}]"#).unwrap();
    let mut failed = Results {
        outcomes: VecDeque::from([outcome(31, "format failed")]),
        commands: vec![],
    };
    assert_eq!(xtask::ci::run(root, &contracts, &mut failed).unwrap(), 31);
    assert_eq!(failed.commands.len(), 1);
    let mut outcomes: VecDeque<_> = xtask::driver::plan(root)
        .iter()
        .map(|_| outcome(0, ""))
        .collect();
    outcomes.push_back(outcome(0, "test durable ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;"));
    let mut passed = Results {
        outcomes,
        commands: vec![],
    };
    assert_eq!(xtask::ci::run(root, &contracts, &mut passed).unwrap(), 0);
    assert!(passed.outcomes.is_empty());
}
