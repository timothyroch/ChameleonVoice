use log::info;    
use actix_web::{get, post, web, App, HttpResponse, HttpServer, Responder};
use actix_web::http::header;
use tokio::time::{sleep, Duration};
use std::convert::Infallible;
use actix_web::web::Bytes;

mod config;
mod state;

use crate::config::Config;
use crate::state::AppState;

/// Simple health check route
#[get("/health")]
async fn health() -> impl Responder {
    "OK"
}

#[post("/start")]
async fn start(app: web::Data<AppState>) -> impl Responder {
    let mut worker = app.worker.lock().await;
    if worker.is_some() { return HttpResponse::Ok().body("already running"); }

    // reset stop flag
    let _ = app.stop_tx.send(false);
    let tx = app.tx.clone();
    let mut stop_rx = app.stop_tx.subscribe();

    // Mock producer
    let handle = tokio::spawn(async move {
        let mut t: f32 = 0.0;
        loop {
            if *stop_rx.borrow() { break; }

            // emit one frame
            let _ = tx.send(format!(
                r#"{{"t0":{:.1},"t1":{:.1},"text":"hello world","lang":"en"}}"#,
                t, t + 0.3
            ));
            t += 0.3;

            // wait for either: sleep or stop signal
            tokio::select! {
                _ = sleep(Duration::from_millis(300)) => {},
                _ = stop_rx.changed() => {
                    if *stop_rx.borrow() { break; }
                }
            }
        }
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


    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "text/event-stream"))
        .insert_header((header::CACHE_CONTROL, "no-cache"))
        .streaming(stream)
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Init logger
    env_logger::init();
    info!("Starting Live Translator...");

    let cfg = Config::from_env();

    info!("HTTP server listening on 127.0.0.1:{}", cfg.port);

    let shared = web::Data::new(AppState::new());

    HttpServer::new(move || {
        App::new()
            .app_data(shared.clone()) // provide AppState to handlers
            .service(health) 
            .service(start)            
            .service(stop)             
            .service(stream_raw)       
    })
    .bind(("127.0.0.1", cfg.port))?
    .run()
    .await
}
