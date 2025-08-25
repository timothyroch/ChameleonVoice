use std::env;
use log::{info, error};
use env_logger;
use actix_web::{get, App, HttpServer, Responder};

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

    // Read port from env or default
    let port = env::var("PORT")
        .unwrap_or_else(|_| "8080".to_string())
        .parse::<u16>()
        .unwrap_or(8080);

    info!("HTTP server listening on 127.0.0.1:{}", port);

    HttpServer::new(|| {
        App::new()
            .service(health) // just /health endpoint for now
    })
    .bind(("127.0.0.1", port))?
    .run()
    .await
}
