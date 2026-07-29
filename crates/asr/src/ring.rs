//! Fixed-capacity lock-free SPSC audio ring.

use std::{
    cell::UnsafeCell,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

struct Shared {
    slots: Box<[UnsafeCell<f32>]>,
    read: AtomicUsize,
    write: AtomicUsize,
}

// SAFETY: exactly one Producer writes slots before publishing `write`, and exactly
// one Consumer reads published slots before advancing `read`. Handles are not Clone.
unsafe impl Sync for Shared {}

/// Single producer half of an audio ring.
pub struct RingProducer {
    shared: Arc<Shared>,
}
/// Single consumer half of an audio ring.
pub struct RingConsumer {
    shared: Arc<Shared>,
}
pub(crate) type Producer = RingProducer;
pub(crate) type Consumer = RingConsumer;

#[must_use]
pub fn spsc_ring(capacity: usize) -> (RingProducer, RingConsumer) {
    let size = capacity.max(1).saturating_add(1);
    let slots = (0..size)
        .map(|_| UnsafeCell::new(0.0))
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let shared = Arc::new(Shared {
        slots,
        read: AtomicUsize::new(0),
        write: AtomicUsize::new(0),
    });
    (
        RingProducer {
            shared: Arc::clone(&shared),
        },
        RingConsumer { shared },
    )
}

impl RingProducer {
    /// Pushes samples without allocating. New audio is dropped if the consumer lags.
    pub fn push_slice(&mut self, samples: &[f32]) {
        let size = self.shared.slots.len();
        for sample in samples {
            let write = self.shared.write.load(Ordering::Relaxed);
            let next = (write + 1) % size;
            if next == self.shared.read.load(Ordering::Acquire) {
                break;
            }
            // SAFETY: this producer exclusively writes the unpublished slot.
            unsafe {
                *self.shared.slots[write].get() = *sample;
            }
            self.shared.write.store(next, Ordering::Release);
        }
    }
}

impl RingConsumer {
    #[must_use]
    pub fn available(&self) -> usize {
        let read = self.shared.read.load(Ordering::Relaxed);
        let write = self.shared.write.load(Ordering::Acquire);
        if write >= read {
            write - read
        } else {
            self.shared.slots.len() - read + write
        }
    }

    pub(crate) fn take_latest(&mut self, limit: usize) -> (Vec<f32>, usize) {
        let available = self.available();
        let skipped = available.saturating_sub(limit);
        let size = self.shared.slots.len();
        let mut read = (self.shared.read.load(Ordering::Relaxed) + skipped) % size;
        let count = available - skipped;
        let mut output = Vec::with_capacity(count);
        for _ in 0..count {
            // SAFETY: producer published this slot and cannot reuse it until read advances.
            output.push(unsafe { *self.shared.slots[read].get() });
            read = (read + 1) % size;
        }
        self.shared.read.store(read, Ordering::Release);
        (output, skipped)
    }

    /// Test/benchmark hook for observing a bounded window without exposing storage.
    #[doc(hidden)]
    pub fn take_latest_for_test(&mut self, limit: usize) -> (Vec<f32>, usize) {
        self.take_latest(limit)
    }
}

#[cfg(test)]
mod tests {
    use super::spsc_ring;

    #[test]
    fn ring_is_fixed_capacity_and_preserves_order() {
        let (mut producer, mut consumer) = spsc_ring(3);
        producer.push_slice(&[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(consumer.available(), 3);
        assert_eq!(consumer.take_latest(3).0, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn latest_window_skips_old_samples() {
        let (mut producer, mut consumer) = spsc_ring(5);
        producer.push_slice(&[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(consumer.take_latest(2), (vec![3.0, 4.0], 2));
    }
}
