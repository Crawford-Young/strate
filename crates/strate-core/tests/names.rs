//! The session-name parser: `<repo>-<issue>[-sN]` windows and
//! `<repo>-<issue>-<role>-<task>[-N]` agents. Names are synthetic.

use strate_core::names::{ParsedName, is_director, parse_name};

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

// claude-config's typed names (scripts/lib.mjs, TYPED_RE): `ticket-` and
// `epic-` name a repo and issue; `director-` and `general-` are Unlinked.

#[test]
fn a_ticket_or_epic_name_is_its_repo_and_issue() {
    assert_eq!(parsed("ticket-lorem-51"), ("lorem".into(), 51, None));
    assert_eq!(parsed("epic-dolor-sit-7"), ("dolor-sit".into(), 7, None));
    assert_eq!(parsed("Ticket-lorem-51"), ("lorem".into(), 51, None));
}

#[test]
fn a_typed_name_keeps_its_rotation_and_agent_suffixes() {
    assert_eq!(parsed("ticket-lorem-51-s2"), ("lorem".into(), 51, Some(2)));
    assert_eq!(
        parsed("ticket-lorem-51-impl-parser-2"),
        ("lorem".into(), 51, None)
    );
}

#[test]
fn director_and_general_names_are_unlinked_even_with_digits() {
    for name in [
        "director-lorem",
        "director-wave-3",
        "general-fix-5-things",
        "General-lorem-12",
    ] {
        assert_eq!(parse_name(name), None, "{name:?}");
    }
}

#[test]
fn a_typed_prefix_with_no_repo_left_is_an_untyped_name() {
    // Like lib.mjs's legacy form: `ticket-51` is repo `ticket`, issue 51.
    assert_eq!(parsed("ticket-51"), ("ticket".into(), 51, None));
    assert_eq!(parsed("epic-9-s1"), ("epic".into(), 9, Some(1)));
}

#[test]
fn a_repo_github_cannot_hold_does_not_parse() {
    // A pasted launch line as a window name: no guessing at the repo.
    for name in [
        "claude -n ticket-lorem-dolor-180",
        "claude -n lorem-180",
        "ticket-lorem ipsum-3",
        "lorem/ipsum-4",
        "lörem-5",
    ] {
        assert_eq!(parse_name(name), None, "{name:?}");
    }
    assert_eq!(parsed("Lorem_ip.sum-6"), ("Lorem_ip.sum".into(), 6, None));
}

#[test]
fn a_director_name_carries_the_typed_prefix_in_any_case() {
    for yes in ["director-demo", "Director-Harness", "DIRECTOR-x"] {
        assert!(is_director(yes), "{yes:?}");
    }
    for no in [
        "director-",
        "director",
        "directors-demo",
        "general-notes",
        "ticket-demo-repo-1",
        "claude -n director-demo",
        "",
    ] {
        assert!(!is_director(no), "{no:?}");
    }
}
