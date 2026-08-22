//! Single background worker for scrollback matching and terminal replicas.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;

use phantom_emu::{
    AlacrittyCore, CompiledSearch, CursorShape, SearchError, SearchOptions, SearchOutcome,
    SearchRange, VtCore,
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
}

struct FindRequest {
    generation: u64,
    tab: u64,
    query: String,
    options: SearchOptions,
    scope: Option<SearchRange>,
    cancelled: Arc<AtomicBool>,
}

pub(crate) struct FindResponse {
    pub generation: u64,
    pub result: Result<SearchOutcome, SearchError>,
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

    fn publish(&self, response: FindResponse) {
        *self.value.lock().expect("find response mutex poisoned") = Some(response);
        self.ready.notify_one();
    }

    fn take(&self) -> Option<FindResponse> {
        self.value
            .lock()
            .expect("find response mutex poisoned")
            .take()
    }

    #[cfg(test)]
    fn take_timeout(&self, timeout: std::time::Duration) -> Option<FindResponse> {
        let value = self.value.lock().expect("find response mutex poisoned");
        let (mut value, _) = self
            .ready
            .wait_timeout_while(value, timeout, |value| value.is_none())
            .expect("find response mutex poisoned");
        value.take()
    }
}

enum CachedCompile {
    Ready(Box<CompiledSearch>),
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
    thread: Option<thread::JoinHandle<()>>,
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
        let compile_count = Arc::new(AtomicUsize::new(0));
        let worker_compile_count = Arc::clone(&compile_count);
        let thread = thread::Builder::new()
            .name("scrollback-find".to_string())
            .spawn(move || run_worker(commands, worker_response, outbox, worker_compile_count))
            .ok();
        Self {
            tx: thread.as_ref().map(|_| tx),
            response,
            thread,
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
    ) {
        self.send(FindCommand::Create {
            tab,
            rows,
            cols,
            scrollback_lines,
            cursor_shape,
        });
    }

    pub fn advance(&mut self, tab: u64, bytes: Vec<u8>, cancel_search: bool) {
        if bytes.is_empty() {
            return;
        }
        if cancel_search {
            self.cancel();
        }
        self.send(FindCommand::Advance { tab, bytes });
    }

    pub fn resize(&mut self, tab: u64, rows: u16, cols: u16) {
        self.cancel();
        self.send(FindCommand::Resize { tab, rows, cols });
    }

    pub fn set_options(&mut self, scrollback_lines: u32, cursor_shape: CursorShape) {
        self.cancel();
        self.send(FindCommand::SetOptions {
            scrollback_lines,
            cursor_shape,
        });
    }

    pub fn remove(&mut self, tab: u64) {
        self.send(FindCommand::Remove { tab });
    }

    pub fn submit(
        &mut self,
        tab: u64,
        query: String,
        options: SearchOptions,
        scope: Option<SearchRange>,
    ) -> Option<u64> {
        self.cancel();
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
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
        if tx.send(command).is_ok() {
            true
        } else {
            self.tx = None;
            false
        }
    }
}

impl Drop for FindWorker {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_worker(
    commands: mpsc::Receiver<FindCommand>,
    response: Arc<ResponseSlot>,
    outbox: Arc<dyn PtyOutbox>,
    compile_count: Arc<AtomicUsize>,
) {
    let mut replicas = HashMap::new();
    let mut cached = None;
    while let Ok(command) = commands.recv() {
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
                    cached = Some(match CompiledSearch::new(&request.query, request.options) {
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
                response.publish(FindResponse {
                    generation: request.generation,
                    result,
                });
                if !outbox.send(AppEvent::FindWake) {
                    break;
                }
            }
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

    fn worker() -> FindWorker {
        let mut worker = FindWorker::new(Arc::new(NoopOutbox));
        worker.create(7, 4, 40, 100, CursorShape::Block);
        worker
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
        let result = worker
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .result
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
            worker
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
                .result
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
            worker
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
                .result
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
            assert!(worker
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
                .result
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
        );

        assert_eq!(response.take().unwrap().generation, 2);
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
        response.publish(FindResponse {
            generation: 1,
            result: Ok(SearchOutcome::default()),
        });
        response.publish(FindResponse {
            generation: 2,
            result: Ok(SearchOutcome::default()),
        });
        assert_eq!(response.take().unwrap().generation, 2);
        assert!(response.take().is_none());
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
}
