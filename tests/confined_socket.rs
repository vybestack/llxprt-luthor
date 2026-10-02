#![cfg(unix)]

use luthor::supervisor::validate_stop_socket_path;
use std::{
    env, fs,
    os::unix::net::{UnixListener, UnixStream},
};

#[test]
fn confined_relative_socket_connects_without_accepting_overlong_absolute_paths() {
    let dir = tempfile::tempdir().unwrap();
    let relative = dir
        .path()
        .strip_prefix(env::current_dir().unwrap())
        .unwrap()
        .join("state");
    fs::create_dir(&relative).unwrap();
    let socket = relative.join("control.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let stream = UnixStream::connect(&socket).unwrap();
    let (_peer, _) = listener.accept().unwrap();
    assert!(validate_stop_socket_path(&relative, "attempt-a").is_ok());
    assert!(validate_stop_socket_path(&dir.path().join("x".repeat(200)), "attempt-a").is_err());
    drop(stream);
    drop(listener);
    fs::remove_file(socket).unwrap();
}
