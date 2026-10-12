use luthor::{OwnershipError, WorktreeOwner};
use std::{
    fs,
    io::{Read, Write},
    os::{fd::AsRawFd, unix::fs::PermissionsExt, unix::net::UnixStream, unix::process::CommandExt},
    process::Command,
    thread,
    time::Duration,
};

fn owner_fd(owner: &WorktreeOwner) -> i32 {
    let mut command = Command::new("/bin/sh");
    owner.inherit_into(&mut command);
    command
        .get_envs()
        .find(|(key, _)| *key == "LUTHOR_WORKTREE_OWNER_FD")
        .unwrap()
        .1
        .unwrap()
        .to_str()
        .unwrap()
        .parse()
        .unwrap()
}

fn gated_spawn(
    intended: &WorktreeOwner,
    unrelated_fd: i32,
    child_gate: UnixStream,
    parent_fd: i32,
) -> thread::JoinHandle<std::process::ExitStatus> {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "exit 0"]);
    intended.inherit_into(&mut command);
    let socket = child_gate.as_raw_fd();
    unsafe {
        command.pre_exec(move || {
            // Only async-signal-safe syscalls between fork and exec. Report the
            // exact PID, fd, CLOEXEC flag and inode retained by this child.
            if libc::close(parent_fd) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            let mut stat: libc::stat = std::mem::zeroed();
            if libc::fstat(unrelated_fd, &mut stat) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            #[cfg(target_os = "macos")]
            let dev = stat.st_dev as u64;
            #[cfg(not(target_os = "macos"))]
            let dev = stat.st_dev;
            let identity = [
                libc::getpid() as u64,
                unrelated_fd as u64,
                libc::fcntl(unrelated_fd, libc::F_GETFD) as u64,
                dev,
                stat.st_ino,
            ];
            if libc::write(socket, identity.as_ptr().cast(), 40) != 40 {
                return Err(std::io::Error::last_os_error());
            }
            let mut release = 0u8;
            if libc::read(socket, (&raw mut release).cast(), 1) != 1 || release != b'R' {
                return Err(std::io::Error::other(
                    "pre_exec gate closed without release",
                ));
            }
            Ok(())
        });
    }
    thread::spawn(move || {
        let mut child = command.spawn().unwrap();
        drop(child_gate);
        child.wait().unwrap()
    })
}

#[test]
fn unrelated_cloexec_owner_is_retained_by_fork_child_until_exec() {
    let _isolation = super::super::acquire();
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let unrelated = WorktreeOwner::acquire(dir.path(), "unrelated").unwrap();
    let protocol = serde_json::to_value(
        unrelated
            .protocol_evidence(dir.path(), "unrelated", "attempt")
            .unwrap(),
    )
    .unwrap();
    let intended = WorktreeOwner::acquire(dir.path(), "intended").unwrap();
    let fd = owner_fd(&unrelated);
    let (mut parent, child) = UnixStream::pair().unwrap();
    parent
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let spawned = gated_spawn(&intended, fd, child, parent.as_raw_fd());
    let mut bytes = [0u8; 40];
    parent.read_exact(&mut bytes).unwrap();
    let identity: Vec<u64> = bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|part| u64::from_ne_bytes(*part))
        .collect();
    assert_ne!(identity[0], std::process::id() as u64);
    assert_eq!(identity[1], fd as u64);
    assert_eq!(
        identity[2] & libc::FD_CLOEXEC as u64,
        libc::FD_CLOEXEC as u64
    );
    assert_eq!(identity[3], protocol["dev"].as_u64().unwrap());
    assert_eq!(identity[4], protocol["ino"].as_u64().unwrap());
    eprintln!("pre_exec retained unrelated owner: pid/fd/flags/dev/ino={identity:?}");
    drop(unrelated);
    assert!(matches!(
        WorktreeOwner::acquire_existing(dir.path(), "unrelated"),
        Err(OwnershipError::Busy)
    ));
    parent.write_all(b"R").unwrap();
    assert!(spawned.join().unwrap().success());
    drop(WorktreeOwner::acquire_existing(dir.path(), "unrelated").unwrap());
    // The unrelated descriptor closes at exec; intentional owner exclusion is
    // not weakened by the fixture guard or an explicit unlock-on-drop.
    assert!(matches!(
        WorktreeOwner::acquire_existing(dir.path(), "intended"),
        Err(OwnershipError::Busy)
    ));
    drop(intended);
    drop(WorktreeOwner::acquire_existing(dir.path(), "intended").unwrap());
}
