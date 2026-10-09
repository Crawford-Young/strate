//! The app's settings from the environment. Pure: the caller passes the
//! env lookup and home dir.
//!
//! The hook receiver is opt-in: it starts only when `STRATE_HOOK_PORT`
//! names a port, binding 127.0.0.1 there. `STRATE_HOOK_TOKEN`, when set,
//! is the bearer token every hook must carry (the README's hook snippet
//! sends the same variable). strate never edits Claude Code's settings.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use strate_core::discovery::resolve_config_dir;
use strate_core::hooks::HookOptions;
use strate_core::registry::{PollOptions, cli_runner, resolve_claude};
use strate_core::tail::TailOptions;

use crate::engine::Config;

pub const HOOK_PORT: &str = "STRATE_HOOK_PORT";
pub const HOOK_TOKEN: &str = "STRATE_HOOK_TOKEN";

/// At most four `store-changed` events a second.
pub const DEBOUNCE: Duration = Duration::from_millis(250);
/// Shortest gap between discovery passes for new subagents.
pub const REDISCOVER_AFTER: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct Settings {
    /// The Claude Code config dir.
    pub root: PathBuf,
    /// The `claude` binary; `None` leaves the registry off.
    pub claude: Option<PathBuf>,
    /// `Some` when the hook receiver is opted in.
    pub hooks: Option<HookOptions>,
}

impl Settings {
    pub fn from_env(
        env: impl Fn(&str) -> Option<OsString>,
        home: Option<PathBuf>,
    ) -> Result<Self, String> {
        let root = resolve_config_dir(env("CLAUDE_CONFIG_DIR"), home.clone())
            .ok_or("no home dir to find ~/.claude in, and CLAUDE_CONFIG_DIR is unset")?;
        Ok(Self {
            root,
            claude: resolve_claude(None, env("PATH"), home),
            hooks: hook_options(env(HOOK_PORT), env(HOOK_TOKEN))?,
        })
    }

    /// The engine config for these settings with the store at `db`.
    pub fn engine(self, db: PathBuf) -> Config {
        Config {
            root: self.root,
            db,
            registry: self.claude.map(cli_runner),
            poll: PollOptions::default(),
            hooks: self.hooks,
            tail: TailOptions::default(),
            debounce: DEBOUNCE,
            rediscover_after: REDISCOVER_AFTER,
        }
    }
}

fn hook_options(
    port: Option<OsString>,
    token: Option<OsString>,
) -> Result<Option<HookOptions>, String> {
    let Some(port) = port.filter(|p| !p.is_empty()) else {
        return Ok(None);
    };
    let port = port
        .to_str()
        .and_then(|p| p.trim().parse::<u16>().ok())
        .filter(|&p| p != 0)
        .ok_or_else(|| format!("{HOOK_PORT} must be a port from 1 to 65535"))?;
    let token = token
        .filter(|t| !t.is_empty())
        .map(|t| {
            t.into_string()
                .map_err(|_| format!("{HOOK_TOKEN} must be valid Unicode"))
        })
        .transpose()?;
    Ok(Some(HookOptions {
        port,
        token,
        ..HookOptions::default()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        move |key| map.get(key).cloned()
    }

    fn home() -> Option<PathBuf> {
        Some(PathBuf::from("/home/u"))
    }

    #[test]
    fn hooks_are_off_by_default() {
        let settings = Settings::from_env(env(&[]), home()).expect("settings");
        assert!(settings.hooks.is_none());
        assert_eq!(settings.root, PathBuf::from("/home/u/.claude"));
        let empty = Settings::from_env(env(&[(HOOK_PORT, "")]), home()).expect("settings");
        assert!(empty.hooks.is_none());
    }

    #[test]
    fn a_hook_port_opts_in_with_an_optional_token() {
        let on = Settings::from_env(env(&[(HOOK_PORT, "47615")]), home()).expect("settings");
        let hooks = on.hooks.expect("opted in");
        assert_eq!((hooks.port, hooks.token), (47615, None));

        let with_token = Settings::from_env(
            env(&[(HOOK_PORT, " 47615 "), (HOOK_TOKEN, "lorem")]),
            home(),
        )
        .expect("settings");
        assert_eq!(
            with_token.hooks.and_then(|h| h.token).as_deref(),
            Some("lorem")
        );
    }

    #[test]
    fn a_bad_hook_port_is_an_error_not_a_silent_off() {
        for bad in ["0", "lorem", "70000", "-1"] {
            let err = Settings::from_env(env(&[(HOOK_PORT, bad)]), home()).expect_err(bad);
            assert!(err.contains(HOOK_PORT), "{err}");
        }
    }

    #[test]
    fn the_config_dir_follows_claude_config_dir_and_needs_a_home_otherwise() {
        let custom =
            Settings::from_env(env(&[("CLAUDE_CONFIG_DIR", "/cfg/alt")]), None).expect("settings");
        assert_eq!(custom.root, PathBuf::from("/cfg/alt"));
        assert!(Settings::from_env(env(&[]), None).is_err());
    }

    #[test]
    fn the_engine_config_debounces_to_four_hertz_and_polls_each_second() {
        let settings = Settings {
            root: PathBuf::from("/cfg"),
            claude: None,
            hooks: None,
        };
        let config = settings.engine(PathBuf::from("/data/strate.db"));
        assert_eq!(config.debounce, Duration::from_millis(250));
        assert_eq!(config.poll.every, Duration::from_secs(1));
        assert!(config.registry.is_none());
        assert_eq!(config.db, PathBuf::from("/data/strate.db"));
    }
}
