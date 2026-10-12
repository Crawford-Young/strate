//! Time accounting: a port of the time half of claude-config's
//! `scripts/audit-lib.mjs` (#97). A timeline's records are replayed in
//! file order into intervals (model requests, tool calls, questions to the
//! user, waits for the next prompt, idle gaps), and a sweep over
//! `[first, last]` gives every millisecond to one bucket: idle beats ask
//! beats model beats tools beats wait, and a span none covers is other.
//! Totals only: audit's per-UTC-day rows split the same spans, so their
//! sums are these.

use std::collections::HashMap;

use jiff::{Timestamp, Zoned, tz::TimeZone};

/// Silence longer than this between two records is idle, not active.
pub const IDLE_GAP_MS: i64 = 10 * 60 * 1000;

/// Tools that wait on a person: their time is user time.
const USER_TOOLS: [&str; 2] = ["AskUserQuestion", "ExitPlanMode"];

/// What the time model reads from one record.
#[derive(Debug, Clone, PartialEq)]
pub enum Record {
    Assistant {
        /// `requestId`, else `message.id`, else `uuid`.
        request: Option<String>,
        /// Each `tool_use` block's id and name.
        tool_uses: Vec<(String, Option<String>)>,
    },
    User {
        /// Each `tool_result` block's `tool_use_id`.
        tool_results: Vec<Option<String>>,
        /// `Some` on a prompt (no tool result, not `isMeta`): `true` when a
        /// person typed it, `false` for a task notification.
        prompt: Option<bool>,
    },
    Other,
}

/// A record and its timestamp in epoch ms.
#[derive(Debug, Clone, PartialEq)]
pub struct Timed {
    pub at: i64,
    pub record: Record,
}

/// A timeline's wall time and where it went. The buckets sum to `wall_ms`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Split {
    pub wall_ms: i64,
    pub idle_ms: i64,
    pub model_ms: i64,
    pub tools_ms: i64,
    /// Questions to the user plus waits for a typed prompt: waiting time.
    pub user_ms: i64,
    pub other_ms: i64,
}

impl Split {
    /// Wall time less the idle gaps.
    pub fn active_ms(&self) -> i64 {
        self.wall_ms - self.idle_ms
    }
}

/// `Date.parse` for the `YYYY-MM-DDTHH:MM:SS[.fff]Z` stamps transcripts
/// carry, in epoch ms; `None` for anything else.
pub fn parse_timestamp(s: &str) -> Option<i64> {
    let (clock, fraction) = s.strip_suffix('Z')?.split_at_checked(19)?;
    let b = clock.as_bytes();
    if [4, 7].iter().any(|&i| b[i] != b'-') || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> {
        let digits = &clock[from..to];
        digits
            .bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| digits.parse().ok())?
    };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 59 {
        return None;
    }
    let ms = match fraction.strip_prefix('.') {
        None if fraction.is_empty() => 0,
        Some(f) if !f.is_empty() && f.bytes().all(|c| c.is_ascii_digit()) => {
            format!("{f:0<3}")[..3].parse().ok()?
        }
        _ => return None,
    };
    // Days from civil (H. Hinnant), with March as the first month.
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((mo + 9) % 12) + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some(((days * 24 + h) * 60 + mi) * 60_000 + sec * 1000 + ms)
}

/// Overlap priority, highest first.
#[derive(Debug, Clone, Copy)]
enum Cat {
    Idle,
    Ask,
    Model,
    Tools,
    Wait,
}

/// Splits a timeline (records in file order). `also` holds timestamps that
/// count as activity without being replayed, such as a session's subagent
/// records: a parent waiting on a running agent is not idle. `None` when
/// there are no records.
pub fn split(records: &[Timed], also: &[i64], idle_gap_ms: i64) -> Option<Split> {
    split_at(records, also, idle_gap_ms, &[])?
        .pop()
        .map(|(_, whole)| whole)
}

/// `split` cut into buckets at `cuts` (epoch ms, any order): a new bucket
/// starts at each cut strictly inside the timeline, and a bucket is keyed by
/// its start. A span crossing a cut counts on both sides; the buckets sum to
/// `split`'s result.
pub fn split_at(
    records: &[Timed],
    also: &[i64],
    idle_gap_ms: i64,
    cuts: &[i64],
) -> Option<Vec<(i64, Split)>> {
    if records.is_empty() {
        return None;
    }
    let mut ts: Vec<i64> = records
        .iter()
        .map(|r| r.at)
        .chain(also.iter().copied())
        .collect();
    ts.sort_unstable();
    let (first, last) = (ts[0], ts[ts.len() - 1]);
    let mut intervals: Vec<(i64, i64, Cat)> = ts
        .windows(2)
        .filter(|w| w[1] - w[0] > idle_gap_ms)
        .map(|w| (w[0], w[1], Cat::Idle))
        .collect();

    // A request runs from whatever fed it (the last prompt or tool result)
    // to its last block; a tool from its tool_use to its first result.
    let mut requests: HashMap<Option<&str>, (i64, i64)> = HashMap::new();
    let mut tools: Vec<(Option<&str>, i64, Option<i64>)> = Vec::new();
    let mut tool_ids: HashMap<&str, usize> = HashMap::new();
    let (mut last_input, mut last_end): (Option<i64>, Option<i64>) = (None, None);
    for Timed { at: t, record } in records {
        let t = *t;
        match record {
            Record::Assistant { request, tool_uses } => {
                let start = t.min(last_input.unwrap_or(t).max(last_end.unwrap_or(i64::MIN)));
                let span = requests.entry(request.as_deref()).or_insert((start, t));
                span.1 = span.1.max(t);
                last_end = Some(last_end.unwrap_or(t).max(t));
                for (id, name) in tool_uses {
                    if !tool_ids.contains_key(id.as_str()) {
                        tool_ids.insert(id, tools.len());
                        tools.push((name.as_deref(), t, None));
                    }
                }
            }
            Record::User {
                tool_results,
                prompt,
            } => {
                for id in tool_results.iter().flatten() {
                    if let Some(&i) = tool_ids.get(id.as_str()) {
                        tools[i].2.get_or_insert(t);
                    }
                }
                // The wait for a prompt starts at the last work.
                if let (Some(true), Some(from)) = (prompt, last_end.max(last_input)) {
                    intervals.push((from, t, Cat::Wait));
                }
                if !tool_results.is_empty() || prompt.is_some() {
                    last_input = Some(t);
                }
            }
            Record::Other => {}
        }
    }
    intervals.extend(requests.into_values().map(|(a, b)| (a, b, Cat::Model)));
    for (name, start, end) in tools {
        let cat = match name {
            Some(n) if USER_TOOLS.contains(&n) => Cat::Ask,
            _ => Cat::Tools,
        };
        intervals.extend(end.map(|end| (start, end, cat)));
    }
    Some(sweep(&intervals, first, last, cuts))
}

/// Every local midnight strictly inside `(first_ms, last_ms)`, as epoch ms.
/// A day is from one midnight to the next, so a 23 h or 25 h day stays one.
pub fn local_midnights(tz: &TimeZone, first_ms: i64, last_ms: i64) -> Vec<i64> {
    let Some(start) = instant(tz, first_ms) else {
        return Vec::new();
    };
    let mut midnights = Vec::new();
    let mut day = start.date();
    while let Ok(next) = day.tomorrow() {
        day = next;
        let Some(ms) = day
            .to_zoned(tz.clone())
            .ok()
            .and_then(|z| z.start_of_day().ok())
            .map(|z| z.timestamp().as_millisecond())
        else {
            break;
        };
        if ms >= last_ms {
            break;
        }
        if ms > first_ms {
            midnights.push(ms);
        }
    }
    midnights
}

/// The local calendar day of `ms` as `YYYY-MM-DD`; `None` outside jiff's
/// range (years -9999 to 9999).
pub fn local_day(tz: &TimeZone, ms: i64) -> Option<String> {
    instant(tz, ms).map(|z| z.date().to_string())
}

fn instant(tz: &TimeZone, ms: i64) -> Option<Zoned> {
    Timestamp::from_millisecond(ms)
        .ok()
        .map(|t| t.to_zoned(tz.clone()))
}

/// Gives each span of `[first, last]` to the highest-priority category
/// live over it, so parallel spans count once. Each cut inside the range
/// starts a new bucket.
fn sweep(intervals: &[(i64, i64, Cat)], first: i64, last: i64, cuts: &[i64]) -> Vec<(i64, Split)> {
    let mut starts: Vec<i64> = cuts
        .iter()
        .copied()
        .filter(|&c| c > first && c < last)
        .collect();
    starts.sort_unstable();
    starts.dedup();
    starts.insert(0, first);
    let mut out: Vec<(i64, Split)> = starts
        .iter()
        .zip(starts.iter().skip(1).chain([&last]))
        .map(|(&from, &to)| {
            let wall_ms = to - from;
            (
                from,
                Split {
                    wall_ms,
                    ..Split::default()
                },
            )
        })
        .collect();
    let mut points: Vec<(i64, Option<(Cat, i32)>)> = starts.iter().map(|&c| (c, None)).collect();
    points.push((last, None));
    for &(a, b, cat) in intervals {
        let (s, e) = (a.max(first), b.min(last));
        if e > s {
            points.extend([(s, Some((cat, 1))), (e, Some((cat, -1)))]);
        }
    }
    points.sort_by_key(|p| p.0);
    let mut live = [0i32; 5];
    for pair in points.windows(2) {
        if let Some((cat, delta)) = pair[0].1 {
            live[cat as usize] += delta;
        }
        let span = pair[1].0 - pair[0].0;
        let at = starts.partition_point(|&c| c <= pair[0].0) - 1;
        let bucket = &mut out[at].1;
        let slot = match live.iter().position(|&n| n > 0) {
            Some(0) => &mut bucket.idle_ms,
            Some(1 | 4) => &mut bucket.user_ms,
            Some(2) => &mut bucket.model_ms,
            Some(_) => &mut bucket.tools_ms,
            None => &mut bucket.other_ms,
        };
        *slot += span;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_791_021_600_000; // 2026-10-03T10:00:00.000Z

    fn at(s: i64, record: Record) -> Timed {
        Timed {
            at: T0 + s * 1000,
            record,
        }
    }

    fn prompt(s: i64, human: bool) -> Timed {
        at(
            s,
            Record::User {
                tool_results: vec![],
                prompt: Some(human),
            },
        )
    }

    fn reply(s: i64, request: &str, tool: Option<(&str, &str)>) -> Timed {
        at(
            s,
            Record::Assistant {
                request: Some(request.into()),
                tool_uses: tool
                    .map(|(id, name)| (id.to_string(), Some(name.to_string())))
                    .into_iter()
                    .collect(),
            },
        )
    }

    fn result(s: i64, id: &str) -> Timed {
        at(
            s,
            Record::User {
                tool_results: vec![Some(id.into())],
                prompt: None,
            },
        )
    }

    #[test]
    fn timestamps_parse_to_epoch_ms() {
        // 2026-01-01 is day 20454 (946684800 s at 2000-01-01, plus 9497
        // days for 26 years with 7 leap days); Oct 3 is 275 days later.
        // (20454 + 275) * 86400 + 10 h = 1791021600 s.
        assert_eq!(parse_timestamp("2026-10-03T10:00:00.000Z"), Some(T0));
        assert_eq!(parse_timestamp("2026-10-03T10:00:00Z"), Some(T0));
        assert_eq!(parse_timestamp("2026-10-03T10:00:05.5Z"), Some(T0 + 5500));
        assert_eq!(
            parse_timestamp("2026-10-03T10:00:05.12345Z"),
            Some(T0 + 5123)
        );
        assert_eq!(parse_timestamp("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(
            parse_timestamp("2024-02-29T00:00:00.000Z"),
            Some(1_709_164_800_000)
        );
        for bad in [
            "",
            "lorem",
            "2026-10-03 10:00:00Z",
            "2026-10-03T10:00:00",
            "2026-13-03T10:00:00Z",
            "2026-10-03T24:00:00Z",
            "2026-10-03T10:00:00.Z",
            "+026-10-03T10:00:00Z",
        ] {
            assert_eq!(parse_timestamp(bad), None, "{bad}");
        }
    }

    /// The cost fixture's main session, in seconds from 10:00:00. Its
    /// subagent's records (132..312 s) only count as activity.
    fn session() -> Vec<Timed> {
        vec![
            prompt(0, true),
            reply(5, "req_c1", None),
            reply(8, "req_c1", Some(("toolu_c1", "AskUserQuestion"))),
            result(128, "toolu_c1"),
            reply(130, "req_c2", Some(("toolu_c2", "Agent"))),
            result(315, "toolu_c2"),
            reply(320, "req_c3", None),
            prompt(440, true),
            reply(445, "req_c5", None),
            prompt(1800, true),
            reply(1804, "req_c4", None),
            reply(1806, "msg_syn1", None),
        ]
    }

    #[test]
    fn a_session_splits_into_model_tools_user_and_idle() {
        let sub: Vec<i64> = [132, 192, 252, 312].map(|s| T0 + s * 1000).into();
        let split = split(&session(), &sub, IDLE_GAP_MS).expect("records");
        // Records at 0..1806 s; the only gap over 600 s is 445 -> 1800 (1355 s).
        // Requests run from what fed them to their last block: c1 [0,8],
        // c2 [128,130], c3 [315,320], c5 [440,445], c4 [1800,1804],
        // synthetic [1804,1806] -> model 8+2+5+5+4+2 = 26 s.
        // AskUserQuestion [8,128] = 120 s and the wait for the 440 s prompt
        // since the last work [320,440] = 120 s -> user 240 s. The wait for
        // the 1800 s prompt [445,1800] is all idle (idle wins).
        // Agent [130,315] -> tools 185 s. 26 + 240 + 185 + 1355 = 1806.
        assert_eq!(
            split,
            Split {
                wall_ms: 1_806_000,
                idle_ms: 1_355_000,
                model_ms: 26_000,
                tools_ms: 185_000,
                user_ms: 240_000,
                other_ms: 0,
            }
        );
        assert_eq!(split.active_ms(), 451_000);
    }

    #[test]
    fn subagent_records_keep_a_waiting_parent_out_of_idle() {
        // Without the subagent the Agent call's 185 s is still one gap under
        // 600 s, so shrink the gap to 100 s to see the difference.
        let records = session();
        let alone = split(&records, &[], 100_000).expect("records");
        let sub: Vec<i64> = [132, 192, 252, 312].map(|s| T0 + s * 1000).into();
        let with = split(&records, &sub, 100_000).expect("records");
        // Alone: gaps over 100 s are 8->128 (120), 130->315 (185),
        // 320->440 (120), 445->1800 (1355) = 1780 s idle.
        assert_eq!(alone.idle_ms, 1_780_000);
        // With the subagent at 132, 192, 252, 312: 130->315 is covered.
        assert_eq!(with.idle_ms, 1_595_000);
    }

    #[test]
    fn a_subagent_run_splits_on_its_own_records() {
        let run = [
            prompt(132, true),
            reply(192, "req_s1", Some(("toolu_s1", "Bash"))),
            result(252, "toolu_s1"),
            reply(312, "req_s2", None),
        ];
        // The first prompt has no work before it: no wait. s1 [132,192],
        // Bash [192,252], s2 [252,312] -> model 120 s, tools 60 s.
        assert_eq!(
            split(&run, &[], IDLE_GAP_MS),
            Some(Split {
                wall_ms: 180_000,
                model_ms: 120_000,
                tools_ms: 60_000,
                ..Split::default()
            })
        );
    }

    #[test]
    fn a_task_notification_is_not_a_wait_and_parallel_calls_count_once() {
        let run = [
            prompt(0, true),
            at(
                5,
                Record::Assistant {
                    request: Some("req_a".into()),
                    tool_uses: vec![
                        ("toolu_a".into(), Some("Read".into())),
                        ("toolu_b".into(), Some("Grep".into())),
                    ],
                },
            ),
            result(15, "toolu_b"),
            result(25, "toolu_a"),
            // A replayed tool_use id keeps its first start.
            reply(26, "req_b", Some(("toolu_a", "Read"))),
            prompt(86, false),
            reply(90, "req_c", None),
        ];
        // model [0,5], [25,26], [86,90] = 10 s; tools Read [5,25] and Grep
        // [5,15] overlap -> 20 s; [26,86] has nothing but a task
        // notification at its end -> other 60 s.
        assert_eq!(
            split(&run, &[], IDLE_GAP_MS),
            Some(Split {
                wall_ms: 90_000,
                model_ms: 10_000,
                tools_ms: 20_000,
                other_ms: 60_000,
                ..Split::default()
            })
        );
    }

    #[test]
    fn no_records_no_split() {
        assert_eq!(split(&[], &[T0], IDLE_GAP_MS), None);
        assert_eq!(split_at(&[], &[T0], IDLE_GAP_MS, &[T0]), None);
    }

    fn sum(buckets: &[(i64, Split)]) -> Split {
        buckets.iter().fold(Split::default(), |a, (_, b)| Split {
            wall_ms: a.wall_ms + b.wall_ms,
            idle_ms: a.idle_ms + b.idle_ms,
            model_ms: a.model_ms + b.model_ms,
            tools_ms: a.tools_ms + b.tools_ms,
            user_ms: a.user_ms + b.user_ms,
            other_ms: a.other_ms + b.other_ms,
        })
    }

    #[test]
    fn no_cuts_is_one_bucket_keyed_by_its_start() {
        let whole = split(&session(), &[], IDLE_GAP_MS).expect("records");
        assert_eq!(
            split_at(&session(), &[], IDLE_GAP_MS, &[]),
            Some(vec![(T0, whole)])
        );
    }

    #[test]
    fn a_cut_splits_a_span_and_the_buckets_sum_to_the_whole() {
        // Cut at 200 s lands inside the Agent call [130,315] (tools) and
        // after the AskUserQuestion (user).
        let cut = T0 + 200_000;
        let buckets = split_at(&session(), &[], IDLE_GAP_MS, &[cut]).expect("records");
        assert_eq!(buckets.len(), 2);
        assert_eq!((buckets[0].0, buckets[1].0), (T0, cut));
        assert_eq!(buckets[0].1.wall_ms, 200_000);
        assert_eq!(buckets[1].1.wall_ms, 1_606_000);
        // Tools [130,315]: 70 s before the cut, 115 s after.
        assert_eq!(buckets[0].1.tools_ms, 70_000);
        assert_eq!(buckets[1].1.tools_ms, 115_000);
        assert_eq!(
            sum(&buckets),
            split(&session(), &[], IDLE_GAP_MS).expect("records")
        );
    }

    #[test]
    fn cuts_outside_the_span_or_repeated_are_ignored() {
        let whole = split(&session(), &[], IDLE_GAP_MS).expect("records");
        let last = T0 + 1_806_000;
        let cuts = [T0 - 5, T0, last, last + 5, T0 + 100_000, T0 + 100_000];
        let buckets = split_at(&session(), &[], IDLE_GAP_MS, &cuts).expect("records");
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[1].0, T0 + 100_000);
        assert_eq!(sum(&buckets), whole);
    }

    #[test]
    fn cuts_may_arrive_unsorted() {
        let a = T0 + 100_000;
        let b = T0 + 900_000;
        let sorted = split_at(&session(), &[], IDLE_GAP_MS, &[a, b]);
        assert_eq!(split_at(&session(), &[], IDLE_GAP_MS, &[b, a]), sorted);
    }

    #[test]
    fn an_idle_gap_straddling_a_cut_is_idle_on_both_sides() {
        // The wait for the 1800 s prompt [445,1800] is idle; cut inside it.
        let cut = T0 + 1_000_000;
        let buckets = split_at(&session(), &[], IDLE_GAP_MS, &[cut]).expect("records");
        assert_eq!(buckets[0].1.idle_ms, 555_000);
        assert_eq!(buckets[1].1.idle_ms, 800_000);
        assert_eq!(buckets[0].1.active_ms(), 445_000);
        assert_eq!(buckets[1].1.active_ms(), 6_000);
    }

    #[test]
    fn a_wait_straddling_a_cut_is_user_time_on_both_sides() {
        // Wait [320,440] for the 440 s prompt; cut at 380 s.
        let cut = T0 + 380_000;
        let buckets = split_at(&session(), &[], IDLE_GAP_MS, &[cut]).expect("records");
        // Ask [8,128] = 120 s sits wholly before the cut.
        assert_eq!(buckets[0].1.user_ms, 120_000 + 60_000);
        assert_eq!(buckets[1].1.user_ms, 60_000);
    }

    #[test]
    fn local_days_follow_the_zone() {
        let toronto = jiff::tz::TimeZone::get("America/Toronto").expect("tz");
        // 2026-10-03T10:00Z is 06:00 EDT on the 3rd.
        assert_eq!(local_day(&toronto, T0).as_deref(), Some("2026-10-03"));
        // 02:00Z on the 4th is 22:00 EDT on the 3rd.
        assert_eq!(
            local_day(&toronto, T0 + 16 * 3_600_000).as_deref(),
            Some("2026-10-03")
        );
        assert_eq!(
            local_day(&toronto, T0 + 20 * 3_600_000).as_deref(),
            Some("2026-10-04")
        );
        assert_eq!(
            local_day(&jiff::tz::TimeZone::UTC, T0).as_deref(),
            Some("2026-10-03")
        );
        // Past jiff's range (year 9999) there is no local day, never "".
        assert_eq!(local_day(&toronto, i64::MAX), None);
    }

    #[test]
    fn a_span_crossing_one_local_midnight_has_one_cut() {
        let toronto = jiff::tz::TimeZone::get("America/Toronto").expect("tz");
        // Local midnight 2026-10-04 EDT (UTC-4) is 04:00Z.
        let midnight = T0 + 18 * 3_600_000;
        assert_eq!(
            local_midnights(&toronto, T0, T0 + 30 * 3_600_000),
            vec![midnight]
        );
        let utc = jiff::tz::TimeZone::UTC;
        assert_eq!(
            local_midnights(&utc, T0, T0 + 30 * 3_600_000),
            vec![T0 + 14 * 3_600_000]
        );
        // Midnights on the span's edge are not strictly inside.
        assert_eq!(
            local_midnights(&toronto, midnight, midnight + 1),
            Vec::<i64>::new()
        );
        assert_eq!(
            local_midnights(&toronto, midnight - 1, midnight),
            Vec::<i64>::new()
        );
        assert_eq!(local_midnights(&toronto, T0, T0), Vec::<i64>::new());
    }

    #[test]
    fn dst_days_stay_one_day() {
        let toronto = jiff::tz::TimeZone::get("America/Toronto").expect("tz");
        // Fall back 2026-11-01 (25 h day): midnight Nov 1 is 04:00Z (EDT),
        // Nov 2 is 05:00Z (EST). Spring forward 2026-03-08 (23 h day):
        // Mar 8 is 05:00Z (EST), Mar 9 is 04:00Z (EDT).
        let nov1 = parse_timestamp("2026-11-01T04:00:00Z").expect("ts");
        let nov2 = parse_timestamp("2026-11-02T05:00:00Z").expect("ts");
        let nov3 = parse_timestamp("2026-11-03T05:00:00Z").expect("ts");
        assert_eq!(nov2 - nov1, 25 * 3_600_000);
        assert_eq!(
            local_midnights(&toronto, nov1 - 1000, nov3 + 1000),
            vec![nov1, nov2, nov3]
        );
        let mar8 = parse_timestamp("2026-03-08T05:00:00Z").expect("ts");
        let mar9 = parse_timestamp("2026-03-09T04:00:00Z").expect("ts");
        assert_eq!(mar9 - mar8, 23 * 3_600_000);
        assert_eq!(
            local_midnights(&toronto, mar8 - 1000, mar9 + 1000),
            vec![mar8, mar9]
        );
        assert_eq!(local_day(&toronto, mar9 - 1).as_deref(), Some("2026-03-08"));
        assert_eq!(local_day(&toronto, mar9).as_deref(), Some("2026-03-09"));
    }

    #[test]
    fn a_session_across_a_dst_change_splits_per_local_day() {
        let toronto = jiff::tz::TimeZone::get("America/Toronto").expect("tz");
        // Two replies 8 min apart (under the idle gap) around the Nov 2
        // local midnight (05:00Z).
        let mid = parse_timestamp("2026-11-02T05:00:00Z").expect("ts");
        let records = [
            Timed {
                at: mid - 240_000,
                record: Record::Assistant {
                    request: Some("a".into()),
                    tool_uses: vec![],
                },
            },
            Timed {
                at: mid + 240_000,
                record: Record::Assistant {
                    request: Some("b".into()),
                    tool_uses: vec![],
                },
            },
        ];
        let cuts = local_midnights(&toronto, mid - 240_000, mid + 240_000);
        let buckets = split_at(&records, &[], IDLE_GAP_MS, &cuts).expect("records");
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].1.wall_ms, 240_000);
        assert_eq!(buckets[1].1.wall_ms, 240_000);
        assert_eq!(
            local_day(&toronto, buckets[0].0).as_deref(),
            Some("2026-11-01")
        );
        assert_eq!(
            local_day(&toronto, buckets[1].0).as_deref(),
            Some("2026-11-02")
        );
        assert_eq!(
            sum(&buckets),
            split(&records, &[], IDLE_GAP_MS).expect("records")
        );
    }
}
