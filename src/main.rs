//! calendar-notification
//!
//! A system-tray Google Calendar reminder daemon with an on-demand agenda /
//! add-event widget for Ubuntu/GNOME.
//!
//! Architecture: the iced daemon owns the main thread (winit event loop, widget
//! window). A dedicated background tokio runtime owns the tray, OAuth, calendar
//! sync, and the reminder scheduler. The two communicate over channels:
//!   - engine → UI  via `UnboundedReceiver<UiEvent>` (bridged to a subscription)
//!   - UI → engine  via `UnboundedSender<Command>`

mod app;
mod config;
mod engine;
mod google;
mod notify;
mod tray;
mod ui;

use iced::Task;
use tokio::sync::mpsc::unbounded_channel;
use tracing::{error, info};

use app::App;
use config::Config;
use engine::UiEvent;
use google::{auth::build_authenticator, client::GoogleClient};
use tray::CalTray;

fn main() -> iced::Result {
    // Select the rustls crypto provider explicitly. Multiple providers may be
    // compiled in transitively (ring + aws-lc-rs), in which case rustls refuses
    // to auto-pick and panics on first TLS use — installing one here is the
    // documented, deterministic fix.
    let _ = rustls::crypto::ring::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "calendar_notification=info".into()),
        )
        .init();

    let cfg = match Config::load_or_create() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to load config: {e:#}");
            return Ok(());
        }
    };

    if !cfg.has_credentials() {
        let path = config::config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        eprintln!(
            "\nNo Google OAuth credentials configured yet.\n\
             Edit {path}\n\
             and set `client_id` and `client_secret` (see README.md → Google Cloud setup),\n\
             then run `calendar-notification` again.\n"
        );
        return Ok(());
    }

    // Channels between the UI (main thread) and the engine (background runtime).
    let (cmd_tx, cmd_rx) = unbounded_channel::<engine::Command>();
    let (ui_tx, ui_rx) = unbounded_channel::<UiEvent>();
    app::install_ui_receiver(ui_rx);

    // Background runtime: tray + OAuth + sync + scheduler.
    let bg_cmd_tx = cmd_tx.clone();
    std::thread::Builder::new()
        .name("engine".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    error!("failed to start background runtime: {e:#}");
                    return;
                }
            };
            rt.block_on(async move {
                // Bring the tray up first so the icon appears immediately, even
                // while first-run OAuth consent is happening in the browser.
                let tray = CalTray::new(bg_cmd_tx.clone()).spawn_tray().await;

                let auth = match build_authenticator(&cfg).await {
                    Ok(a) => a,
                    Err(e) => {
                        error!("OAuth failed: {e:#}");
                        let _ = ui_tx.send(UiEvent::Status("Auth failed — see logs".into()));
                        return;
                    }
                };
                let client = GoogleClient::new(auth);
                info!("engine starting");
                engine::run(cfg, client, ui_tx, cmd_rx, tray).await;
                info!("engine stopped; exiting");
                std::process::exit(0);
            });
        })
        .expect("spawn engine thread");

    // Main thread: run the iced daemon (starts with no window; the tray opens it).
    let boot_tx = cmd_tx.clone();
    iced::daemon(
        move || (App::new(boot_tx.clone()), Task::none()),
        app::update,
        app::view,
    )
    .title(app::title)
    .subscription(app::subscription)
    .theme(app::theme)
    .run()
}
