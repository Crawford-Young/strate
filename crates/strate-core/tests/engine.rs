//! The host API of `strate_core::engine`, driven the way an outside host
//! (strate's app, edge-deck) drives it: only public items.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use strate_core::engine::{Config, DEBOUNCE, Engine, REDISCOVER_AFTER};
use strate_core::registry::Runner;
use strate_core::store::Store;

/// A fresh, empty directory under the system temp dir.
fn scratch(tag: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "strate-engine-it-{}-{}-{tag}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A runner that counts its calls and returns a synthetic one-entry payload.
fn counting_runner(calls: Arc<AtomicUsize>) -> Runner {
    Box::new(move || {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok::<_, io::Error>(
            br#"[{"cwd":"/work/demo","kind":"interactive","startedAt":1700000000000,"pid":4242,"status":"busy"}]"#
                .to_vec(),
        )
    })
}

fn wait_for(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    done()
}

fn registry_row(db: &std::path::Path) -> Option<(i64, i64)> {
    let conn = Store::open_reader(db).ok()?;
    conn.query_row(
        "SELECT pid, started_at_ms FROM registry WHERE gone = 0",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .ok()
}

#[test]
fn config_new_has_the_host_defaults() {
    let root = PathBuf::from("root-dir");
    let db = PathBuf::from("store.db");
    let config = Config::new(root.clone(), db.clone());
    assert_eq!(config.root, root);
    assert_eq!(config.db, db);
    assert!(config.registry.is_none());
    assert!(config.github.is_none());
    assert!(config.hooks.is_none());
    assert_eq!(config.poll.every, Duration::from_secs(1));
    assert_eq!(DEBOUNCE, Duration::from_millis(250));
    assert_eq!(REDISCOVER_AFTER, Duration::from_secs(2));
    assert_eq!(config.debounce, DEBOUNCE);
    assert_eq!(config.rediscover_after, REDISCOVER_AFTER);
}

#[test]
fn start_creates_the_store_at_the_host_set_path_and_stop_returns() {
    let root = scratch("root");
    let db = scratch("db").join("custom-name.sqlite");
    let engine = Engine::start(Config::new(root, db.clone()), || {}).unwrap();
    assert!(db.is_file(), "store not created at {}", db.display());
    engine.stop();
}

#[test]
fn host_set_poll_interval_drives_the_registry_into_the_store() {
    let root = scratch("root");
    let db = scratch("db").join("poll.sqlite");
    let calls = Arc::new(AtomicUsize::new(0));
    let mut config = Config::new(root, db.clone());
    config.poll.every = Duration::from_millis(25);
    config.registry = Some(counting_runner(calls.clone()));
    let engine = Engine::start(config, || {}).unwrap();

    assert!(
        wait_for(Duration::from_secs(2), || calls.load(Ordering::SeqCst) >= 3),
        "runner called {} times, wanted at least 3 within 2 s",
        calls.load(Ordering::SeqCst)
    );
    assert!(
        wait_for(Duration::from_secs(5), || registry_row(&db).is_some()),
        "registry row never landed"
    );
    assert_eq!(registry_row(&db), Some((4242, 1_700_000_000_000)));
    engine.stop();
}

#[test]
fn on_change_fires_after_a_change() {
    let root = scratch("root");
    let db = scratch("db").join("change.sqlite");
    let fired = Arc::new(AtomicUsize::new(0));
    let mut config = Config::new(root, db);
    config.poll.every = Duration::from_millis(25);
    config.registry = Some(counting_runner(Arc::new(AtomicUsize::new(0))));
    let seen = fired.clone();
    let engine = Engine::start(config, move || {
        seen.fetch_add(1, Ordering::SeqCst);
    })
    .unwrap();
    assert!(
        wait_for(Duration::from_secs(5), || fired.load(Ordering::SeqCst) >= 1),
        "on_change never fired"
    );
    engine.stop();
}
