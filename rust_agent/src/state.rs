use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

/// Shared app state for SSE + background worker
pub struct AppState {
    pub tx: broadcast::Sender<String>,                 
    pub worker: tokio::sync::Mutex<Option<JoinHandle<()>>>, 
    pub stop_tx: watch::Sender<bool>,                  
}

impl AppState {
    /// Initialize channels and empty worker slot
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(256);

        // false = keep running; set true to request shutdown
        let (stop_tx, _stop_rx) = watch::channel(false);

        Self {
            tx,
            worker: tokio::sync::Mutex::new(None),
            stop_tx,
        }
    }
}
