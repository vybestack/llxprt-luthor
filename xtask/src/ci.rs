//! Execute the mandatory stages and exact behavioral contracts using one runner.
use crate::{
    contracts::{self, Contract},
    driver::{Runner, plan, run_commands},
};
use std::path::Path;

pub fn run(root: &Path, contracts: &[Contract], runner: &mut impl Runner) -> Result<i32, String> {
    let code = run_commands(&plan(root), runner)?;
    if code != 0 {
        return Ok(code);
    }
    contracts::run(root, contracts, runner)
}
