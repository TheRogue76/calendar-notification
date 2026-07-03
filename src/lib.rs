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
//!
//! The binary (`main.rs`) is a thin wrapper over [`run`]. Exposing the modules
//! as a library lets integration tests exercise the public API.

pub mod app;
pub mod config;
pub mod engine;
pub mod google;
pub mod icon;
pub mod notify;
pub mod tray;
pub mod ui;

use iced::Task;
use tokio::sync::mpsc::unbounded_channel;
use tracing::{error, info};

use app::App;
use config::Config;
use engine::UiEvent;
use google::{auth::build_authenticator, client::GoogleClient};
use tray::CalTray;

/// Install the fail-fast panic hook: any panic on any thread logs (via the
/// default hook, keeping location/message/backtrace) then exits the process, so
/// a panic on the background engine thread can't leave a zombie (live tray, dead
/// sync/reminders). Under systemd `Restart=on-failure` this is self-healing.
pub fn install_fail_fast_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        std::process::exit(101);
    }));
}

/// Full application entry point (called by `main`).
pub fn run() -> iced::Result {
    install_fail_fast_panic_hook();

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
            // 2 workers, not one per core: the whole workload is a handful of
            // HTTP calls every poll plus the tray's D-Bus traffic. Default
            // sizing spawned 16 idle workers here (and their malloc arenas).
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    // Exit rather than return: a bare return would leave the
                    // windowless iced daemon running with no tray and no engine
                    // — an invisible zombie. Exiting lets systemd restart us.
                    error!("failed to start background runtime: {e:#}");
                    std::process::exit(1);
                }
            };
            rt.block_on(async move {
                // Bring the tray up first so the icon appears immediately, even
                // while first-run OAuth consent is happening in the browser.
                let tray = CalTray::new(bg_cmd_tx.clone()).spawn_tray().await;

                let auth = match build_authenticator(&cfg).await {
                    Ok(a) => a,
                    Err(e) => {
                        // Same fail-fast rationale as the panic hook: without an
                        // engine the daemon is an invisible zombie (no tray, no
                        // window), so exit and let systemd restart us.
                        error!("OAuth failed: {e:#}");
                        std::process::exit(1);
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
    // Small custom executor: iced's default tokio executor also spawns one
    // worker per core, which the UI (subscription bridge + window tasks)
    // doesn't remotely need.
    .executor::<app::UiExecutor>()
    .title(app::title)
    .subscription(app::subscription)
    .theme(app::theme)
    .run()
}
