mod continuation;
mod dispatch;
mod operator;
mod recovery;
mod resume;
mod support;

pub fn run(mut args: impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    match args.next().as_deref() {
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
        Some(command @ ("status" | "show" | "logs")) => {
            operator::operator(std::iter::once(command.to_owned()).chain(args))
        }
        Some(command @ ("pause" | "reconcile")) => operator::mutate(command, args.collect()),
        Some("discover") => operator::discover(args.collect()),
        Some("daemon") => crate::daemon::run(&args.collect::<Vec<_>>()),
        Some("dispatch") => dispatch::dispatch(args.collect()),
        Some("resume") => resume::resume(args.collect()),
        Some("retry") => recovery::retry(args.collect()),
        Some("recover") => recovery::recover(args.collect()),
        Some("continue-undispatched") => continuation::continue_undispatched(args.collect()),
        Some("amend-undispatched") => crate::cli::amend_undispatched(args.collect()),
        Some("--help" | "-h") => {
            println!(
                "Usage: luthor discover --config <path>\n       luthor daemon --config PATH --config-revision REV [--repository owner/repo --issues N,N,...] [--once] [--execute]\n       luthor dispatch --config <path> --repository owner/repo --issue N --config-revision REV [--execute]\n       luthor resume TASK --config <path> --execute\n       luthor retry TASK --attempt ID --config PATH --config-revision REV --actor LOGIN --reason TEXT [--revalidate-terminal-exit] --execute\n       luthor recover TASK --attempt ID --config PATH --actor LOGIN --reason TEXT --execute\n       luthor continue-undispatched --task TASK --attempt ID --config PATH --config-revision REV --actor LOGIN --reason legacy_preflight_recovery --execute\n       luthor amend-undispatched --task TASK --attempt ID --config PATH --config-revision REV --actor LOGIN --reason native_initial_branch_is_conversation --execute\n       luthor status --config <path>\n       luthor show TASK --config <path>\n       luthor logs TASK [--attempt ATTEMPT] --config <path>"
            );
            Ok(())
        }
        _ => {
            Err("expected `discover`, `daemon`, `dispatch`, `resume`, `retry`, `recover`, `continue-undispatched`, or `amend-undispatched`".into())
        }
    }
}
