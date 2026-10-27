//! Keeps the *sends* in message order while the transfers themselves run in parallel.
//!
//! Every file of a job gets an index. A worker may send its finished upload only when all lower indexes are
//! resolved (sent, failed or cancelled), so episode 2 never appears in the chat before episode 1.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use tokio::sync::watch;

pub struct Gate {
    /// (smallest unresolved index, resolved flags)
    state: Mutex<(usize, Vec<bool>)>,
    tx: watch::Sender<usize>,
    _keep_open: watch::Receiver<usize>,
}

impl Gate {
    pub fn new(total: usize) -> Self {
        let (tx, rx) = watch::channel(0usize);
        Self {
            state: Mutex::new((0, vec![false; total])),
            tx,
            _keep_open: rx,
        }
    }

    /// Marks `index` as done (whatever the outcome) and lets the next waiting index through.
    pub fn resolve(&self, index: usize) {
        let next = {
            let mut st = self.state.lock().unwrap();
            if let Some(flag) = st.1.get_mut(index) {
                *flag = true;
            }
            while st.0 < st.1.len() && st.1[st.0] {
                st.0 += 1;
            }
            st.0
        };
        let _ = self.tx.send(next);
    }

    #[cfg(test)]
    fn next(&self) -> usize {
        self.state.lock().unwrap().0
    }

    /// Waits until it is `index`'s turn. Returns `false` when the job was cancelled meanwhile.
    pub async fn wait_turn(&self, index: usize, cancel: &AtomicBool) -> bool {
        let mut rx = self.tx.subscribe();
        loop {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            let next = {
                let current = rx.borrow();
                *current
            };
            if next >= index {
                return true;
            }
            tokio::select! {
                _ = rx.changed() => {}
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
        }
    }
}

/// Resolves its index when dropped, so a worker that fails, is cancelled or even panics never blocks the others.
pub struct Resolve {
    gate: Arc<Gate>,
    index: usize,
}

impl Resolve {
    pub fn new(gate: Arc<Gate>, index: usize) -> Self {
        Self { gate, index }
    }
}

impl Drop for Resolve {
    fn drop(&mut self) {
        self.gate.resolve(self.index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_advances_in_order() {
        let gate = Gate::new(4);
        assert_eq!(gate.next(), 0);
        gate.resolve(1); // finished early: nobody may jump the queue
        assert_eq!(gate.next(), 0);
        gate.resolve(0);
        assert_eq!(gate.next(), 2);
        gate.resolve(3);
        assert_eq!(gate.next(), 2);
        gate.resolve(2);
        assert_eq!(gate.next(), 4);
    }

    #[test]
    fn guard_resolves_on_drop() {
        let gate = Arc::new(Gate::new(2));
        {
            let _g = Resolve::new(gate.clone(), 0);
        }
        assert_eq!(gate.next(), 1);
    }
}
