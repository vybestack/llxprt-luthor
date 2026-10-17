mod continuation;
mod dependencies;
mod dispatch;
mod operator;
mod recovery;
mod requests;
mod resume;
mod support;

pub fn run(mut args: impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let command = args.next();
    if matches!(command.as_deref(), Some("__supervise" | "__worker_gate")) {
        return internal_command(command.as_deref(), args);
    }
    match command.as_deref() {
        Some(command @ ("status" | "show" | "logs")) => {
            operator::operator(std::iter::once(command.to_owned()).chain(args))
        }
        Some(command @ ("pause" | "reconcile")) => operator::mutate(command, args.collect()),
        Some("discover") => operator::discover(args.collect()),
        Some("daemon") => crate::daemon::run(&args.collect::<Vec<_>>()),
        Some("dispatch") => dispatch::dispatch(args.collect()),
        Some("resume") => resume::resume(args.collect()),
        Some("resize-capacity") => crate::cli::capacity::run(args.collect()),
        Some("repin") => crate::cli::repin::run(args.collect()),
        Some("adopt-pin") => crate::cli::pin_adoption::run(args.collect()),
        Some("retry") => recovery::retry(args.collect()),
        Some("recover") => recovery::recover(args.collect()),
        Some("abandon") => recovery::abandon(args.collect()),
        Some("continue-undispatched") => continuation::continue_undispatched(args.collect()),
        Some("continue-amended") => crate::cli::native_continuation::run(args.collect()),
        Some("amend-undispatched") => crate::cli::amend_undispatched(args.collect()),
        Some("--help" | "-h") => {
            println!(
                "Usage: luthor continue-amended --task TASK --predecessor ID --attempt ID --mode stopped-resume|natural-exit-retry --config PATH --config-revision REV --actor LOGIN --execute\n       luthor discover --config <path>\n       luthor daemon --config PATH --config-revision REV [--repository owner/repo --issues N,N,...] [--once] [--execute]\n       luthor dispatch --config <path> --repository owner/repo --issue N --config-revision REV [--execute]\n       luthor resume TASK --config <path> --execute\n       luthor retry TASK --attempt ID --config PATH --config-revision REV --actor LOGIN --reason TEXT [--revalidate-terminal-exit] --execute\n       luthor repin TASK --config PATH --executable PATH --sha256 HEX --actor LOGIN --reason TEXT [--execute]\n       luthor adopt-pin TASK --config PATH --executable PATH --sha256 HEX --actor LOGIN --reason TEXT [--execute]\n       luthor recover TASK --attempt ID --config PATH --actor LOGIN --reason TEXT --execute\n       luthor abandon TASK --config PATH --actor LOGIN --reason TEXT --execute\n       luthor continue-undispatched --task TASK --attempt ID --config PATH --config-revision REV --actor LOGIN --reason legacy_preflight_recovery --execute\n       luthor amend-undispatched --task TASK --attempt ID --config PATH --config-revision REV --actor LOGIN --reason native_initial_branch_is_conversation --execute\n       luthor resize-capacity --config PATH --expected-capacity N --expected-reservations N --actor LOGIN --reason TEXT --execute\n       luthor status --config <path>\n       luthor show TASK --config <path>\n       luthor logs TASK [--attempt ATTEMPT] --config <path>"
            );
            Ok(())
        }
        _ => {
            Err("expected `discover`, `daemon`, `dispatch`, `resume`, `retry`, `adopt-pin`, `repin`, `recover`, `abandon`, `continue-undispatched`, `resize-capacity`, or `amend-undispatched`".into())
        }
    }
}

fn internal_command(
    command: Option<&str>,
    mut args: impl Iterator<Item = String>,
) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        Some("__supervise") => {
            let root = args.next().ok_or("missing state root")?;
            let attempt = args.next().ok_or("missing attempt id")?;
            if args.next().is_some() {
                return Err("unexpected argument".into());
            }
            crate::supervisor::supervise(std::path::Path::new(&root), &attempt)?;
            Ok(())
        }
        #[cfg(unix)]
        Some("__worker_gate") => {
            let plan = args.next().ok_or("missing gate plan")?;
            if args.next().is_some() {
                return Err("unexpected argument".into());
            }
            crate::supervisor::worker_gate(std::path::Path::new(&plan))?;
            Ok(())
        }
        _ => Err("invalid internal command".into()),
    }
}
