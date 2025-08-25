use log::info;
mod config; 
use actix_web::{get, App, HttpServer, Responder};
use crate::config::Config;

/// Simple health check route
#[get("/health")]
async fn health() -> impl Responder {
    "OK"
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Init logger
    env_logger::init();
    info!("Starting Live Translator...");

    let cfg = Config::from_env();

    info!("HTTP server listening on 127.0.0.1:{}", cfg.port);

    HttpServer::new(|| {
        App::new()
            .service(health) // just /health endpoint for now
    })
    .bind(("127.0.0.1", cfg.port))?
    .run()
    .await
}
