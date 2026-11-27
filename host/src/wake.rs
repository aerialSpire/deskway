use std::sync::mpsc::{self, Receiver, SendError, Sender};
use std::sync::{Arc, Mutex, Weak};

use polling::Poller;

/// Channels share this handle so they can outlive the poller attached to it.
#[derive(Clone, Default)]
pub(crate) struct Waker(Arc<Mutex<Weak<Poller>>>);

impl Waker {
    pub(crate) fn attach(&self, poller: &Arc<Poller>) {
        *self.0.lock().unwrap() = Arc::downgrade(poller);
    }

    pub(crate) fn channel<T>(&self) -> (Wake<T>, Receiver<T>) {
        let (tx, rx) = mpsc::channel();
        (Wake { tx: Arc::new(tx), wake: self.clone() }, rx)
    }
}

/// A channel sender that notifies the receiving thread's poller after enqueueing.
pub(crate) struct Wake<T> {
    tx: Arc<Sender<T>>,
    wake: Waker,
}

/// A sender for a worker that must not keep its consumer's queue open.
pub(crate) struct WeakWake<T> {
    tx: Weak<Sender<T>>,
    wake: Waker,
}

impl<T> Clone for WeakWake<T> {
    fn clone(&self) -> Self {
        Self { tx: self.tx.clone(), wake: self.wake.clone() }
    }
}

impl<T> WeakWake<T> {
    pub(crate) fn upgrade(&self) -> Option<Wake<T>> {
        Some(Wake { tx: self.tx.upgrade()?, wake: self.wake.clone() })
    }
}

impl<T> Clone for Wake<T> {
    fn clone(&self) -> Self {
        Self { tx: self.tx.clone(), wake: self.wake.clone() }
    }
}

impl<T> Wake<T> {
    pub(crate) fn downgrade(&self) -> WeakWake<T> {
        WeakWake { tx: Arc::downgrade(&self.tx), wake: self.wake.clone() }
    }

    pub(crate) fn send(&self, message: T) -> Result<(), SendError<T>> {
        self.tx.send(message)?;
        if let Some(poller) = self.wake.0.lock().unwrap().upgrade() {
            if let Err(e) = poller.notify() {
                eprintln!("cannot notify channel poller: {e}");
            }
        }
        Ok(())
    }
}
