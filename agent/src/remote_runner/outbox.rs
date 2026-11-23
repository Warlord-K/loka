use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use loka_shared::runners::{RunnerCompletion, RunnerEvent, USAGE_EVENT};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, Notify};
use tracing::{debug, info, warn};
use uuid::Uuid;

const DIR: &str = "outbox";
const MAX_JOURNAL_BYTES: u64 = 16 * 1024 * 1024;
const MAX_BATCH: usize = 100;
const FIRST_RETRY: Duration = Duration::from_secs(1);
const MAX_RETRY: Duration = Duration::from_secs(15);
const REMINDER: Duration = Duration::from_secs(300);
const REFUSALS_BEFORE_DROP: u32 = 12;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum Entry {
    Events { events: Vec<RunnerEvent> },
    Completion { completion: Box<RunnerCompletion> },
}

pub(super) enum Batch {
    Events(Vec<RunnerEvent>),
    Completion(Box<RunnerCompletion>),
}

#[derive(Debug)]
pub(super) enum Sent {
    Delivered,
    Stale,
    Refused(String),
    Retry(anyhow::Error),
}

pub(super) trait Deliver: Send + Sync + 'static {
    fn deliver<'a>(&'a self, lease_id: Uuid, batch: &'a Batch) -> BoxFuture<'a, Sent>;
}

enum Msg {
    Event(RunnerEvent),
    Complete(Box<RunnerCompletion>, oneshot::Sender<()>),
}

struct Inner {
    dir: PathBuf,
    deliver: Arc<dyn Deliver>,
    lanes: Mutex<HashMap<Uuid, mpsc::WeakUnboundedSender<Msg>>>,
    finishing: Mutex<BTreeSet<Uuid>>,
    kick: Notify,
    settled: Notify,
}

#[derive(Clone)]
pub(super) struct Outbox {
    inner: Arc<Inner>,
}

pub(super) struct Lane {
    lease_id: Uuid,
    sender: mpsc::UnboundedSender<Msg>,
}

#[derive(Clone)]
pub(super) struct EventSink {
    sender: mpsc::UnboundedSender<Msg>,
}

impl EventSink {
    pub(super) fn push(&self, event: RunnerEvent) {
        let _ = self.sender.send(Msg::Event(event));
    }
}

impl Lane {
    pub(super) fn sink(&self) -> EventSink {
        EventSink { sender: self.sender.clone() }
    }

    pub(super) async fn complete(&self, completion: RunnerCompletion) {
        let (done, wait) = oneshot::channel();
        if self.sender.send(Msg::Complete(Box::new(completion), done)).is_err() {
            warn!(lease_id = %self.lease_id, "Runner completion had no outbox lane to go to");
            return;
        }
        let _ = wait.await;
    }
}

impl Outbox {
    pub(super) fn open(root: &Path, deliver: Arc<dyn Deliver>) -> std::io::Result<Self> {
        let dir = root.join(DIR);
        std::fs::create_dir_all(&dir)?;
        let outbox = Outbox {
            inner: Arc::new(Inner {
                dir,
                deliver,
                lanes: Mutex::new(HashMap::new()),
                finishing: Mutex::new(BTreeSet::new()),
                kick: Notify::new(),
                settled: Notify::new(),
            }),
        };
        outbox.recover();
        Ok(outbox)
    }

    pub(super) fn lane(&self, lease_id: Uuid) -> Lane {
        let mut lanes = self.inner.lanes.lock().unwrap();
        if let Some(sender) = lanes.get(&lease_id).and_then(mpsc::WeakUnboundedSender::upgrade).filter(|sender| !sender.is_closed()) {
            return Lane { lease_id, sender };
        }
        let (sender, receiver) = mpsc::unbounded_channel();
        lanes.insert(lease_id, sender.downgrade());
        let journal = Journal::create(&self.inner.dir, lease_id);
        tokio::spawn(run_lane(self.clone(), lease_id, receiver, journal, VecDeque::new()));
        Lane { lease_id, sender }
    }

    pub(super) async fn complete(&self, lease_id: Uuid, completion: RunnerCompletion) {
        self.lane(lease_id).complete(completion).await;
    }

    pub(super) fn finishing(&self) -> Vec<Uuid> {
        self.inner.finishing.lock().unwrap().iter().copied().collect()
    }

    pub(super) fn kick(&self) {
        self.inner.kick.notify_waiters();
    }

    pub(super) async fn flush(&self, wait: Duration) {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let settled = self.inner.settled.notified();
            tokio::pin!(settled);
            settled.as_mut().enable();
            if self.inner.finishing.lock().unwrap().is_empty() {
                return;
            }
            self.kick();
            if tokio::time::timeout_at(deadline, settled).await.is_err() {
                return;
            }
        }
    }

    fn recover(&self) {
        let Ok(entries) = std::fs::read_dir(&self.inner.dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(lease_id) = path.file_stem().and_then(|stem| stem.to_str()).and_then(|stem| stem.parse::<Uuid>().ok()) else {
                continue;
            };
            let (journal, pending) = Journal::reopen(&self.inner.dir, lease_id);
            if pending.is_empty() {
                journal.remove();
                continue;
            }
            let completed = pending.iter().any(|(_, entry)| matches!(entry, Entry::Completion { .. }));
            info!(%lease_id, entries = pending.len(), completed, "Delivering runner updates queued before a restart");
            if completed {
                self.inner.finishing.lock().unwrap().insert(lease_id);
            }
            let (sender, receiver) = mpsc::unbounded_channel();
            self.inner.lanes.lock().unwrap().insert(lease_id, sender.downgrade());
            drop(sender);
            tokio::spawn(run_lane(self.clone(), lease_id, receiver, journal, pending));
        }
    }

    fn finished(&self, lease_id: Uuid) {
        self.inner.finishing.lock().unwrap().remove(&lease_id);
        let mut lanes = self.inner.lanes.lock().unwrap();
        if lanes.get(&lease_id).is_some_and(|sender| sender.upgrade().is_none_or(|sender| sender.is_closed())) {
            lanes.remove(&lease_id);
        }
        drop(lanes);
        self.inner.settled.notify_waiters();
    }
}

struct Journal {
    path: PathBuf,
    sent_path: PathBuf,
    file: Option<File>,
    next_line: u64,
    bytes: u64,
    full: bool,
}

impl Journal {
    fn paths(dir: &Path, lease_id: Uuid) -> (PathBuf, PathBuf) {
        (dir.join(format!("{lease_id}.jsonl")), dir.join(format!("{lease_id}.sent")))
    }

    fn create(dir: &Path, lease_id: Uuid) -> Self {
        let (path, sent_path) = Self::paths(dir, lease_id);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&sent_path);
        Journal { path, sent_path, file: None, next_line: 0, bytes: 0, full: false }
    }

    fn reopen(dir: &Path, lease_id: Uuid) -> (Self, VecDeque<(u64, Entry)>) {
        let (path, sent_path) = Self::paths(dir, lease_id);
        let sent = std::fs::read_to_string(&sent_path).ok().and_then(|text| text.trim().parse::<u64>().ok()).unwrap_or(0);
        let mut pending = VecDeque::new();
        let mut next_line = 0;
        let mut bytes = 0;
        if let Ok(file) = File::open(&path) {
            for line in BufReader::new(file).lines() {
                let Ok(line) = line else { break };
                bytes += line.len() as u64 + 1;
                let number = next_line;
                next_line += 1;
                if number < sent {
                    continue;
                }
                match serde_json::from_str::<Entry>(&line) {
                    Ok(entry) => pending.push_back((number, entry)),
                    Err(error) => debug!(%error, "Skipped an unreadable outbox line"),
                }
            }
        }
        let mut journal = Journal { path, sent_path, file: None, next_line, bytes, full: false };
        journal.full = bytes >= MAX_JOURNAL_BYTES;
        (journal, pending)
    }

    fn append(&mut self, entry: &Entry) -> Option<u64> {
        let line = match serde_json::to_string(entry) {
            Ok(line) => line,
            Err(error) => {
                warn!(%error, "Could not encode a runner update for the outbox");
                return None;
            }
        };
        if self.file.is_none() {
            match OpenOptions::new().create(true).append(true).open(&self.path) {
                Ok(file) => self.file = Some(file),
                Err(error) => warn!(%error, path = %self.path.display(), "Could not open the runner outbox, keeping updates in memory"),
            }
        }
        if let Some(file) = self.file.as_mut() {
            if let Err(error) = writeln!(file, "{line}") {
                warn!(%error, "Could not write to the runner outbox, keeping updates in memory");
            }
        }
        self.bytes += line.len() as u64 + 1;
        let number = self.next_line;
        self.next_line += 1;
        Some(number)
    }

    fn mark_sent(&self, through: u64) {
        if self.file.is_none() && !self.path.exists() {
            return;
        }
        if let Err(error) = std::fs::write(&self.sent_path, (through + 1).to_string()) {
            debug!(%error, "Could not record outbox progress");
        }
    }

    fn remove(self) {
        drop(self.file);
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(&self.sent_path);
    }
}

struct LaneState {
    pending: VecDeque<(u64, Entry)>,
    waiters: Vec<oneshot::Sender<()>>,
    completed: bool,
    dropped_events: u64,
}

impl LaneState {
    fn absorb(&mut self, journal: &mut Journal, outbox: &Outbox, lease_id: Uuid, msg: Msg) {
        match msg {
            Msg::Event(event) => {
                if self.completed {
                    debug!(%lease_id, "Dropped a runner event that came after the completion");
                    return;
                }
                let usage = event.kind == USAGE_EVENT;
                if !usage && journal.full {
                    self.dropped_events += 1;
                    if self.dropped_events == 1 {
                        warn!(%lease_id, "Runner outbox for this run is full, so older activity is kept and newer activity lines are dropped until Loka is reachable");
                    }
                    return;
                }
                if usage {
                    self.pending.retain(|(_, entry)| !matches!(entry, Entry::Events { events } if events.len() == 1 && events[0].kind == USAGE_EVENT));
                }
                let entry = Entry::Events { events: vec![event] };
                if let Some(line) = journal.append(&entry) {
                    self.pending.push_back((line, entry));
                }
                if journal.bytes >= MAX_JOURNAL_BYTES {
                    journal.full = true;
                }
            }
            Msg::Complete(completion, done) => {
                self.waiters.push(done);
                if self.completed {
                    return;
                }
                self.completed = true;
                outbox.inner.finishing.lock().unwrap().insert(lease_id);
                let entry = Entry::Completion { completion };
                if let Some(line) = journal.append(&entry) {
                    self.pending.push_back((line, entry));
                }
                if let Some(file) = journal.file.as_ref() {
                    let _ = file.sync_data();
                }
            }
        }
    }

    fn batch(&self) -> Option<(usize, Batch)> {
        let (_, first) = self.pending.front()?;
        if let Entry::Completion { completion } = first {
            return Some((1, Batch::Completion(completion.clone())));
        }
        let mut events = Vec::new();
        let mut taken = 0;
        for (_, entry) in &self.pending {
            match entry {
                Entry::Events { events: more } if events.len() + more.len() <= MAX_BATCH || events.is_empty() => {
                    events.extend(more.iter().cloned());
                    taken += 1;
                }
                _ => break,
            }
        }
        Some((taken, Batch::Events(events)))
    }

    fn settle(&mut self, taken: usize, journal: &Journal) -> bool {
        let mut last = None;
        let mut finished = false;
        for _ in 0..taken {
            if let Some((line, entry)) = self.pending.pop_front() {
                last = Some(line);
                finished |= matches!(entry, Entry::Completion { .. });
            }
        }
        if let Some(line) = last {
            journal.mark_sent(line);
        }
        finished
    }
}

async fn run_lane(
    outbox: Outbox,
    lease_id: Uuid,
    mut receiver: mpsc::UnboundedReceiver<Msg>,
    mut journal: Journal,
    pending: VecDeque<(u64, Entry)>,
) {
    let completed = pending.iter().any(|(_, entry)| matches!(entry, Entry::Completion { .. }));
    let mut state = LaneState { pending, waiters: Vec::new(), completed, dropped_events: 0 };
    let mut open = true;
    let mut delay = FIRST_RETRY;
    let mut failures = 0u32;
    let mut refusals = 0u32;
    let mut since: Option<Instant> = None;
    let mut reminded: Option<Instant> = None;
    let mut delivered_completion = false;
    loop {
        while open {
            match receiver.try_recv() {
                Ok(msg) => state.absorb(&mut journal, &outbox, lease_id, msg),
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => open = false,
            }
        }
        if delivered_completion {
            break;
        }
        let Some((taken, batch)) = state.batch() else {
            if !open {
                break;
            }
            match receiver.recv().await {
                Some(msg) => state.absorb(&mut journal, &outbox, lease_id, msg),
                None => open = false,
            }
            continue;
        };
        let completion = matches!(batch, Batch::Completion(_));
        match outbox.inner.deliver.deliver(lease_id, &batch).await {
            Sent::Delivered => {
                if let Some(down) = since.take() {
                    info!(%lease_id, failures, waited_secs = down.elapsed().as_secs(), "Runner updates delivered after Loka was unreachable");
                }
                failures = 0;
                refusals = 0;
                reminded = None;
                delay = FIRST_RETRY;
                delivered_completion = state.settle(taken, &journal);
            }
            Sent::Stale => {
                if completion {
                    warn!(%lease_id, failures, "Runner completion was stale");
                } else {
                    debug!(%lease_id, "Runner events were refused because the run already ended");
                }
                delivered_completion = state.settle(taken, &journal);
            }
            Sent::Refused(reason) => {
                refusals += 1;
                if refusals >= REFUSALS_BEFORE_DROP {
                    warn!(%lease_id, attempts = refusals, %reason, completion, "Loka kept refusing a runner update, dropping it");
                    refusals = 0;
                    delivered_completion = state.settle(taken, &journal);
                    continue;
                }
                if refusals == 1 {
                    warn!(%lease_id, %reason, completion, "Loka refused a runner update, retrying");
                }
                wait(&outbox, &mut receiver, &mut open, &mut state, &mut journal, lease_id, delay).await;
                delay = (delay * 2).min(MAX_RETRY);
            }
            Sent::Retry(error) => {
                failures += 1;
                let down = *since.get_or_insert_with(Instant::now);
                if reminded.is_none_or(|at| at.elapsed() >= REMINDER) {
                    reminded = Some(Instant::now());
                    warn!(%lease_id, error = %format!("{error:#}"), failures, queued = state.pending.len(), down_secs = down.elapsed().as_secs(), "Loka is unreachable, keeping runner updates on disk until it is back");
                }
                wait(&outbox, &mut receiver, &mut open, &mut state, &mut journal, lease_id, delay).await;
                delay = (delay * 2).min(MAX_RETRY);
            }
        }
    }
    for waiter in state.waiters.drain(..) {
        let _ = waiter.send(());
    }
    if state.dropped_events > 0 {
        warn!(%lease_id, dropped = state.dropped_events, "Some activity lines of this run were dropped while Loka was unreachable");
    }
    receiver.close();
    while let Ok(msg) = receiver.try_recv() {
        if let Msg::Complete(_, done) = msg {
            let _ = done.send(());
        }
    }
    journal.remove();
    outbox.finished(lease_id);
}

async fn wait(
    outbox: &Outbox,
    receiver: &mut mpsc::UnboundedReceiver<Msg>,
    open: &mut bool,
    state: &mut LaneState,
    journal: &mut Journal,
    lease_id: Uuid,
    delay: Duration,
) {
    let sleep = tokio::time::sleep(delay);
    tokio::pin!(sleep);
    let kicked = outbox.inner.kick.notified();
    tokio::pin!(kicked);
    loop {
        tokio::select! {
            _ = &mut sleep => return,
            _ = &mut kicked => return,
            msg = receiver.recv(), if *open => match msg {
                Some(msg) => state.absorb(journal, outbox, lease_id, msg),
                None => *open = false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Default)]
    struct Fake {
        down: AtomicBool,
        stale_events: AtomicBool,
        sent: Mutex<Vec<String>>,
    }

    impl Deliver for Fake {
        fn deliver<'a>(&'a self, _lease_id: Uuid, batch: &'a Batch) -> BoxFuture<'a, Sent> {
            Box::pin(async move {
                if self.down.load(Ordering::SeqCst) {
                    return Sent::Retry(anyhow::anyhow!("connection refused"));
                }
                match batch {
                    Batch::Events(_) if self.stale_events.load(Ordering::SeqCst) => Sent::Stale,
                    Batch::Events(events) => {
                        self.sent.lock().unwrap().push(events.iter().map(|e| e.text.clone()).collect::<Vec<_>>().join(","));
                        Sent::Delivered
                    }
                    Batch::Completion(completion) => {
                        self.sent.lock().unwrap().push(format!("done:{}", completion.output));
                        Sent::Delivered
                    }
                }
            })
        }
    }

    fn event(kind: &str, text: &str) -> RunnerEvent {
        RunnerEvent { kind: kind.into(), tool: None, text: text.into() }
    }

    fn completion(output: &str) -> RunnerCompletion {
        RunnerCompletion {
            output: output.into(),
            session_id: None,
            cost_usd: 0.0,
            exit_code: 0,
            usage: Default::default(),
            error: None,
            workspace_key: "w".into(),
            pr_url: None,
            pr_number: None,
            publish_pr_state: None,
            checked_pr_url: None,
            publish_pr_checked_at: None,
            checkpoint: None,
            steered: Vec::new(),
            retryable: false,
            work_summary: None,
            context_sent: None,
            published_head: None,
        }
    }

    fn root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("loka-outbox-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn updates_wait_on_disk_and_drain_in_order_when_loka_is_back() {
        let root = root();
        let fake = Arc::new(Fake::default());
        fake.down.store(true, Ordering::SeqCst);
        let outbox = Outbox::open(&root, fake.clone()).unwrap();
        let lease = Uuid::new_v4();
        let lane = outbox.lane(lease);
        let sink = lane.sink();
        sink.push(event("tool", "a"));
        sink.push(event(USAGE_EVENT, "u1"));
        sink.push(event("text", "b"));
        sink.push(event(USAGE_EVENT, "u2"));
        drop(sink);
        let finishing = tokio::spawn(async move { lane.complete(completion("ok")).await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(outbox.finishing(), vec![lease]);
        let journal = std::fs::read_to_string(root.join(DIR).join(format!("{lease}.jsonl"))).unwrap();
        assert_eq!(journal.lines().count(), 5, "every update is on disk while Loka is down");
        fake.down.store(false, Ordering::SeqCst);
        outbox.flush(Duration::from_secs(5)).await;
        finishing.await.unwrap();
        assert_eq!(*fake.sent.lock().unwrap(), vec!["a,b,u2".to_string(), "done:ok".to_string()]);
        assert!(outbox.finishing().is_empty());
        assert!(!root.join(DIR).join(format!("{lease}.jsonl")).exists());
    }

    #[tokio::test]
    async fn a_restarted_runner_delivers_what_the_last_one_queued() {
        let root = root();
        let lease = Uuid::new_v4();
        let dir = root.join(DIR);
        std::fs::create_dir_all(&dir).unwrap();
        let lines = [
            Entry::Events { events: vec![event("tool", "sent before")] },
            Entry::Events { events: vec![event("tool", "c")] },
            Entry::Completion { completion: Box::new(completion("late")) },
        ]
        .iter()
        .map(|entry| serde_json::to_string(entry).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
        std::fs::write(dir.join(format!("{lease}.jsonl")), format!("{lines}\n")).unwrap();
        std::fs::write(dir.join(format!("{lease}.sent")), "1").unwrap();
        let fake = Arc::new(Fake::default());
        fake.down.store(true, Ordering::SeqCst);
        let outbox = Outbox::open(&root, fake.clone()).unwrap();
        assert_eq!(outbox.finishing(), vec![lease], "the lease stays held until its completion lands");
        fake.down.store(false, Ordering::SeqCst);
        outbox.flush(Duration::from_secs(5)).await;
        assert_eq!(*fake.sent.lock().unwrap(), vec!["c".to_string(), "done:late".to_string()]);
        assert!(outbox.finishing().is_empty());
        assert!(!dir.join(format!("{lease}.jsonl")).exists());
    }

    #[tokio::test]
    async fn stale_events_still_let_the_completion_through_and_completions_are_kept_once() {
        let root = root();
        let fake = Arc::new(Fake::default());
        fake.stale_events.store(true, Ordering::SeqCst);
        let outbox = Outbox::open(&root, fake.clone()).unwrap();
        let lease = Uuid::new_v4();
        let lane = outbox.lane(lease);
        lane.sink().push(event("tool", "late"));
        tokio::join!(lane.complete(completion("first")), outbox.complete(lease, completion("second")));
        assert_eq!(*fake.sent.lock().unwrap(), vec!["done:first".to_string()]);
    }
}
