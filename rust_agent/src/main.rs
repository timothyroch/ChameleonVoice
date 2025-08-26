use log::info;    
use actix_web::{get, post, web, App, HttpResponse, HttpServer, Responder};
use actix_web::http::header;
use tokio::time::{sleep, Duration};
use std::convert::Infallible;
use actix_web::web::Bytes;
use actix_cors::Cors;
use futures_util::{stream, StreamExt};
use tokio_stream::wrappers::IntervalStream;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

mod config;
mod state;

use crate::config::Config;
use crate::state::AppState;

/// Simple health check route
#[get("/health")]
async fn health() -> impl Responder {
    "OK"
}

/// dBFS for i16 PCM slice; returns (is_voice, db)
fn vad_rms_db(pcm: &[i16]) -> (bool, f32) {
    if pcm.is_empty() { return (false, -90.0); }
    let sum_sq: f64 = pcm.iter().map(|&s| (s as f64) * (s as f64)).sum();
    let rms = (sum_sq / pcm.len() as f64).sqrt();
    let db = if rms <= 1.0 { -90.0 } else { 20.0 * (rms / 32768.0).log10() as f32 };
    let is_voice = db > -45.0; // simple threshold
    (is_voice, db)
}

fn device_arg(format: &str, device: &str) -> String {
    if format == "dshow" && !device.starts_with("audio=") {
        format!("audio={}", device)
    } else if format == "avfoundation" && !device.starts_with(':') {
        format!(":{}", device)
    } else {
        device.to_string()
    }
}


#[post("/start")]
async fn start(app: web::Data<AppState>, cfg: web::Data<Config>) -> impl Responder {
    let mut worker = app.worker.lock().await;
    if worker.is_some() { return HttpResponse::Ok().body("already running"); }

    // reset stop flag
    let _ = app.stop_tx.send(false);
    let tx = app.tx.clone();

    // capture the config values we need into the task
    let cfg_ffmpeg = (
    tx.clone(),
    app.stop_tx.subscribe(),
    cfg.ffmpeg.clone(),
    cfg.ffmpeg_format.clone(),
    cfg.ffmpeg_device.clone(),
    cfg.sample_rate,
    cfg.chunk_ms,
    );
    // Mock producer
    let handle = tokio::spawn(async move {
        let (tx, mut stop_rx, ffmpeg, format, device, sr, chunk_ms) = cfg_ffmpeg;

        // ---- ffmpeg command: microphone -> mono s16le @ sr Hz -> stdout ----
        let dev = device_arg(&format, &device);
        let mut cmd = Command::new(ffmpeg);
        cmd.args([
            "-hide_banner","-loglevel","error","-nostdin",
            "-fflags","nobuffer","-flags","low_delay","-analyzeduration","0","-probesize","32",
            "-f",&format,"-rtbufsize","100M","-thread_queue_size","1024",
            "-i",&dev,
            "-ac","1","-ar",&sr.to_string(),
            "-f","s16le","pipe:1",
        ]);

        let mut child = match cmd.stdout(std::process::Stdio::piped()).spawn() {
            Ok(c) => c,
            Err(e) => { let _ = tx.send(format!(r#"{{"error":"spawn ffmpeg","msg":"{}"}}"#, e)); return; }
        };

        let mut stdout = match child.stdout.take() {
            Some(s) => s,
            None => { let _ = tx.send(r#"{"error":"no ffmpeg stdout"}"#.to_string()); return; }
        };

        let bytes_per_sample = 2usize; // i16
        let samples_per_chunk = (sr as u64 * chunk_ms as u64 / 1000) as usize;
        let bytes_per_chunk = samples_per_chunk * bytes_per_sample;

        let mut buf = vec![0u8; bytes_per_chunk];
        let mut t: f32 = 0.0;
        loop {
            if *stop_rx.borrow() { break; }

             // read one chunk or report error/EOF
            match stdout.read_exact(&mut buf).await {
            Ok(_) => {
                // convert to i16
                let mut pcm = Vec::with_capacity(samples_per_chunk);
                for i in (0..buf.len()).step_by(2) {
                    pcm.push(i16::from_le_bytes([buf[i], buf[i + 1]]));
                }

                // simple VAD + dBFS
                let (is_voice, db) = vad_rms_db(&pcm);


            // emit one frame
            let _ = tx.send(format!(
                r#"{{"t0":{:.2},"db":{:.1},"voice":{},"sr":{},"chunk_ms":{}}}"#,
                t, db, if is_voice { "true" } else { "false" }, sr, chunk_ms
            ));
            t += (samples_per_chunk as f32) / (sr as f32);

            // wait for either: sleep or stop signal
            tokio::select! {
                _ = sleep(Duration::from_millis(0)) => {},
                _ = stop_rx.changed() => {
                    if *stop_rx.borrow() { break; }
                }
            }
        } 
         Err(e) => {
        let _ = tx.send(format!(r#"{{"error":"ffmpeg read","msg":"{}"}}"#, e));
        break;
    }
}
        }
    // best-effort terminate ffmpeg
    let _ = child.kill().await;
    });

    *worker = Some(handle);
    HttpResponse::Ok().body("started")
}

#[post("/stop")]
async fn stop(app: web::Data<AppState>) -> impl Responder {
    let _ = app.stop_tx.send(true);
    if let Some(handle) = app.worker.lock().await.take() {
        handle.abort(); 
    }
    HttpResponse::Ok().body("stopped")
}

#[get("/stream/raw")]
async fn stream_raw(app: web::Data<AppState>) -> impl Responder {
    let rx = app.tx.subscribe();

    // Build an SSE response
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        match rx.recv().await {
            Ok(line) => {
                let frame = format!("data: {}\n\n", line);
                Some((Ok::<Bytes, Infallible>(Bytes::from(frame)), rx))
            }
            Err(_) => None,
        }
    });

    // comment keepalive frames every 15s
    let ka = IntervalStream::new(tokio::time::interval(Duration::from_secs(15)))
        .map(|_| Ok::<Bytes, Infallible>(Bytes::from_static(b": keepalive\n\n")));

    // merge both streams
    let merged = stream::select(stream, ka);


    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "text/event-stream"))
        .insert_header((header::CACHE_CONTROL, "no-cache"))
        .insert_header((header::CONNECTION, "keep-alive"))
        .streaming(merged)
}

#[get("/debug/sse")]
async fn debug_sse() -> impl Responder {
    let ticks = IntervalStream::new(tokio::time::interval(Duration::from_millis(500)))
        .enumerate()
        .map(|(i, _)| {
            let frame = format!("data: {{\"tick\":{}}}\n\n", i);
            Ok::<Bytes, Infallible>(Bytes::from(frame))
        });

    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "text/event-stream"))
        .insert_header((header::CACHE_CONTROL, "no-cache"))
        .insert_header((header::CONNECTION, "keep-alive"))
        .streaming(ticks)
}


#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Init logger
    env_logger::init();
    info!("Starting Live Translator...");

    let cfg = Config::from_env();

    info!("HTTP server listening on 127.0.0.1:{}", cfg.port);

    let shared = web::Data::new(AppState::new());

    let cfg_data = web::Data::new(cfg.clone());

    HttpServer::new(move || {
        App::new()
            .app_data(cfg_data.clone()) // share Config with handlers
            .app_data(shared.clone()) // provide AppState to handlers
            .wrap(Cors::permissive()) // allow any origin for quick testing
            .service(health) 
            .service(start)            
            .service(stop)             
            .service(stream_raw)
            .service(debug_sse)
    })
    .bind(("127.0.0.1", cfg.port))?
    .run()
    .await
}
