use super::{FIXTURES, acquire};
use std::{sync::mpsc, thread, time::Duration};

#[test]
fn nested_fixture_guards_exclude_other_threads_until_last_drop() {
    let first = acquire();
    let second = acquire();
    assert_eq!(FIXTURES.lock().unwrap().as_ref().unwrap().1, 2);
    drop(first);
    let (blocked_tx, blocked_rx) = mpsc::channel();
    let (entered_tx, entered_rx) = mpsc::channel();
    let other = thread::spawn(move || {
        assert_ne!(
            FIXTURES.lock().unwrap().as_ref().unwrap().0,
            thread::current().id()
        );
        blocked_tx.send(()).unwrap();
        let guard = acquire();
        entered_tx.send(()).unwrap();
        drop(guard);
    });
    blocked_rx.recv_timeout(Duration::from_secs(300)).unwrap();
    assert!(matches!(
        entered_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    drop(second);
    entered_rx.recv_timeout(Duration::from_secs(300)).unwrap();
    other.join().unwrap();
    // A new fixture on this thread must be able to acquire after the last drop.
    drop(acquire());
}

#[cfg(unix)]
mod inheritance;
