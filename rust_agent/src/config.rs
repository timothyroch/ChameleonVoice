use std::env;

/// Centralized application settings
#[derive(Clone)]
pub struct Config {
    pub port: u16,
    pub ffmpeg: String,
    pub ffmpeg_format: String,
    pub ffmpeg_device: String,
    pub sample_rate: u32,
    pub chunk_ms: u32,
    pub whisper_model: String,
    pub whisper_threads: usize,
    pub translate_base: String,

}

impl Config {
    /// Load configuration from environment variables (with defaults)
    pub fn from_env() -> Self {
        let port = env::var("PORT")
            .unwrap_or_else(|_| "8080".to_string())
            .parse::<u16>()
            .unwrap_or(8080);

        // ffmpeg settings (safe defaults; override via env)
        let ffmpeg = env::var("FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string());
        let ffmpeg_format = env::var("FFMPEG_FORMAT").unwrap_or_else(|_| {
            if cfg!(target_os = "windows") { "dshow".into() }
            else if cfg!(target_os = "macos") { "avfoundation".into() }
            else { "pulse".into() }
        });
        let ffmpeg_device = env::var("FFMPEG_DEVICE").unwrap_or_else(|_| "default".to_string());

        // audio params
        let sample_rate = env::var("SAMPLE_RATE").ok().and_then(|s| s.parse().ok()).unwrap_or(16_000);
        let chunk_ms    = env::var("CHUNK_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(300);
        let whisper_model = env::var("WHISPER_MODEL")
            .unwrap_or_else(|_| "./models/ggml-base.en.bin".to_string());
        let whisper_threads = env::var("WHISPER_THREADS").ok().and_then(|s| s.parse().ok())
            .unwrap_or_else(|| num_cpus::get().max(1));

        // translation
        let translate_base = env::var("TRANSLATE_BASE")
            .unwrap_or_else(|_| "http://127.0.0.1:5000".to_string());

        Self { 
            port,
            ffmpeg,
            ffmpeg_format,
            ffmpeg_device,
            sample_rate,
            chunk_ms,
            whisper_model, 
            whisper_threads,
            translate_base,
        }
    }
}
