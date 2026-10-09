use luthor::platform::{ProcessStatError, parse_linux_process_stat};

fn stat_fixture(name: &str, state: char, start: &str) -> String {
    let mut fields = vec![state.to_string()];
    fields.extend((0..18).map(|n| n.to_string()));
    fields.push(start.to_owned());
    format!("42 ({name}) {}\n", fields.join(" "))
}

#[test]
fn linux_process_observation_parses_start_ticks_after_parenthesized_command() {
    let stat = stat_fixture("worker ) with spaces", 'S', "987654");
    let observed = parse_linux_process_stat(&stat).unwrap();
    assert_eq!(observed.state, 'S');
    assert_eq!(observed.start_time_ticks, "987654");
}

#[test]
fn linux_process_observation_rejects_missing_identity_fields() {
    assert_eq!(
        parse_linux_process_stat("42 (worker) S"),
        Err(ProcessStatError::Malformed)
    );
    assert_eq!(
        parse_linux_process_stat("worker S 1 2 3"),
        Err(ProcessStatError::Malformed)
    );
}

#[cfg(target_os = "macos")]
mod platform_dry_run {
    use luthor::{
        config::{CommandTemplate, Config, Mapping, Marker, Source},
        eligibility::Candidate,
        state::StateStore,
        supervisor::{ExitReceipt, execute_with_binary, prepare_initial, prepare_resume},
        worktree::ensure_worktree,
    };
    use std::{
        fs,
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        path::{Path, PathBuf},
        process::Command,
        thread,
        time::{Duration, Instant},
    };
    use tempfile::{TempDir, tempdir};
    mod diagnostics;
    mod fixture;
    pub(super) mod initial;
    mod provider;
    pub(super) mod stop;
    use diagnostics::{
        bounded_sanitized_output, report_stop_request_failure, stream_tail, wait_for_stop_receipt,
    };
    use fixture::{NativeFixture, claimed_store, fixture_config};
    use provider::start_provider;
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires an installed LLxprt rs binary; set LUTHOR_RS_BINARY"]
fn installed_rs_initial_turn_uses_private_config_and_loopback_provider() {
    platform_dry_run::initial::run();
}
#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires an installed LLxprt rs binary; set LUTHOR_RS_BINARY"]
fn installed_rs_stop_uses_private_supervisor_and_reconciles() {
    platform_dry_run::stop::run();
}
