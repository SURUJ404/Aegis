//! Global `tracing` initialization.

use lq_core::config::TelemetryConfig;

/// Install a global `tracing` subscriber configured from
/// [`TelemetryConfig`](lq_core::config::TelemetryConfig). Returns an error if a
/// subscriber is already installed (call once at process start).
pub fn init_logging(cfg: &TelemetryConfig) -> anyhow::Result<()> {
    let filter = tracing_subscriber::EnvFilter::builder().parse_lossy(&cfg.log_level);

    if cfg.json_logs {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .json()
            .with_writer(std::io::stdout)
            .try_init()
            .map_err(|e| anyhow::anyhow!(e))?;
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .with_writer(std::io::stdout)
            .try_init()
            .map_err(|e| anyhow::anyhow!(e))?;
    }
    tracing::info!(level = %cfg.log_level, json = cfg.json_logs, "logging initialized");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_subscriber_once() {
        let cfg = TelemetryConfig {
            log_level: "error".into(),
            ..TelemetryConfig::default()
        };
        init_logging(&cfg).expect("first init succeeds");
        // A second install must be refused rather than silently overriding.
        assert!(init_logging(&cfg).is_err());
    }
}