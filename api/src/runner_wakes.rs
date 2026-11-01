use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::watch;
use uuid::Uuid;

#[derive(Clone, Default)]
pub struct RunnerWakes {
    inner: Arc<Mutex<HashMap<Uuid, watch::Sender<u64>>>>,
}

pub struct WakeSubscription {
    id: Uuid,
    rx: Option<watch::Receiver<u64>>,
    wakes: RunnerWakes,
}

impl RunnerWakes {
    pub fn wake(&self, id: Uuid) {
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(tx) = map.get(&id) {
            if tx.receiver_count() == 0 {
                map.remove(&id);
            } else {
                tx.send_modify(|n| *n = n.wrapping_add(1));
            }
        }
    }

    pub fn subscribe(&self, id: Uuid) -> WakeSubscription {
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let rx = map.entry(id).or_insert_with(|| watch::channel(0).0).subscribe();
        WakeSubscription { id, rx: Some(rx), wakes: self.clone() }
    }

    pub fn listening(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    fn release(&self, id: Uuid) {
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if map.get(&id).is_some_and(|tx| tx.receiver_count() == 0) {
            map.remove(&id);
        }
    }
}

impl WakeSubscription {
    pub async fn woken(&mut self) -> bool {
        match self.rx.as_mut() {
            Some(rx) => rx.changed().await.is_ok(),
            None => false,
        }
    }
}

impl Drop for WakeSubscription {
    fn drop(&mut self) {
        self.rx.take();
        self.wakes.release(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn only_the_named_runner_wakes_and_bursts_collapse() {
        let wakes = RunnerWakes::default();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut first = wakes.subscribe(a);
        let mut second = wakes.subscribe(a);
        let mut other = wakes.subscribe(b);
        for _ in 0..50 {
            wakes.wake(a);
        }
        assert!(tokio::time::timeout(Duration::from_millis(50), first.woken()).await.unwrap());
        assert!(tokio::time::timeout(Duration::from_millis(50), second.woken()).await.unwrap());
        assert!(tokio::time::timeout(Duration::from_millis(50), first.woken()).await.is_err(), "fifty wakes count once");
        assert!(tokio::time::timeout(Duration::from_millis(50), other.woken()).await.is_err(), "another runner stays asleep");
        wakes.wake(Uuid::new_v4());
        assert_eq!(wakes.listening(), 2);
        drop(first);
        assert_eq!(wakes.listening(), 2);
        drop(second);
        assert_eq!(wakes.listening(), 1);
        drop(other);
        assert_eq!(wakes.listening(), 0);
    }
}
