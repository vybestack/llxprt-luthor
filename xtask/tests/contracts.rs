use std::path::Path;
use xtask::{
    contracts::{manifest, run},
    driver::{CommandSpec, Outcome, Runner},
};

struct Results {
    output: String,
    exit: i32,
    seen: Vec<CommandSpec>,
}
impl Runner for Results {
    fn run(&mut self, command: &CommandSpec) -> Result<Outcome, String> {
        self.seen.push(command.clone());
        Ok(Outcome {
            code: self.exit,
            stdout: self.output.clone(),
            stderr: String::new(),
        })
    }
}

#[test]
fn malformed_duplicate_and_empty_manifests_fail() {
    for input in [
        "[]",
        "not json",
        r#"[{"scenario":"x","target":"t","test":"x","extra":true}]"#,
        r#"[{"scenario":"x","target":"t","test":"x"},{"scenario":"y","target":"t","test":"x"}]"#,
    ] {
        assert!(manifest(input).is_err());
    }
}

#[test]
fn full_contract_stage_rejects_renamed_absent_ignored_filtered_skipped_or_failed_tests() {
    let contracts = manifest(
        r#"[{"scenario":"child stays gated","target":"supervisor","test":"child_gated"}]"#,
    )
    .unwrap();
    for (exit, output) in [
        (0, ""),
        (0, "running 0 tests\n"),
        (0, "test renamed ... ok\n"),
        (0, "test child_gated ... ignored\n"),
        (1, "test child_gated ... FAILED\n"),
        (19, ""),
    ] {
        let mut runner = Results {
            output: output.into(),
            exit,
            seen: vec![],
        };
        assert_eq!(
            run(Path::new("/fixture"), &contracts, &mut runner).unwrap(),
            exit.max(1)
        );
        assert_eq!(
            runner.seen[0].args,
            [
                "test",
                "-p",
                "luthor",
                "--test",
                "supervisor",
                "--all-features",
                "--offline",
                "--locked",
                "child_gated",
                "--",
                "--exact",
                "--test-threads=1"
            ]
        );
    }
}

#[test]
fn process_spawn_failure_is_not_a_successful_contract() {
    struct Broken;
    impl Runner for Broken {
        fn run(&mut self, _: &CommandSpec) -> Result<Outcome, String> {
            Err("spawn denied".into())
        }
    }
    let contracts = manifest(
        r#"[{"scenario":"child stays gated","target":"supervisor","test":"child_gated"}]"#,
    )
    .unwrap();
    assert_eq!(
        run(Path::new("/fixture"), &contracts, &mut Broken).unwrap_err(),
        "spawn denied"
    );
}
