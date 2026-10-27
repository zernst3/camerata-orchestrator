//! Ordering for saves that replace the whole of something (the onboarding draft).
//!
//! The server stores the draft as ONE blob and every save replaces it. If two saves are in flight
//! at once nothing orders them, so an older draft can land after a newer one and silently undo a
//! decision. [`SaveQueue`] is the single in-flight slot that prevents that: while a save is out,
//! a newer draft is only remembered (replacing any older one that was waiting), and when the save
//! finishes the newest remembered draft, if any, is sent. At most one save is ever in flight and
//! an older draft is never sent after a newer one.
//!
//! Pure: it only decides what to send and when; the caller does the sending.

/// The in-flight slot and the latest pending value.
#[derive(Debug)]
pub struct SaveQueue<T> {
    in_flight: bool,
    pending: Option<T>,
}

impl<T> Default for SaveQueue<T> {
    fn default() -> Self {
        Self {
            in_flight: false,
            pending: None,
        }
    }
}

impl<T> SaveQueue<T> {
    /// A new value to save. `Some(value)` when nothing is in flight: send it now. `None` when a
    /// save is out: the value is kept as the latest pending one, replacing an older pending one.
    pub fn submit(&mut self, value: T) -> Option<T> {
        if self.in_flight {
            self.pending = Some(value);
            None
        } else {
            self.in_flight = true;
            Some(value)
        }
    }

    /// The save that was in flight has finished. `Some(value)`: the newest pending value, to send
    /// next (the slot stays taken). `None`: nothing waiting, the slot is free.
    pub fn complete(&mut self) -> Option<T> {
        match self.pending.take() {
            Some(next) => Some(next),
            None => {
                self.in_flight = false;
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_value_goes_at_once_and_later_ones_wait_for_it() {
        let mut q = SaveQueue::default();
        assert_eq!(q.submit(1), Some(1));
        assert_eq!(q.submit(2), None);
        assert_eq!(
            q.submit(3),
            None,
            "3 replaces 2: the older one is never sent"
        );
        assert_eq!(q.complete(), Some(3));
        assert_eq!(q.complete(), None);
        // The slot is free again.
        assert_eq!(q.submit(4), Some(4));
    }
}
