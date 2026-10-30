//! `strate_core::charge`: the agent-name parser and charge keys, ported from
//! claude-config's `scripts/lib.mjs` (`parseAgentName`, `chargeKey`). Every
//! grammar vector below is one of `scripts/test/session-name.test.mjs`'s, so
//! strate and the harness read a name the same way; the trim vectors pin JS
//! `String.prototype.trim`'s whitespace set.

use strate_core::charge::{Charge, ChargeKind, NameForm, ParsedName, charge_key, parse_agent_name};

fn issue(repo: &str, number: u64) -> Charge {
    numbered(ChargeKind::Issue, repo, number)
}

fn epic(repo: &str, number: u64) -> Charge {
    numbered(ChargeKind::Epic, repo, number)
}

fn pr(repo: &str, number: u64) -> Charge {
    numbered(ChargeKind::Pr, repo, number)
}

fn numbered(kind: ChargeKind, repo: &str, number: u64) -> Charge {
    Charge {
        kind,
        repo: Some(repo.into()),
        number: Some(number),
        topic: None,
    }
}

fn repo(repo: &str) -> Charge {
    Charge {
        kind: ChargeKind::Repo,
        repo: Some(repo.into()),
        number: None,
        topic: None,
    }
}

fn topic(topic: &str) -> Charge {
    Charge {
        kind: ChargeKind::Topic,
        repo: None,
        number: None,
        topic: Some(topic.into()),
    }
}

fn parsed(role: Option<&str>, charge: Charge, key: &str, form: NameForm) -> ParsedName {
    ParsedName {
        role: role.map(str::to_string),
        charge,
        key: key.into(),
        form,
    }
}

fn parse(name: &str) -> ParsedName {
    parse_agent_name(name).unwrap_or_else(|| panic!("{name:?} should parse"))
}

// ---- the grammar: <role>_<kind>_<target> ----------------------------------------

#[test]
fn the_new_grammar_reads_role_charge_key_and_form() {
    assert_eq!(
        parse("manager_epic_strate-105"),
        parsed(
            Some("manager"),
            epic("strate", 105),
            "epic_strate-105",
            NameForm::Agent
        )
    );
    assert_eq!(
        parse("lead_repo_strate"),
        parsed(Some("lead"), repo("strate"), "repo_strate", NameForm::Agent)
    );
    assert_eq!(
        parse("researcher_topic_pricing"),
        parsed(
            Some("researcher"),
            topic("pricing"),
            "topic_pricing",
            NameForm::Agent
        )
    );
    assert_eq!(parse("review_pr_strate-31").charge, pr("strate", 31));
    assert_eq!(
        parse("impl_issue_claude-config-360").key,
        "issue_claude-config-360"
    );
}

#[test]
fn the_last_dash_digits_of_a_numbered_target_is_the_number() {
    assert_eq!(parse("impl_issue_repo-5-2").charge, issue("repo-5", 2));
    assert_eq!(
        parse("impl_epic_dog-eat-dog-40").charge,
        epic("dog-eat-dog", 40)
    );
    for raw in [
        "impl_issue_strate",
        "impl_issue_strate-0",
        "impl_issue_strate-05",
        "impl_pr_-5",
        "impl_repo_strate-",
        "impl_topic_a--b",
        "impl_bogus_strate",
        "_issue_strate-5",
        "impl__strate",
        "impl_issue_strate-5_x",
        "impl.issue.strate-5",
    ] {
        assert_eq!(parse_agent_name(raw), None, "{raw:?}");
    }
}

#[test]
fn built_names_round_trip_with_the_key_as_the_name_minus_its_role() {
    let cases = [
        ("impl_issue_strate-16", "impl", issue("strate", 16)),
        ("lead_repo_claude-config", "lead", repo("claude-config")),
        (
            "researcher_topic_llm-pricing",
            "researcher",
            topic("llm-pricing"),
        ),
        ("manager_epic_repo2-9", "manager", epic("repo2", 9)),
        ("review-2_pr_a-1-3", "review-2", pr("a-1", 3)),
    ];
    for (name, role, charge) in cases {
        let p = parse(name);
        assert_eq!(
            (p.role.as_deref(), &p.charge, p.form),
            (Some(role), &charge, NameForm::Agent),
            "{name}"
        );
        assert_eq!(p.key, &name[name.find('_').unwrap() + 1..], "{name}");
    }
}

#[test]
fn input_is_trimmed_and_case_folded_and_junk_is_none() {
    let p = parse("  Impl_Issue_Claude-Config-360 ");
    assert_eq!(
        (p.role.as_deref(), p.key.as_str()),
        (Some("impl"), "issue_claude-config-360")
    );
    for raw in [
        "",
        "   ",
        "code-4c",
        "some-other-name",
        "Agent orchestration app naming",
        "ticket-repo",
        "director-",
        "repo-0",
        "harness",
    ] {
        assert_eq!(parse_agent_name(raw), None, "{raw:?}");
    }
}

#[test]
fn trimming_is_js_trims_whitespace_and_line_terminators_exactly() {
    // JS `String.prototype.trim`: WhiteSpace (with the BOM) and
    // LineTerminator, but not U+0085, which Rust counts as whitespace.
    let js = "\u{9}\u{a}\u{b}\u{c}\u{d}\u{20}\u{a0}\u{1680}\u{2000}\u{2005}\u{200a}\
              \u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}";
    for c in js.chars() {
        let name = format!("{c}impl_issue_x-5{c}");
        assert_eq!(parse(&name).key, "issue_x-5", "{c:?}");
    }
    assert_eq!(parse_agent_name("\u{85}impl_issue_x-5"), None);
    assert_eq!(parse_agent_name("impl_issue_x-5\u{85}"), None);
    assert_eq!(
        charge_key("\u{85}some-name\u{feff}").as_deref(),
        Some("\u{85}some-name")
    );
}

// ---- legacy forms ---------------------------------------------------------------

#[test]
fn legacy_typed_names_map_to_the_same_charge_with_the_suffix_role() {
    assert_eq!(
        parse("ticket-claude-config-175"),
        parsed(
            None,
            issue("claude-config", 175),
            "issue_claude-config-175",
            NameForm::Typed
        )
    );
    assert_eq!(
        parse("epic-dog-eat-dog-40-recon"),
        parsed(
            Some("recon"),
            epic("dog-eat-dog", 40),
            "epic_dog-eat-dog-40",
            NameForm::Typed
        )
    );
    assert_eq!(
        parse("director-strate-audit"),
        parsed(
            Some("director"),
            topic("strate-audit"),
            "topic_strate-audit",
            NameForm::Typed
        )
    );
    assert_eq!(
        parse("general-spend-review"),
        parsed(
            None,
            topic("spend-review"),
            "topic_spend-review",
            NameForm::Typed
        )
    );
    let p = parse("ticket-claude-config-214-review-r3-2");
    assert_eq!(
        (p.role.as_deref(), p.key.as_str()),
        (Some("review"), "issue_claude-config-214"),
        "round and dedupe suffixes drop"
    );
    assert_eq!(
        parse("ticket-repo-5-2").key,
        "issue_repo-5",
        "a lazy repo: -2 is the harness's dedupe"
    );
    assert_eq!(
        parse("ticket-repo-5-1").key,
        "issue_repo-5-1",
        "dedupe starts at 2, so -1 is an issue number"
    );
    for raw in [
        "ticket-repo-5-implementer",
        "ticket-repo-5-review-",
        "ticket-repo-5-r1",
        "ticket-repo-5-r2-review",
        "repo-5-review",
    ] {
        assert_eq!(parse_agent_name(raw), None, "{raw:?}");
    }
}

#[test]
fn legacy_tree_names_key_to_their_ticket_else_epic_else_repo() {
    assert_eq!(
        parse("what-is-dark_what-is-dark_t23_review-r3"),
        parsed(
            Some("review"),
            issue("what-is-dark", 23),
            "issue_what-is-dark-23",
            NameForm::Tree
        )
    );
    assert_eq!(
        parse("gmail-cleanup_inbox_e202306_t2_impl"),
        parsed(
            Some("impl"),
            issue("inbox", 2),
            "issue_inbox-2",
            NameForm::Tree
        )
    );
    assert_eq!(
        parse("harness_claude-config_t360"),
        parsed(
            None,
            issue("claude-config", 360),
            "issue_claude-config-360",
            NameForm::Tree
        )
    );
    assert_eq!(
        parse("harness_claude-config_e176").key,
        "epic_claude-config-176"
    );
    assert_eq!(
        parse("harness_claude-config_recon").charge,
        repo("claude-config")
    );
    for raw in [
        "harness_",
        "_claude-config",
        "harness__claude-config",
        "harness_claude-config_t264_e176",
        "harness_claude-config_t0",
        "harness_claude-config_t264_implementer",
        "harness_claude-config_t264_impl_recon",
        "harness_claude-config_t264_review-r1",
    ] {
        assert_eq!(parse_agent_name(raw), None, "{raw:?}");
    }
}

#[test]
fn the_new_grammar_wins_over_a_tree_reading() {
    let p = parse("x_repo_impl");
    assert_eq!((p.form, p.key.as_str()), (NameForm::Agent, "repo_impl"));
    assert_eq!(
        parse("x_issue_t5").form,
        NameForm::Tree,
        "not an issue target, so it falls through to the tree form"
    );
}

#[test]
fn a_charge_key_on_its_own_parses_with_no_role() {
    assert_eq!(
        parse("issue_x-5"),
        parsed(None, issue("x", 5), "issue_x-5", NameForm::Charge)
    );
    assert_eq!(parse("repo_strate").charge, repo("strate"));
    assert_eq!(parse("Topic_Harness").charge, topic("harness"));
    assert_eq!(parse("epic_dog-eat-dog-40").charge, epic("dog-eat-dog", 40));
    for raw in ["issue_x", "pr_x-0", "bogus_x-5"] {
        assert_ne!(
            parse_agent_name(raw).map(|p| p.form),
            Some(NameForm::Charge),
            "{raw:?}"
        );
    }
    for name in ["impl_issue_x-5", "ticket-x-5", "issue_x-5"] {
        assert_eq!(charge_key(name).as_deref(), Some("issue_x-5"), "{name}");
    }
    let key = charge_key("harness_claude-config_t360").expect("key");
    assert_eq!(
        charge_key(&key).as_deref(),
        Some("issue_claude-config-360"),
        "a key round-trips through charge_key"
    );
}

#[test]
fn a_bare_slug_dash_number_is_an_issue_of_its_slug() {
    assert_eq!(
        parse("claude-config-62"),
        parsed(
            None,
            issue("claude-config", 62),
            "issue_claude-config-62",
            NameForm::Bare
        )
    );
}

#[test]
fn every_form_of_one_charge_shares_its_key() {
    for name in [
        "impl_issue_claude-config-264",
        "review-2_issue_claude-config-264",
        "ticket-claude-config-264",
        "ticket-claude-config-264-review-2",
        "harness_claude-config_e176_t264_impl",
        "claude-config-264",
        "Ticket-Claude-Config-264",
    ] {
        assert_eq!(
            charge_key(name).as_deref(),
            Some("issue_claude-config-264"),
            "{name}"
        );
    }
    assert_ne!(
        charge_key("epic-repo-5"),
        charge_key("ticket-repo-5"),
        "an epic and an issue on one number are different charges"
    );
    assert_ne!(
        charge_key("review_pr_repo-5"),
        charge_key("impl_issue_repo-5"),
        "a PR and an issue are different charges"
    );
}

#[test]
fn charge_key_is_the_parsed_key_else_the_trimmed_title_else_none() {
    assert_eq!(
        charge_key("director-harness").as_deref(),
        Some("topic_harness")
    );
    assert_eq!(
        charge_key("  some-other-name ").as_deref(),
        Some("some-other-name")
    );
    assert_eq!(charge_key(""), None);
    assert_eq!(charge_key("   "), None);
}

#[test]
fn a_charge_key_is_kind_underscore_target() {
    assert_eq!(issue("strate", 7).key(), "issue_strate-7");
    assert_eq!(epic("dog-eat-dog", 40).key(), "epic_dog-eat-dog-40");
    assert_eq!(pr("a-1", 3).key(), "pr_a-1-3");
    assert_eq!(repo("strate").key(), "repo_strate");
    assert_eq!(topic("llm-pricing").key(), "topic_llm-pricing");
    assert_eq!(ChargeKind::Pr.as_str(), "pr");
}

#[test]
fn a_charge_serializes_for_the_webview() {
    let json = serde_json::to_value(issue("strate", 7)).expect("json");
    assert_eq!(
        json,
        serde_json::json!({"kind": "issue", "repo": "strate", "number": 7, "topic": null})
    );
    let json = serde_json::to_value(topic("pricing")).expect("json");
    assert_eq!(json["kind"], "topic");
}
