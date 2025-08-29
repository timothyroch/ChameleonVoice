use std::sync::Arc;
use tokio::sync::mpsc;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext};
use std::sync::Mutex;
use tokio::task;
use std::collections::BTreeMap;

#[derive(Debug)]
pub struct AsrJob {
    pub seq: u64,
    pub t0: f32,
    pub t1: f32,
    pub pcm_f32: Vec<f32>,
}

#[derive(Debug)]
struct AsrRes {
    seq: u64,
    t0: f32,
    t1: f32,
    text: String,
}

/// Spawns a parallel ASR pipeline and an in-order collector
// ctx : shared WhisperContext
// threads_per_state: ggml threads each worker will use
// workers: number of worker states to spawn
// tx_sse: broadcast to emit JSON lines to SSE

pub fn spawn_parallel_asr(
    ctx: Arc<WhisperContext>,
    threads_per_state: i32,
    workers: usize,
    tx_sse: tokio::sync::broadcast::Sender<String>,
    lang_force: Option<String>,
) -> mpsc::Sender<AsrJob>{
    let (job_tx, mut job_rx) = mpsc::channel::<AsrJob>(256);
    let rx_shared = Arc::new(Mutex::new(job_rx));
    let (res_tx, mut res_rx) = mpsc::channel::<AsrRes>(256);

    // Workers
    for _ in 0..workers.max(1) {
        let ctx2 = ctx.clone();
        let res_tx2 = res_tx.clone();
        let rx2 = rx_shared.clone();
        let lang_force2 = lang_force.clone();
        task::spawn_blocking(move || {
            let mut wstate = match ctx2.create_state() {
                Ok(s) => s,
                Err(e) => {
                    let _ = res_tx2.blocking_send(AsrRes {
                        seq: u64::MAX,t0: 0.0, t1: 0.0,
                        text:format!("[Whisper state error: {}]", e)
                    });
                    return ;
                }
            };

            while let Some(job) = rx2.lock().unwrap().blocking_recv() {
                let AsrJob { seq, t0, t1, pcm_f32 } = job;

                let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
                params.set_n_threads(threads_per_state);
                params.set_translate(false);
                params.set_language(Some("en"));       
                params.set_no_timestamps(true);
                params.set_print_special(false);
                params.set_print_progress(false);
                params.set_print_realtime(false);
                params.set_no_context(true);
                params.set_single_segment(true);
                params.set_temperature(0.0);
                params.set_max_len(120);

                let mut text_out = String::new();

                if let Err(e) = wstate.full(params, &pcm_f32) {
                    text_out = format!("whisper error: {}", e)
                } else if let Ok(n) = wstate.full_n_segments() {
                    for i in 0..n {
                        if let Ok(seg) = wstate.full_get_segment_text(i) {
                            text_out.push_str(&seg);
                        }                        
                    }
                    text_out = text_out.trim().to_string();
                }

                let _ = res_tx2.blocking_send(AsrRes { seq, t0, t1, text: text_out });
            }
        });
    }
    drop(res_tx);

    // In-order collector
    let tx_for_sse = tx_sse.clone();
    tokio::spawn(async move {
        let mut next_seq: u64 = 0;
        let mut buffer: BTreeMap<u64, AsrRes> = BTreeMap::new();
        while let Some(r) = res_rx.recv().await {
            buffer.insert(r.seq, r);
            while let Some(r2) = buffer.remove(&next_seq) {
                if !r2.text.is_empty() {
                    let _ = tx_for_sse.send(format!(
                        r#"{{"t0":{:.2},"t1":{:.2},"text":"{}","lang":"und"}}"#,
                        r2.t0, r2.t1, r2.text.replace('"', "\\\"") 
                    ));
                }
                next_seq = next_seq.saturating_add(1);
            }
        }
    });

    job_tx

}
