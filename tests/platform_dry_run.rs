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
