use std::env;

/// Centralized application settings
pub struct Config {
    pub port: u16,
}

impl Config {
    /// Load configuration from environment variables (with defaults)
    pub fn from_env() -> Self {
        let port = env::var("PORT")
            .unwrap_or_else(|_| "8080".to_string())
            .parse::<u16>()
            .unwrap_or(8080);

        Self { port }
    }
}
