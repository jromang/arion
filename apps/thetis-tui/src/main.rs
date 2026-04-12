//! Thetis-tui entry point: console SDR control via ratatui.

fn main() -> anyhow::Result<()> {
    // Log to file instead of stderr (terminal is owned by ratatui).
    let log_dir = directories::ProjectDirs::from("rs", "thetis", "thetis")
        .map(|p| p.cache_dir().to_path_buf())
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&log_dir)?;
    let log_file = std::fs::File::create(log_dir.join("thetis-tui.log"))?;
    tracing_subscriber::fmt()
        .with_writer(log_file)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!("starting thetis-tui");

    let opts = thetis_app::AppOptions {
        radio_ip_override: std::env::var("HL2_IP").ok(),
    };
    let mut view = thetis_tui::TuiView::new(opts);
    view.run()
}
