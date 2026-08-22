//! Single background worker for scrollback matching and terminal replicas.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

use phantom_emu::{
    AlacrittyCore, CompiledSearch, CursorShape, SearchError, SearchOptions, SearchOutcome,
    SearchRange, VtCore,
};

use crate::{AppEvent, PtyOutbox};

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
    tx: mpsc::Sender<FindCommand>,
    rx: mpsc::Receiver<FindResponse>,
    generation: u64,
    cancellation: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    compile_count: Arc<AtomicUsize>,
}

impl FindWorker {
    pub fn new(outbox: Arc<dyn PtyOutbox>) -> Self {
        let (tx, commands) = mpsc::channel();
        let (responses, rx) = mpsc::channel();
        let compile_count = Arc::new(AtomicUsize::new(0));
        let worker_compile_count = Arc::clone(&compile_count);
        let _ = thread::Builder::new()
            .name("scrollback-find".to_string())
            .spawn(move || run_worker(commands, responses, outbox, worker_compile_count));
        Self {
            tx,
            rx,
            generation: 0,
            cancellation: None,
            #[cfg(test)]
            compile_count,
        }
    }

    pub fn create(
        &self,
        tab: u64,
        rows: u16,
        cols: u16,
        scrollback_lines: u32,
        cursor_shape: CursorShape,
    ) {
        let _ = self.tx.send(FindCommand::Create {
            tab,
            rows,
            cols,
            scrollback_lines,
            cursor_shape,
        });
    }

    pub fn advance(&mut self, tab: u64, bytes: &[u8], cancel_search: bool) {
        if bytes.is_empty() {
            return;
        }
        if cancel_search {
            self.cancel();
        }
        let _ = self.tx.send(FindCommand::Advance {
            tab,
            bytes: bytes.to_vec(),
        });
    }

    pub fn resize(&mut self, tab: u64, rows: u16, cols: u16) {
        self.cancel();
        let _ = self.tx.send(FindCommand::Resize { tab, rows, cols });
    }

    pub fn set_options(&mut self, scrollback_lines: u32, cursor_shape: CursorShape) {
        self.cancel();
        let _ = self.tx.send(FindCommand::SetOptions {
            scrollback_lines,
            cursor_shape,
        });
    }

    pub fn remove(&mut self, tab: u64) {
        let _ = self.tx.send(FindCommand::Remove { tab });
    }

    pub fn submit(
        &mut self,
        tab: u64,
        query: String,
        options: SearchOptions,
        scope: Option<SearchRange>,
    ) -> u64 {
        self.cancel();
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let cancelled = Arc::new(AtomicBool::new(false));
        self.cancellation = Some(Arc::clone(&cancelled));
        let _ = self.tx.send(FindCommand::Search(FindRequest {
            generation,
            tab,
            query,
            options,
            scope,
            cancelled,
        }));
        generation
    }

    pub fn cancel(&mut self) {
        if let Some(cancelled) = self.cancellation.take() {
            cancelled.store(true, Ordering::Release);
        }
    }

    pub fn try_recv(&self) -> Option<FindResponse> {
        self.rx.try_recv().ok()
    }

    #[cfg(test)]
    pub(crate) fn recv_timeout(&self, timeout: std::time::Duration) -> Option<FindResponse> {
        self.rx.recv_timeout(timeout).ok()
    }

    #[cfg(test)]
    fn compile_count(&self) -> usize {
        self.compile_count.load(Ordering::Relaxed)
    }
}

fn run_worker(
    commands: mpsc::Receiver<FindCommand>,
    responses: mpsc::Sender<FindResponse>,
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
                if responses
                    .send(FindResponse {
                        generation: request.generation,
                        result,
                    })
                    .is_err()
                {
                    continue;
                }
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
        let worker = FindWorker::new(Arc::new(NoopOutbox));
        worker.create(7, 4, 40, 100, CursorShape::Block);
        worker
    }

    #[test]
    fn replica_preserves_matches_and_reuses_exact_compile_keys() {
        let mut worker = worker();
        let bytes = "alpha ALPHA 世界\r\nwrapped alpha".as_bytes();
        worker.advance(7, bytes, true);
        worker.resize(7, 6, 18);
        let mut authoritative = AlacrittyCore::new(4, 40, 100, CursorShape::Block);
        authoritative.advance(bytes);
        authoritative.resize(6, 18);
        worker.submit(7, "alpha".into(), SearchOptions::default(), None);
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
        worker.submit(7, "alpha".into(), SearchOptions::default(), None);
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
        worker.submit(7, "alpha".into(), options, None);
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
        worker.advance(7, "wide 世界 wrapped target".repeat(4).as_bytes(), true);
        worker.resize(7, 6, 20);
        let options = SearchOptions {
            regex: true,
            ..SearchOptions::default()
        };
        for _ in 0..2 {
            worker.submit(7, "[".into(), options, None);
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
        let (response_tx, response_rx) = mpsc::channel();
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
            response_tx,
            Arc::new(NoopOutbox),
            Arc::new(AtomicUsize::new(0)),
        );

        let responses: Vec<_> = response_rx.try_iter().collect();
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].generation, 2);
    }
}
