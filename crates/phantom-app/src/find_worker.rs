//! Single background worker for scrollback matching and terminal replicas.

use std::collections::HashMap;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;

use phantom_emu::{
    AlacrittyCore, CancellableCompiledSearch, CursorShape, SearchError, SearchOptions,
    SearchOutcome, SearchRange, VtCore,
};

use crate::{AppEvent, PtyOutbox};

const FIND_COMMAND_CAPACITY: usize = 16;

enum FindCommand {
    Create {
        tab: u64,
        rows: u16,
        cols: u16,
        scrollback_lines: u32,
        cursor_shape: CursorShape,
    },
    Advance {
        tab: u64,
        bytes: Vec<u8>,
    },
    Resize {
        tab: u64,
        rows: u16,
        cols: u16,
    },
    SetOptions {
        scrollback_lines: u32,
        cursor_shape: CursorShape,
    },
    Remove {
        tab: u64,
    },
    Search(FindRequest),
    #[cfg(test)]
    Pause {
        entered: mpsc::Sender<()>,
        release: Arc<std::sync::Barrier>,
    },
    #[cfg(test)]
    Panic,
}

struct FindRequest {
    generation: u64,
    tab: u64,
    query: String,
    options: SearchOptions,
    scope: Option<SearchRange>,
    cancelled: Arc<AtomicBool>,
}

pub(crate) enum FindResponse {
    Completed {
        generation: u64,
        result: Result<SearchOutcome, SearchError>,
    },
    WorkerFailed,
}

struct ResponseSlot {
    value: Mutex<Option<FindResponse>>,
    ready: Condvar,
}

impl ResponseSlot {
    fn new() -> Self {
        Self {
            value: Mutex::new(None),
            ready: Condvar::new(),
        }
    }

    fn publish(&self, response: FindResponse) -> bool {
        let mut value = self
            .value
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let should_wake = value.is_none();
        *value = Some(response);
        if should_wake {
            self.ready.notify_one();
        }
        should_wake
    }

    fn take(&self) -> Option<FindResponse> {
        self.value
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    #[cfg(test)]
    fn take_timeout(&self, timeout: std::time::Duration) -> Option<FindResponse> {
        let value = self
            .value
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (mut value, _) = self
            .ready
            .wait_timeout_while(value, timeout, |value| value.is_none())
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        value.take()
    }
}

enum CachedCompile {
    Ready(Box<CancellableCompiledSearch>),
    Invalid {
        query: String,
        options: SearchOptions,
        error: SearchError,
    },
}

impl CachedCompile {
    fn matches(&self, query: &str, options: SearchOptions) -> bool {
        match self {
            Self::Ready(compiled) => compiled.matches(query, options),
            Self::Invalid {
                query: cached,
                options: cached_options,
                ..
            } => cached == query && *cached_options == options,
        }
    }
}

pub(crate) struct FindWorker {
    tx: Option<mpsc::SyncSender<FindCommand>>,
    response: Arc<ResponseSlot>,
    shutdown: Arc<AtomicBool>,
    generation: u64,
    cancellation: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    compile_count: Arc<AtomicUsize>,
}

impl FindWorker {
    pub fn new(outbox: Arc<dyn PtyOutbox>) -> Self {
        let (tx, commands) = mpsc::sync_channel(FIND_COMMAND_CAPACITY);
        let response = Arc::new(ResponseSlot::new());
        let worker_response = Arc::clone(&response);
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let compile_count = Arc::new(AtomicUsize::new(0));
        let worker_compile_count = Arc::clone(&compile_count);
        let failure_response = Arc::clone(&response);
        let failure_outbox = Arc::clone(&outbox);
        let thread = thread::Builder::new()
            .name("scrollback-find".to_string())
            .spawn(move || {
                if panic::catch_unwind(AssertUnwindSafe(|| {
                    run_worker(
                        commands,
                        worker_response,
                        outbox,
                        worker_compile_count,
                        worker_shutdown,
                    )
                }))
                .is_err()
                    && failure_response.publish(FindResponse::WorkerFailed)
                {
                    let _ = failure_outbox.send(AppEvent::FindWake);
                }
            })
            .ok();
        Self {
            tx: thread.as_ref().map(|_| tx),
            response,
            shutdown,
            generation: 0,
            cancellation: None,
            #[cfg(test)]
            compile_count,
        }
    }

    pub fn create(
        &mut self,
        tab: u64,
        rows: u16,
        cols: u16,
        scrollback_lines: u32,
        cursor_shape: CursorShape,
    ) -> bool {
        self.send(FindCommand::Create {
            tab,
            rows,
            cols,
            scrollback_lines,
            cursor_shape,
        })
    }

    pub fn advance(&mut self, tab: u64, bytes: Vec<u8>, cancel_search: bool) -> bool {
        if bytes.is_empty() {
            return self.available();
        }
        if cancel_search {
            self.cancel();
        }
        self.send(FindCommand::Advance { tab, bytes })
    }

    pub fn resize(&mut self, tab: u64, rows: u16, cols: u16) -> bool {
        self.cancel();
        self.send(FindCommand::Resize { tab, rows, cols })
    }

    pub fn set_options(&mut self, scrollback_lines: u32, cursor_shape: CursorShape) -> bool {
        self.cancel();
        self.send(FindCommand::SetOptions {
            scrollback_lines,
            cursor_shape,
        })
    }

    pub fn remove(&mut self, tab: u64) -> bool {
        self.send(FindCommand::Remove { tab })
    }

    pub fn submit(
        &mut self,
        tab: u64,
        query: String,
        options: SearchOptions,
        scope: Option<SearchRange>,
    ) -> Option<u64> {
        self.cancel();
        let Some(generation) = self.generation.checked_add(1) else {
            self.disable();
            return None;
        };
        self.generation = generation;
        let cancelled = Arc::new(AtomicBool::new(false));
        self.cancellation = Some(Arc::clone(&cancelled));
        if !self.send(FindCommand::Search(FindRequest {
            generation,
            tab,
            query,
            options,
            scope,
            cancelled,
        })) {
            self.cancellation = None;
            return None;
        }
        Some(generation)
    }

    pub fn cancel(&mut self) {
        if let Some(cancelled) = self.cancellation.take() {
            cancelled.store(true, Ordering::Release);
        }
    }

    pub fn try_recv(&self) -> Option<FindResponse> {
        self.response.take()
    }

    #[cfg(test)]
    pub(crate) fn recv_timeout(&self, timeout: std::time::Duration) -> Option<FindResponse> {
        self.response.take_timeout(timeout)
    }

    #[cfg(test)]
    fn compile_count(&self) -> usize {
        self.compile_count.load(Ordering::Relaxed)
    }

    fn send(&mut self, command: FindCommand) -> bool {
        let Some(tx) = &self.tx else {
            return false;
        };
        match tx.try_send(command) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Full(_)) | Err(mpsc::TrySendError::Disconnected(_)) => {
                self.disable();
                false
            }
        }
    }

    fn disable(&mut self) {
        self.cancel();
        self.shutdown.store(true, Ordering::Release);
        self.tx = None;
    }

    pub fn available(&self) -> bool {
        self.tx.is_some()
    }

    pub fn mark_unavailable(&mut self) {
        self.disable();
    }
}

impl Drop for FindWorker {
    fn drop(&mut self) {
        self.disable();
    }
}

fn run_worker(
    commands: mpsc::Receiver<FindCommand>,
    response: Arc<ResponseSlot>,
    outbox: Arc<dyn PtyOutbox>,
    compile_count: Arc<AtomicUsize>,
    shutdown: Arc<AtomicBool>,
) {
    let mut replicas = HashMap::new();
    let mut cached = None;
    while let Ok(command) = commands.recv() {
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        match command {
            FindCommand::Create {
                tab,
                rows,
                cols,
                scrollback_lines,
                cursor_shape,
            } => {
                replicas.insert(
                    tab,
                    AlacrittyCore::new(rows, cols, scrollback_lines, cursor_shape),
                );
            }
            FindCommand::Advance { tab, bytes } => {
                if let Some(core) = replicas.get_mut(&tab) {
                    core.advance(&bytes);
                    let _ = core.take_pty_output();
                }
            }
            FindCommand::Resize { tab, rows, cols } => {
                if let Some(core) = replicas.get_mut(&tab) {
                    core.resize(rows, cols);
                }
            }
            FindCommand::SetOptions {
                scrollback_lines,
                cursor_shape,
            } => {
                for core in replicas.values_mut() {
                    core.set_terminal_options(scrollback_lines, cursor_shape);
                }
            }
            FindCommand::Remove { tab } => {
                replicas.remove(&tab);
            }
            FindCommand::Search(request) => {
                if request.cancelled.load(Ordering::Acquire) {
                    continue;
                }
                if !cached.as_ref().is_some_and(|entry: &CachedCompile| {
                    entry.matches(&request.query, request.options)
                }) {
                    compile_count.fetch_add(1, Ordering::Relaxed);
                    let compile = CancellableCompiledSearch::new_cancellable(
                        &request.query,
                        request.options,
                        &request.cancelled,
                    );
                    if request.cancelled.load(Ordering::Acquire) {
                        continue;
                    }
                    cached = Some(match compile {
                        Ok(compiled) => CachedCompile::Ready(Box::new(compiled)),
                        Err(error) => CachedCompile::Invalid {
                            query: request.query.clone(),
                            options: request.options,
                            error,
                        },
                    });
                }
                let result = match (replicas.get(&request.tab), cached.as_mut()) {
                    (Some(core), Some(CachedCompile::Ready(compiled))) => core
                        .search_compiled(compiled, request.scope, &request.cancelled)
                        .map(Ok),
                    (_, Some(CachedCompile::Invalid { error, .. })) => Some(Err(error.clone())),
                    (None, _) => Some(Ok(SearchOutcome::default())),
                    _ => None,
                };
                let Some(result) = result else {
                    continue;
                };
                if request.cancelled.load(Ordering::Acquire) {
                    continue;
                }
                let should_wake = response.publish(FindResponse::Completed {
                    generation: request.generation,
                    result,
                });
                if should_wake && !outbox.send(AppEvent::FindWake) {
                    break;
                }
            }
            #[cfg(test)]
            FindCommand::Pause { entered, release } => {
                let _ = entered.send(());
                release.wait();
            }
            #[cfg(test)]
            FindCommand::Panic => panic!("injected find worker panic"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoopOutbox;
    impl PtyOutbox for NoopOutbox {
        fn send(&self, _event: AppEvent) -> bool {
            true
        }
    }

    struct CountingOutbox(Arc<AtomicUsize>);
    impl PtyOutbox for CountingOutbox {
        fn send(&self, _event: AppEvent) -> bool {
            self.0.fetch_add(1, Ordering::Relaxed);
            true
        }
    }

    fn worker() -> FindWorker {
        let mut worker = FindWorker::new(Arc::new(NoopOutbox));
        worker.create(7, 4, 40, 100, CursorShape::Block);
        worker
    }

    fn completed(response: FindResponse) -> (u64, Result<SearchOutcome, SearchError>) {
        match response {
            FindResponse::Completed { generation, result } => (generation, result),
            FindResponse::WorkerFailed => panic!("find worker unexpectedly failed"),
        }
    }

    #[test]
    fn replica_preserves_matches_and_reuses_exact_compile_keys() {
        let mut worker = worker();
        let bytes = "alpha ALPHA 世界\r\nwrapped alpha".as_bytes();
        worker.advance(7, bytes.to_vec(), true);
        worker.resize(7, 6, 18);
        let mut authoritative = AlacrittyCore::new(4, 40, 100, CursorShape::Block);
        authoritative.advance(bytes);
        authoritative.resize(6, 18);
        worker
            .submit(7, "alpha".into(), SearchOptions::default(), None)
            .unwrap();
        let result = completed(
            worker
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
        )
        .1
        .unwrap();
        assert_eq!(
            result,
            authoritative
                .search_scrollback("alpha", SearchOptions::default(), None)
                .unwrap()
        );
        worker
            .submit(7, "alpha".into(), SearchOptions::default(), None)
            .unwrap();
        assert_eq!(
            completed(
                worker
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap(),
            )
            .1
            .unwrap()
            .matches
            .len(),
            3
        );
        assert_eq!(worker.compile_count(), 1);
        let options = SearchOptions {
            case_sensitive: true,
            ..SearchOptions::default()
        };
        worker.submit(7, "alpha".into(), options, None).unwrap();
        assert_eq!(
            completed(
                worker
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap(),
            )
            .1
            .unwrap()
            .matches
            .len(),
            2
        );
        assert_eq!(worker.compile_count(), 2);
    }

    #[test]
    fn replica_tracks_mutations_and_caches_invalid_regex() {
        let mut worker = worker();
        worker.advance(7, "wide 世界 wrapped target".repeat(4).into_bytes(), true);
        worker.resize(7, 6, 20);
        let options = SearchOptions {
            regex: true,
            ..SearchOptions::default()
        };
        for _ in 0..2 {
            worker.submit(7, "[".into(), options, None).unwrap();
            assert!(completed(
                worker
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap()
            )
            .1
            .is_err());
        }
        assert_eq!(worker.compile_count(), 1);
    }

    #[test]
    fn newer_request_cancels_older_generation() {
        let (command_tx, command_rx) = mpsc::channel();
        let response = Arc::new(ResponseSlot::new());
        command_tx
            .send(FindCommand::Create {
                tab: 7,
                rows: 4,
                cols: 40,
                scrollback_lines: 100,
                cursor_shape: CursorShape::Block,
            })
            .unwrap();
        command_tx
            .send(FindCommand::Advance {
                tab: 7,
                bytes: b"alpha beta".to_vec(),
            })
            .unwrap();
        let cancelled = Arc::new(AtomicBool::new(true));
        command_tx
            .send(FindCommand::Search(FindRequest {
                generation: 1,
                tab: 7,
                query: "alpha".into(),
                options: SearchOptions::default(),
                scope: None,
                cancelled,
            }))
            .unwrap();
        command_tx
            .send(FindCommand::Search(FindRequest {
                generation: 2,
                tab: 7,
                query: "beta".into(),
                options: SearchOptions::default(),
                scope: None,
                cancelled: Arc::new(AtomicBool::new(false)),
            }))
            .unwrap();
        drop(command_tx);

        run_worker(
            command_rx,
            Arc::clone(&response),
            Arc::new(NoopOutbox),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicBool::new(false)),
        );

        assert_eq!(completed(response.take().unwrap()).0, 2);
        assert!(response.take().is_none());
    }

    #[test]
    fn command_queue_and_response_storage_are_bounded() {
        let (tx, _rx) = mpsc::sync_channel(FIND_COMMAND_CAPACITY);
        for tab in 0..FIND_COMMAND_CAPACITY as u64 {
            tx.try_send(FindCommand::Remove { tab }).unwrap();
        }
        assert!(matches!(
            tx.try_send(FindCommand::Remove { tab: u64::MAX }),
            Err(mpsc::TrySendError::Full(_))
        ));

        let response = ResponseSlot::new();
        assert!(response.publish(FindResponse::Completed {
            generation: 1,
            result: Ok(SearchOutcome::default()),
        }));
        assert!(!response.publish(FindResponse::Completed {
            generation: 2,
            result: Ok(SearchOutcome::default()),
        }));
        assert_eq!(completed(response.take().unwrap()).0, 2);
        assert!(response.take().is_none());
    }

    #[test]
    fn overwriting_a_pending_response_emits_only_one_wake() {
        let (command_tx, command_rx) = mpsc::channel();
        let response = Arc::new(ResponseSlot::new());
        let wakes = Arc::new(AtomicUsize::new(0));
        command_tx
            .send(FindCommand::Create {
                tab: 7,
                rows: 4,
                cols: 40,
                scrollback_lines: 100,
                cursor_shape: CursorShape::Block,
            })
            .unwrap();
        for generation in 1..=3 {
            command_tx
                .send(FindCommand::Search(FindRequest {
                    generation,
                    tab: 7,
                    query: format!("query-{generation}"),
                    options: SearchOptions::default(),
                    scope: None,
                    cancelled: Arc::new(AtomicBool::new(false)),
                }))
                .unwrap();
        }
        drop(command_tx);

        run_worker(
            command_rx,
            Arc::clone(&response),
            Arc::new(CountingOutbox(Arc::clone(&wakes))),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicBool::new(false)),
        );

        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        assert_eq!(completed(response.take().unwrap()).0, 3);
    }

    #[test]
    fn failed_command_submission_is_reported() {
        let mut worker = worker();
        worker.tx = None;
        assert!(worker
            .submit(7, "target".into(), SearchOptions::default(), None)
            .is_none());
        assert!(worker.cancellation.is_none());
    }

    #[test]
    fn saturated_queue_fails_closed_without_blocking_the_caller() {
        let mut worker = worker();
        let release = Arc::new(std::sync::Barrier::new(2));
        let (entered_tx, entered_rx) = mpsc::channel();
        assert!(worker.send(FindCommand::Pause {
            entered: entered_tx,
            release: Arc::clone(&release),
        }));
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        for tab in 0..FIND_COMMAND_CAPACITY as u64 {
            assert!(worker.remove(tab));
        }

        let started = std::time::Instant::now();
        assert!(!worker.remove(u64::MAX));
        assert!(started.elapsed() < std::time::Duration::from_millis(50));
        assert!(!worker.available());
        release.wait();
    }

    #[test]
    fn drop_does_not_wait_for_a_paused_worker() {
        let mut worker = worker();
        let release = Arc::new(std::sync::Barrier::new(2));
        let (entered_tx, entered_rx) = mpsc::channel();
        assert!(worker.send(FindCommand::Pause {
            entered: entered_tx,
            release: Arc::clone(&release),
        }));
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();

        let started = std::time::Instant::now();
        drop(worker);
        assert!(started.elapsed() < std::time::Duration::from_millis(50));
        release.wait();
    }

    #[test]
    fn worker_panic_publishes_a_failure_response() {
        let mut worker = worker();
        assert!(worker.send(FindCommand::Panic));
        assert!(matches!(
            worker.recv_timeout(std::time::Duration::from_secs(2)),
            Some(FindResponse::WorkerFailed)
        ));
    }
}
