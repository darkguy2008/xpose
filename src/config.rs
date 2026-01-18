use std::fs;
use std::time::Duration;

/// Application configuration loaded from ~/.xposerc
pub struct Config {
    pub entrance_ms: u64,
    pub exit_ms: u64,
    pub animation_speed: f64,
    /// WM_CLASS values to exclude from the exposé view
    pub exclude_classes: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            entrance_ms: 350,
            exit_ms: 350,
            animation_speed: 1.0,
            exclude_classes: Vec::new(),
        }
    }
}

impl Config {
    /// Load configuration from ~/.xposerc
    /// Falls back to defaults if file doesn't exist or has parse errors.
    pub fn load() -> Self {
        let mut config = Self::default();

        let path = match dirs::home_dir() {
            Some(home) => home.join(".xposerc"),
            None => return config,
        };

        let contents = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => return config,
        };

        for line in contents.lines() {
            let line = line.trim();

            // Skip empty lines and comments
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Parse "Key Value" format
            let mut parts = line.split_whitespace();
            let key = match parts.next() {
                Some(k) => k,
                None => continue,
            };
            let value = match parts.next() {
                Some(v) => v,
                None => continue,
            };

            match key {
                "EntranceMs" => {
                    if let Ok(ms) = value.parse::<u64>() {
                        config.entrance_ms = ms;
                        log::debug!("Config: EntranceMs = {}", ms);
                    }
                }
                "ExitMs" => {
                    if let Ok(ms) = value.parse::<u64>() {
                        config.exit_ms = ms;
                        log::debug!("Config: ExitMs = {}", ms);
                    }
                }
                "AnimationSpeed" => {
                    if let Ok(speed) = value.parse::<f64>() {
                        if speed > 0.0 {
                            config.animation_speed = speed;
                            log::debug!("Config: AnimationSpeed = {}", speed);
                        }
                    }
                }
                "ExcludeClass" => {
                    config.exclude_classes.push(value.to_string());
                    log::debug!("Config: ExcludeClass = {}", value);
                }
                _ => {
                    log::debug!("Config: unknown key '{}'", key);
                }
            }
        }

        config
    }

    pub fn entrance_duration(&self) -> Duration {
        Duration::from_millis(self.entrance_ms)
    }

    pub fn exit_duration(&self) -> Duration {
        Duration::from_millis(self.exit_ms)
    }
}
