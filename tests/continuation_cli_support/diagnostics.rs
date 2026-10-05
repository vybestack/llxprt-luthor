use super::Fixture;
use std::{
    io::Read,
    process::{Child, Command, Stdio},
};

struct Blocker(Child);
impl Drop for Blocker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn child_probe_file_reaches_parent_panic_without_changing_cli_stderr() {
    let f = Fixture::new();
    f.correct_storage();
    let child = Command::new("/bin/sh")
        .args(["-c", "printf 'ready\\n'; read line", &f.task])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut blocker = Blocker(child);
    let mut ready = [0; 6];
    blocker
        .0
        .stdout
        .as_mut()
        .unwrap()
        .read_exact(&mut ready)
        .unwrap();
    assert_eq!(&ready, b"ready\n");
    let out = f.run(&f.args());
    drop(blocker);
    assert!(!out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "luthor: continuation refused or held\n"
    );
    let record = f.process_diagnostic();
    assert!(record.starts_with("process_probe stage="), "{record}");
    assert!(!record.contains("record=missing"), "{record}");
    assert!(record.contains(" pid="));
    assert!(record.contains(" uid="));
    assert!(record.contains(" state="));
    assert!(record.contains(" errno="));
    assert!(!record.contains(&f.task));
    assert!(!record.contains('/'));
    assert!(record.len() < 160);
    assert_eq!(record.lines().count(), 1);
    let panic = std::panic::catch_unwind(|| panic!("inspector unavailable: {record}"));
    let message = panic.unwrap_err().downcast::<String>().unwrap();
    assert!(message.contains(&record));
}
