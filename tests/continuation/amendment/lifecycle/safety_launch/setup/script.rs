use super::super::super::Lane;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

pub(crate) fn private(path: &Path) {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn executable(path: &Path, text: String) -> PathBuf {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    path.to_owned()
}
fn mutation(lane: &Lane, change: &str) -> String {
    let path = &lane.plan.worktree;
    match change {
        "clean" => ":".into(),
        "tracked" => format!("printf drift > '{}/README'", path.display()),
        "untracked" => format!("printf drift > '{}/late-untracked'", path.display()),
        "descendant" => format!(
            "git -C '{}' commit --allow-empty -m late-drift >&2",
            path.display()
        ),
        _ => unreachable!(),
    }
}

pub(crate) fn ready_proxy(lane: &Lane, change: &str) -> PathBuf {
    let dir = lane.f.dir.path();
    executable(
        &dir.join("ready-proxy"),
        format!(
            "#!/bin/sh\nexec 3<&0\nmkfifo '{0}/ready-output'\n'{1}' \"$@\" <&3 > '{0}/ready-output' &\npid=$!\nIFS= read -r ready < '{0}/ready-output'\n[ \"$ready\" = READY ] || exit 90\n{2}\nprintf 'READY\\n'\nwait $pid\nprintf done > '{0}/proxy-done'\n",
            dir.display(),
            env!("CARGO_BIN_EXE_luthor"),
            mutation(lane, change)
        ),
    )
}

pub(crate) fn await_proxy(lane: &Lane) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !lane.f.dir.path().join("proxy-done").exists() {
        assert!(Instant::now() < deadline, "READY proxy must terminate");
        thread::sleep(Duration::from_millis(10));
    }
}

pub(crate) fn worker_shim(lane: &Lane, change: &str) -> PathBuf {
    let gate = lane.f.dir.path().join("worker-release");
    fs::write(&gate, b"R").unwrap();
    executable(
        &lane.f.dir.path().join("worker-shim"),
        format!(
            "#!/bin/sh\ndd bs=1 count=1 of=/dev/null 2>/dev/null\n{0}\nexec '{1}' \"$@\" < '{2}'\n",
            mutation(lane, change),
            env!("CARGO_BIN_EXE_luthor"),
            gate.display()
        ),
    )
}
