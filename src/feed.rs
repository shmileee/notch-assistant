//! One notifying integration's data in the panel (GitHub, Linear): the latest fetch or the
//! mock, what you've read of it, which unread events were announced, and the thread fetching
//! it.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::time::Duration;

use crate::oncall::Origin;
use crate::read::{Announced, ReadState};

/// Fetched data says where it came from, so a failed refresh can keep it and say so.
pub trait Fetched { fn origin(&mut self) -> &mut Origin; }

pub struct Feed<T> {
    pub data: T,
    pub read: ReadState,
    pub announced: Announced,
    results: Option<Receiver<Result<T, String>>>,
    /// Keeps the thread alive; dropping it stops the thread, sending on it refreshes now.
    thread: Option<Sender<()>>,
}

/// What `Feed::take` took in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Took {
    /// Anything arrived, a failure included.
    pub changed: bool,
    /// A good fetch arrived.
    pub fresh: bool,
}

impl<T: Fetched + Send + 'static> Feed<T> {
    pub fn new(data: T, read: ReadState) -> Self {
        Feed { data, read, announced: Announced::default(), results: None, thread: None }
    }

    /// Shows `data` with no thread: the mock, or a scene.
    pub fn show(&mut self, data: T, read: ReadState) { *self = Feed::new(data, read); }

    /// Calls `fetch` now and every `every` on a thread named `name`, waking the UI with each
    /// result. Stops any earlier thread.
    pub fn start(&mut self, name: &'static str, every: Duration, fetch: impl FnMut() -> Result<T, String> + Send + 'static, wake: rune::Waker, loading: T, read: ReadState) {
        read.save();
        self.show(loading, read);
        let (tx, rx) = channel();
        self.thread = Some(self::every(name, every, fetch, tx, wake));
        self.results = Some(rx);
    }

    /// Fetches now, if a thread is fetching.
    pub fn refresh(&self) {
        if let Some(thread) = &self.thread { let _ = thread.send(()); }
    }

    /// Takes in finished fetches, handing each good one to `arrived` first. A failure keeps the
    /// last good data and says so.
    pub fn take(&mut self, mut arrived: impl FnMut(&mut ReadState, &T)) -> Took {
        let mut took = Took::default();
        let mut stopped = false;
        let Some(rx) = &self.results else { return took };
        loop {
            match rx.try_recv() {
                Ok(Ok(data)) => {
                    arrived(&mut self.read, &data);
                    self.data = data;
                    took.fresh = true;
                }
                Ok(Err(message)) => fail(self.data.origin(), message),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => { stopped = true; break; }
            }
            took.changed = true;
        }
        if stopped {
            (self.results, self.thread) = (None, None);
            fail(self.data.origin(), "Refreshing stopped. Save this integration in Settings to start it again".into());
            took.changed = true;
        }
        took
    }
}

/// Marks data as stale after a failed refresh, keeping when it was last good.
pub fn fail(origin: &mut Origin, message: String) {
    let stale = match origin { Origin::Live { at } => Some(*at), Origin::Error { stale, .. } => *stale, _ => None };
    *origin = Origin::Error { message, stale };
}

/// How long to wait after a failed fetch: soon at first, then longer, never past the usual wait.
pub fn retry_after(failures: u32, every: Duration) -> Duration {
    Duration::from_secs(15).saturating_mul(1 << failures.saturating_sub(1).min(8)).min(every)
}

/// Calls `fetch` now and every `every` on a thread named `name`, sooner after a failure, sending
/// each result and waking the UI. Dropping the returned sender stops the thread; sending on it
/// fetches now.
pub fn every<T: Send + 'static>(name: &'static str, every: Duration, mut fetch: impl FnMut() -> Result<T, String> + Send + 'static,
    tx: Sender<Result<T, String>>, wake: rune::Waker) -> Sender<()> {
    let (control, commands) = channel::<()>();
    std::thread::Builder::new().name(name.into()).spawn(move || {
        let mut failures = 0;
        loop {
            let result = catch_unwind(AssertUnwindSafe(&mut fetch)).unwrap_or_else(|_| Err("Refreshing failed unexpectedly".into()));
            let wait = match &result {
                Ok(_) => { failures = 0; every }
                Err(e) => { eprintln!("{name}: {e}"); failures += 1; retry_after(failures, every) }
            };
            if tx.send(result).is_err() { return; }
            wake();
            match commands.recv_timeout(wait) {
                Ok(()) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }).expect("spawn a fetching thread");
    control
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pulls::Pulls;

    fn fed(results: Vec<Result<Pulls, String>>, data: Pulls) -> Feed<Pulls> {
        let (tx, rx) = channel();
        for r in results { tx.send(r).unwrap(); }
        let mut feed = Feed::new(data, ReadState::memory(chrono::Local::now()));
        feed.results = Some(rx);
        feed.thread = Some(channel().0);
        feed
    }

    #[test]
    fn a_failure_is_a_change_but_not_fresh_data() {
        let mut feed = fed(vec![Err("offline".into())], Pulls::loading());
        assert_eq!(feed.take(|_, _| {}), Took { changed: true, fresh: false });
        assert!(matches!(feed.data.origin, Origin::Error { .. }));
        let mut feed = fed(vec![Err("offline".into()), Ok(Pulls::loading())], Pulls::loading());
        assert_eq!(feed.take(|_, _| {}), Took { changed: true, fresh: true });
    }

    #[test]
    fn a_thread_that_died_says_so() {
        let (tx, rx) = channel::<Result<Pulls, String>>();
        drop(tx);
        let mut feed = Feed::new(Pulls::loading(), ReadState::memory(chrono::Local::now()));
        feed.results = Some(rx);
        assert_eq!(feed.take(|_, _| {}), Took { changed: true, fresh: false });
        assert!(matches!(&feed.data.origin, Origin::Error { message, .. } if message.contains("stopped")));
        assert_eq!(feed.take(|_, _| {}), Took::default(), "said once");
    }

    #[test]
    fn retries_come_sooner_then_back_off_to_the_usual_wait() {
        let every = Duration::from_secs(180);
        let waits: Vec<u64> = (1..=6).map(|n| retry_after(n, every).as_secs()).collect();
        assert_eq!(waits, [15, 30, 60, 120, 180, 180]);
        assert_eq!(retry_after(40, Duration::from_secs(60)).as_secs(), 60, "no overflow");
    }
}
