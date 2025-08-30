use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::{broadcast, watch, Mutex};
use tokio::task::JoinHandle;

/// Shared app state for SSE + background worker
pub struct AppState {
    pub tx: broadcast::Sender<String>,                 
    pub worker: tokio::sync::Mutex<Option<JoinHandle<()>>>, 
    pub stop_tx: watch::Sender<bool>,  
    pub speaker_buf: Arc<Mutex<VecDeque<i16>>>,
    pub pause_tx: tokio::sync::watch::Sender<bool>,                
}

impl AppState {
    /// Initialize channels and empty worker slot
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(256);

        // false = keep running; set true to request shutdown
        let (stop_tx, _stop_rx) = watch::channel(false);
        let (pause_tx, _) = tokio::sync::watch::channel(false);

        Self {
            tx,
            pause_tx,
            worker: tokio::sync::Mutex::new(None),
            stop_tx,
            speaker_buf: Arc::new(Mutex::new(VecDeque::new())),
        }
    }
}
