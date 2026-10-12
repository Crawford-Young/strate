//! The session-name parser: `<repo>-<issue>[-sN]` windows and
//! `<repo>-<issue>-<role>-<task>[-N]` agents. Names are synthetic.

use strate_core::names::{ParsedName, parse_name};

fn parsed(name: &str) -> (String, u64, Option<u32>) {
    let p = parse_name(name).unwrap_or_else(|| panic!("{name} should parse"));
    (p.repo, p.issue, p.rotation)
}

#[test]
fn a_window_is_repo_and_issue() {
    assert_eq!(parsed("lorem-137"), ("lorem".into(), 137, None));
}

#[test]
fn a_repo_with_dashes_is_the_segments_before_the_issue() {
    assert_eq!(
        parsed("dolor-sit-amet-10"),
        ("dolor-sit-amet".into(), 10, None)
    );
}

#[test]
fn a_trailing_s_n_is_the_rotation() {
    assert_eq!(
        parsed("dolor-sit-amet-10-s2"),
        ("dolor-sit-amet".into(), 10, Some(2))
    );
}

#[test]
fn an_agent_name_yields_its_window_and_no_rotation() {
    assert_eq!(parsed("lorem-137-impl-parser"), ("lorem".into(), 137, None));
    assert_eq!(
        parsed("lorem-137-impl-parser-2"),
        ("lorem".into(), 137, None)
    );
}

#[test]
fn s_n_inside_an_agent_name_is_not_a_rotation() {
    assert_eq!(parsed("lorem-137-impl-s2"), ("lorem".into(), 137, None));
    assert_eq!(parsed("lorem-137-s2-extra"), ("lorem".into(), 137, None));
}

#[test]
fn the_issue_is_the_first_all_digit_segment() {
    assert_eq!(parsed("v2-app-5-fix-12"), ("v2-app".into(), 5, None));
}

#[test]
fn names_without_a_repo_and_issue_do_not_parse() {
    for name in [
        "",
        "lorem",
        "lorem-ipsum",
        "137",
        "137-lorem",
        "-5",
        "lorem-",
        "lorem-1x",
        "lorem-99999999999999999999999",
    ] {
        assert_eq!(parse_name(name), None, "{name:?}");
    }
}

#[test]
fn the_value_type_compares() {
    let p = ParsedName {
        repo: "a".into(),
        issue: 1,
        rotation: None,
    };
    assert_eq!(parse_name("a-1"), Some(p));
}
