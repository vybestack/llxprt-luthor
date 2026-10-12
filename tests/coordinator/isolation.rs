//! Process-wide isolation for fixtures that mix short-lived owners and fork.
//! CLOEXEC closes unrelated owners at exec, not at fork: another test's
//! pre_exec child can keep an already-dropped owner busy until it reaches exec.
//! Keep the guard for the entire fixture lifetime, including real launches.
use std::{
    marker::PhantomData,
    rc::Rc,
    sync::{Condvar, Mutex},
    thread::{self, ThreadId},
};

static FIXTURES: Mutex<Option<(ThreadId, usize)>> = Mutex::new(None);
static RELEASED: Condvar = Condvar::new();

// Some scenarios keep multiple fixtures alive on one harness thread. Count
// nested guards rather than recursively locking or admitting other threads.
pub(crate) struct Guard {
    _thread_bound: PhantomData<Rc<()>>,
}

pub(crate) fn acquire() -> Guard {
    let id = thread::current().id();
    let mut active = FIXTURES.lock().unwrap();
    while active.as_ref().is_some_and(|(owner, _)| *owner != id) {
        active = RELEASED.wait(active).unwrap();
    }
    let count = active.as_ref().map_or(0, |(_, count)| *count);
    *active = Some((id, count + 1));
    Guard {
        _thread_bound: PhantomData,
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let mut active = FIXTURES.lock().unwrap();
        let (owner, count) = active.as_mut().unwrap();
        assert_eq!(*owner, thread::current().id());
        *count -= 1;
        if *count == 0 {
            *active = None;
            RELEASED.notify_one();
        }
    }
}

#[cfg(test)]
mod tests;
