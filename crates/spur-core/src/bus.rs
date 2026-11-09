//! Event bus: monotonic sequence numbers, a bounded journal for replay, and
//! bounded per-subscriber queues that never make the publisher wait.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::actor::Actor;
use crate::name::EventName;

/// Default number of events kept for replay.
pub const DEFAULT_JOURNAL: usize = 4096;
/// Default per-subscriber queue length.
pub const DEFAULT_QUEUE: usize = 256;

/// A published event.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Event {
    /// Sequence number, from 1, increasing by one per event.
    pub seq: u64,
    /// Event name.
    pub name: EventName,
    /// Who caused it.
    pub actor: Actor,
    /// Event data.
    pub payload: Value,
}

/// What a subscriber receives.
#[derive(Clone, Debug, PartialEq)]
pub enum Delivery {
    /// The next matching event.
    Event(Arc<Event>),
    /// The queue was full and matching events were dropped; use [`EventBus::replay`].
    Lagged {
        /// Matching events dropped.
        missed: u64,
        /// Seq of the first dropped event.
        first_missed_seq: u64,
    },
}

/// Which events a subscriber wants: exact names or `prefix.*`. Empty = all.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter(Vec<String>);

impl Filter {
    /// Every event.
    pub fn all() -> Self {
        Self::default()
    }

    /// Events matching any of `patterns` (`buffer.changed`, `agent.*`).
    pub fn new<I, P>(patterns: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<String>,
    {
        Self(patterns.into_iter().map(Into::into).collect())
    }

    /// Whether `name` matches.
    pub fn matches(&self, name: &str) -> bool {
        self.0.is_empty()
            || self.0.iter().any(|p| match p.strip_suffix(".*") {
                Some(prefix) => name
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('.')),
                None => p == name,
            })
    }
}

/// Replay is impossible: events after `since` were dropped from the journal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gap {
    /// The oldest retained seq.
    pub oldest: u64,
}

impl fmt::Display for Gap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "events before seq {} are no longer retained",
            self.oldest
        )
    }
}

impl std::error::Error for Gap {}

#[derive(Default)]
struct Queue {
    /// Events plus at most one lag marker before each event.
    items: VecDeque<Delivery>,
    /// Events in `items`; the capacity bounds this.
    events: usize,
    /// Dropped since the last queued event: (missed, `first_missed_seq`).
    lag: Option<(u64, u64)>,
    closed: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
    capacity: usize,
}

fn lock(s: &Shared) -> MutexGuard<'_, Queue> {
    s.queue.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The receiving end of a subscription. Dropping it unsubscribes.
pub struct Subscription {
    shared: Arc<Shared>,
}

impl Subscription {
    /// The next delivery, if any, without waiting.
    pub fn try_recv(&self) -> Option<Delivery> {
        Self::pop(&mut lock(&self.shared))
    }

    /// Waits up to `timeout` for a delivery.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<Delivery> {
        let q = lock(&self.shared);
        let (mut q, _) = self
            .shared
            .ready
            .wait_timeout_while(q, timeout, |q| {
                q.items.is_empty() && q.lag.is_none() && !q.closed
            })
            .unwrap_or_else(PoisonError::into_inner);
        Self::pop(&mut q)
    }

    /// Everything queued now.
    pub fn drain(&self) -> Vec<Delivery> {
        let mut q = lock(&self.shared);
        std::iter::from_fn(|| Self::pop(&mut q)).collect()
    }

    fn pop(q: &mut Queue) -> Option<Delivery> {
        if let Some(d) = q.items.pop_front() {
            if matches!(d, Delivery::Event(_)) {
                q.events -= 1;
            }
            return Some(d);
        }
        q.lag.take().map(|(missed, first)| Delivery::Lagged {
            missed,
            first_missed_seq: first,
        })
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        lock(&self.shared).closed = true;
    }
}

/// The bus. Owned by the core state thread.
pub struct EventBus {
    next_seq: u64,
    journal: VecDeque<Arc<Event>>,
    journal_cap: usize,
    subs: Vec<(Filter, Weak<Shared>)>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(DEFAULT_JOURNAL)
    }
}

impl EventBus {
    /// A bus keeping `journal` events for replay (at least 1).
    pub fn new(journal: usize) -> Self {
        Self {
            next_seq: 1,
            journal: VecDeque::new(),
            journal_cap: journal.max(1),
            subs: Vec::new(),
        }
    }

    /// Seq of the last published event, 0 if none.
    pub fn head(&self) -> u64 {
        self.next_seq - 1
    }

    /// Publishes an event and returns its seq. Never waits on subscribers.
    pub fn publish(&mut self, name: EventName, actor: Actor, payload: Value) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        let ev = Arc::new(Event {
            seq,
            name,
            actor,
            payload,
        });
        if self.journal.len() == self.journal_cap {
            self.journal.pop_front();
        }
        self.journal.push_back(ev.clone());
        self.subs.retain(|(filter, weak)| {
            let Some(shared) = weak.upgrade() else {
                return false;
            };
            let mut q = lock(&shared);
            if q.closed {
                return false;
            }
            if !filter.matches(ev.name.as_str()) {
                return true;
            }
            if q.events >= shared.capacity {
                let lag = q.lag.get_or_insert((0, seq));
                lag.0 += 1;
            } else {
                if let Some((missed, first)) = q.lag.take() {
                    q.items.push_back(Delivery::Lagged {
                        missed,
                        first_missed_seq: first,
                    });
                }
                q.events += 1;
                q.items.push_back(Delivery::Event(ev.clone()));
                drop(q);
                shared.ready.notify_one();
            }
            true
        });
        seq
    }

    /// Events after `since` (exclusive) that match `filter`.
    ///
    /// # Errors
    ///
    /// Returns [`Gap`] if events after `since` were dropped from the journal.
    pub fn replay(&self, since: u64, filter: &Filter) -> Result<Vec<Arc<Event>>, Gap> {
        if since >= self.head() {
            return Ok(Vec::new());
        }
        let oldest = self.journal.front().map_or(self.next_seq, |e| e.seq);
        if since + 1 < oldest {
            return Err(Gap { oldest });
        }
        Ok(self
            .journal
            .iter()
            .filter(|e| e.seq > since && filter.matches(e.name.as_str()))
            .cloned()
            .collect())
    }

    /// Subscribes. With `since`, also returns the backlog after it, so the caller sees
    /// every matching event exactly once.
    ///
    /// # Errors
    ///
    /// Returns [`Gap`] if the backlog after `since` is no longer retained.
    pub fn subscribe(
        &mut self,
        filter: Filter,
        capacity: usize,
        since: Option<u64>,
    ) -> Result<(Vec<Arc<Event>>, Subscription), Gap> {
        let backlog = match since {
            Some(s) => self.replay(s, &filter)?,
            None => Vec::new(),
        };
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            ready: Condvar::new(),
            capacity: capacity.max(1),
        });
        self.subs.push((filter, Arc::downgrade(&shared)));
        Ok((backlog, Subscription { shared }))
    }

    /// Live subscriber count (dropped ones are pruned on publish).
    pub fn subscriber_count(&self) -> usize {
        self.subs.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> EventName {
        EventName::new(s).unwrap()
    }

    fn seqs(d: &[Delivery]) -> Vec<String> {
        d.iter()
            .map(|d| match d {
                Delivery::Event(e) => e.seq.to_string(),
                Delivery::Lagged {
                    missed,
                    first_missed_seq,
                } => format!("lag{missed}@{first_missed_seq}"),
            })
            .collect()
    }

    #[test]
    fn seq_and_filter() {
        let mut bus = EventBus::default();
        let (_, sub) = bus
            .subscribe(Filter::new(["agent.*", "buffer.saved"]), 16, None)
            .unwrap();
        assert_eq!(
            bus.publish(name("buffer.changed"), Actor::User, Value::Null),
            1
        );
        assert_eq!(
            bus.publish(name("agent.turn.start"), Actor::User, Value::Null),
            2
        );
        assert_eq!(bus.publish(name("agents.x"), Actor::User, Value::Null), 3);
        assert_eq!(
            bus.publish(name("buffer.saved"), Actor::User, Value::Null),
            4
        );
        assert_eq!(seqs(&sub.drain()), ["2", "4"]);
        assert_eq!(bus.head(), 4);
    }

    #[test]
    fn full_queue_never_blocks_and_reports_lag_in_order() {
        let mut bus = EventBus::default();
        let (_, stalled) = bus.subscribe(Filter::all(), 1, None).unwrap();
        for _ in 0..10_000 {
            bus.publish(name("buffer.changed"), Actor::User, Value::Null);
        }
        assert_eq!(seqs(&stalled.drain()), ["1", "lag9999@2"]);

        let (_, sub) = bus.subscribe(Filter::all(), 2, None).unwrap();
        for _ in 0..5 {
            bus.publish(name("buffer.changed"), Actor::User, Value::Null);
        }
        // 10001, 10002 queued; 10003..=10005 dropped.
        assert_eq!(
            sub.try_recv().map(|d| seqs(&[d])),
            Some(vec!["10001".into()])
        );
        bus.publish(name("buffer.changed"), Actor::User, Value::Null);
        assert_eq!(seqs(&sub.drain()), ["10002", "lag3@10003", "10006"]);
        assert!(sub.try_recv().is_none());

        // Two lags while earlier items are still queued keep their order.
        let (_, sub) = bus.subscribe(Filter::all(), 2, None).unwrap();
        let mut publish = |n: usize| {
            for _ in 0..n {
                bus.publish(name("buffer.changed"), Actor::User, Value::Null);
            }
        };
        publish(3); // 10007, 10008 queued; 10009 dropped
        sub.try_recv();
        publish(2); // lag, 10010 queued; 10011 dropped
        sub.try_recv();
        publish(1); // lag, 10012 queued
        assert_eq!(
            seqs(&sub.drain()),
            ["lag1@10009", "10010", "lag1@10011", "10012"]
        );
    }

    #[test]
    fn dropped_subscription_is_pruned() {
        let mut bus = EventBus::default();
        let (_, sub) = bus.subscribe(Filter::all(), 1, None).unwrap();
        assert_eq!(bus.subscriber_count(), 1);
        drop(sub);
        bus.publish(name("a.b"), Actor::User, Value::Null);
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[test]
    fn replay_and_gap() {
        let mut bus = EventBus::new(3);
        assert_eq!(bus.replay(0, &Filter::all()).unwrap(), vec![]);
        for _ in 0..5 {
            bus.publish(name("a.b"), Actor::User, Value::Null);
        }
        let got: Vec<u64> = bus
            .replay(2, &Filter::all())
            .unwrap()
            .iter()
            .map(|e| e.seq)
            .collect();
        assert_eq!(got, [3, 4, 5]);
        assert_eq!(bus.replay(1, &Filter::all()), Err(Gap { oldest: 3 }));
        assert!(bus.replay(5, &Filter::all()).unwrap().is_empty());
        assert!(bus.replay(99, &Filter::all()).unwrap().is_empty());
    }

    #[test]
    fn subscribe_since_has_no_gap_or_duplicate() {
        let mut bus = EventBus::default();
        for _ in 0..3 {
            bus.publish(name("a.b"), Actor::User, Value::Null);
        }
        let (backlog, sub) = bus.subscribe(Filter::all(), 8, Some(1)).unwrap();
        bus.publish(name("a.b"), Actor::User, Value::Null);
        let mut all: Vec<u64> = backlog.iter().map(|e| e.seq).collect();
        for d in sub.drain() {
            if let Delivery::Event(e) = d {
                all.push(e.seq);
            }
        }
        assert_eq!(all, [2, 3, 4]);
    }

    #[test]
    fn recv_across_threads() {
        let mut bus = EventBus::default();
        let (_, sub) = bus.subscribe(Filter::all(), 8, None).unwrap();
        let h = std::thread::spawn(move || sub.recv_timeout(Duration::from_secs(5)));
        bus.publish(name("a.b"), Actor::User, Value::Null);
        assert!(matches!(h.join().unwrap(), Some(Delivery::Event(e)) if e.seq == 1));
    }
}
