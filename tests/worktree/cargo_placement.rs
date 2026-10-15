//! Executable examples of issue #33's placement contract. Git identity alone
//! cannot isolate Cargo's workspace discovery or shorten worker socket paths.
use crate::*;
use std::process::Output;

const MANIFEST: &str =
    "[package]\nname = \"placement-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n";

fn cargo_fixture() -> Fixture {
    let f = Fixture::new();
    let checkout = &f.config.mappings[0].checkout;
    fs::create_dir(checkout.join("src")).unwrap();
    fs::write(checkout.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    fs::write(checkout.join("Cargo.toml"), MANIFEST).unwrap();
    fs::write(
        checkout.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"placement-fixture\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    git(checkout, &["add", "src/lib.rs", "Cargo.toml", "Cargo.lock"]);
    git(checkout, &["commit", "-m", "add standalone Cargo package"]);
    git(
        checkout,
        &[
            "push",
            f.dir.path().join("remote.git").to_str().unwrap(),
            "main",
        ],
    );
    let enclosing = f.dir.path().join("unrelated-workspace");
    fs::create_dir(&enclosing).unwrap();
    fs::write(
        enclosing.join("Cargo.toml"),
        "[workspace]\nmembers = []\nresolver = \"3\"\n",
    )
    .unwrap();
    f
}

fn cargo(f: &Fixture, worktree: &Path, args: &[&str]) -> Output {
    // Bound discovery to this synthetic installation, not the real Luthor
    // workspace containing test tmp/. Both layouts get the identical boundary;
    // the unrelated workspace below it must still break the nested layout.
    Command::new(env!("CARGO"))
        .args(args)
        .env("CARGO_HOME", fs::canonicalize(f.dir.path()).unwrap())
        .current_dir(worktree)
        .output()
        .unwrap()
}

fn create(f: &mut Fixture) -> luthor::state::WorktreeIdentity {
    f.task("task-cargo", 33, true);
    luthor::worktree::preflight("task-cargo", &f.config.worktree_root, &f.config.mappings[0])
        .unwrap();
    let WorktreeResult::Created(identity) = f.create("task-cargo").unwrap() else {
        panic!("expected a new task worktree")
    };
    verify_snapshot(&identity).unwrap();
    identity
}

fn assert_manifest_unchanged(path: &Path) {
    assert_eq!(
        fs::read_to_string(path.join("Cargo.toml")).unwrap(),
        MANIFEST
    );
    assert_eq!(git(path, &["status", "--porcelain=v1"]), "");
}

#[test]
fn nested_workspace_reproduces_issue33_despite_verified_git_worktree() {
    let mut f = cargo_fixture();
    f.config.worktree_root = f.dir.path().join("unrelated-workspace/tmp/w");
    let identity = create(&mut f);
    for args in [
        vec!["locate-project", "--workspace"],
        vec![
            "metadata",
            "--offline",
            "--locked",
            "--no-deps",
            "--format-version",
            "1",
        ],
    ] {
        let output = cargo(&f, &identity.path, &args);
        assert!(!output.status.success(), "{args:?} unexpectedly succeeded");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("current package believes it's in a workspace when it's not"),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_manifest_unchanged(&identity.path);
}

#[test]
fn separate_short_root_keeps_target_workspace_and_worker_fixtures_usable() {
    let mut f = cargo_fixture();
    // A sibling of the unrelated workspace, not a descendant of it.
    f.config.worktree_root = f.dir.path().join("w");
    let identity = create(&mut f);
    let output = cargo(&f, &identity.path, &["locate-project", "--workspace"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let located: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        located["root"].as_str().unwrap(),
        identity.path.join("Cargo.toml").to_str().unwrap()
    );
    let output = cargo(
        &f,
        &identity.path,
        &[
            "metadata",
            "--offline",
            "--locked",
            "--no-deps",
            "--format-version",
            "1",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        metadata["workspace_root"].as_str().unwrap(),
        identity.path.to_str().unwrap()
    );
    #[cfg(unix)]
    relative_worker_socket(&identity.path);
    assert_manifest_unchanged(&identity.path);
}

#[cfg(unix)]
fn relative_worker_socket(worktree: &Path) {
    use std::os::unix::net::{UnixListener, UnixStream};
    let tmp = worktree.join("tmp");
    fs::create_dir(&tmp).unwrap();
    let socket = tmp
        .strip_prefix(std::env::current_dir().unwrap())
        .unwrap()
        .join("fixture.sock");
    // Keep the process cwd stable and exercise both bind and connect through
    // the bounded relative path, even when the CI checkout itself is long.
    let listener = UnixListener::bind(&socket).unwrap();
    let stream = UnixStream::connect(&socket).unwrap();
    let (_peer, _) = listener.accept().unwrap();
    drop(stream);
    drop(listener);
    fs::remove_file(socket).unwrap();
    fs::remove_dir(tmp).unwrap();
}
