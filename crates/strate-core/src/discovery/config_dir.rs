use std::ffi::OsString;
use std::path::PathBuf;

/// Resolves the Claude Code config dir: `CLAUDE_CONFIG_DIR` when set and
/// non-empty, else `<home>/.claude`. Pure: the caller passes the env value
/// and home dir, so nothing here touches the process environment.
pub fn resolve_config_dir(
    claude_config_dir: Option<OsString>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    match claude_config_dir {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => home.map(|h| h.join(".claude")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_wins_over_home() {
        let got = resolve_config_dir(
            Some(OsString::from("/cfg/alt")),
            Some(PathBuf::from("/home/u")),
        );
        assert_eq!(got, Some(PathBuf::from("/cfg/alt")));
    }

    #[test]
    fn falls_back_to_dot_claude_under_home() {
        let got = resolve_config_dir(None, Some(PathBuf::from("/home/u")));
        assert_eq!(got, Some(PathBuf::from("/home/u").join(".claude")));
    }

    #[test]
    fn empty_env_value_counts_as_unset() {
        let got = resolve_config_dir(Some(OsString::new()), Some(PathBuf::from("/home/u")));
        assert_eq!(got, Some(PathBuf::from("/home/u").join(".claude")));
    }

    #[test]
    fn env_override_works_without_home() {
        let got = resolve_config_dir(Some(OsString::from("/cfg/alt")), None);
        assert_eq!(got, Some(PathBuf::from("/cfg/alt")));
    }

    #[test]
    fn no_env_and_no_home_is_none() {
        assert_eq!(resolve_config_dir(None, None), None);
    }
}
