pub mod commands;
pub mod config;
pub mod engine;

use std::sync::{Mutex, PoisonError};

use tauri::{Emitter, Manager, RunEvent};

use crate::commands::{Reader, STORE_CHANGED};
use crate::config::{DB_PATH, Settings, resolve_db};
use crate::engine::Engine;

/// The engine, taken and stopped on exit.
struct Running(Mutex<Option<Engine>>);

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::status,
            commands::workstreams,
            commands::workstream_costs,
            commands::needs_you,
            commands::live_graph,
            commands::audit_rollup,
            commands::audit_timeline,
            commands::audit_performance,
            commands::audit_refresh,
            commands::write_export,
        ])
        .setup(|app| {
            let data = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data)?;
            let db = resolve_db(cfg!(debug_assertions), &data, std::env::var_os(DB_PATH))?;
            let settings =
                Settings::from_env(|key| std::env::var_os(key), app.path().home_dir().ok())?;
            let handle = app.handle().clone();
            let engine = Engine::start(settings.engine(db.clone()), move || {
                let _gone = handle.emit(STORE_CHANGED, ());
            })?;
            app.manage(engine.progress());
            app.manage(engine.refresher());
            app.manage(Reader::open(&db)?);
            app.manage(Running(Mutex::new(Some(engine))));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building strate")
        .run(|app, event| {
            if let RunEvent::Exit = event
                && let Some(running) = app.try_state::<Running>()
                && let Some(engine) = running
                    .0
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
            {
                engine.stop();
            }
        });
}
