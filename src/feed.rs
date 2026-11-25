//! One notifying integration's data in the panel (GitHub, Linear): the latest fetch or the
//! mock, what you've read of it, which unread events were announced, and the thread fetching
//! it.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
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
    /// Keeps the thread alive; dropping it stops the thread.
    _thread: Option<Sender<()>>,
}

impl<T: Fetched + Send + 'static> Feed<T> {
    pub fn new(data: T, read: ReadState) -> Self {
        Feed { data, read, announced: Announced::default(), results: None, _thread: None }
    }

    /// Shows `data` with no thread: the mock, or a scene.
    pub fn show(&mut self, data: T, read: ReadState) { *self = Feed::new(data, read); }

    /// Calls `fetch` now and every `every` on a thread named `name`, waking the UI with each
    /// result. Stops any earlier thread.
    pub fn start(&mut self, name: &'static str, every: Duration, fetch: impl FnMut() -> Result<T, String> + Send + 'static, wake: rune::Waker, loading: T, read: ReadState) {
        read.save();
        self.show(loading, read);
        let (tx, rx) = channel();
        self._thread = Some(spawn(name, every, fetch, tx, wake));
        self.results = Some(rx);
    }

    /// Takes in finished fetches, handing each good one to `arrived` first. A failure keeps the
    /// last good data and says so. True when anything arrived.
    pub fn take(&mut self, mut arrived: impl FnMut(&mut ReadState, &T)) -> bool {
        let Some(rx) = &self.results else { return false };
        let mut changed = false;
        while let Ok(result) = rx.try_recv() {
            match result {
                Ok(data) => {
                    arrived(&mut self.read, &data);
                    self.data = data;
                }
                Err(message) => {
                    let origin = self.data.origin();
                    let stale = match origin { Origin::Live { at } => Some(*at), Origin::Error { stale, .. } => *stale, _ => None };
                    *origin = Origin::Error { message, stale };
                }
            }
            changed = true;
        }
        changed
    }
}

fn spawn<T: Send + 'static>(name: &'static str, every: Duration, mut fetch: impl FnMut() -> Result<T, String> + Send + 'static,
    tx: Sender<Result<T, String>>, wake: rune::Waker) -> Sender<()> {
    let (control, commands) = channel::<()>();
    std::thread::Builder::new().name(name.into()).spawn(move || loop {
        let result = fetch();
        if let Err(e) = &result { eprintln!("{name}: {e}"); }
        if tx.send(result).is_err() { return; }
        wake();
        match commands.recv_timeout(every) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }).expect("spawn a fetching thread");
    control
}
