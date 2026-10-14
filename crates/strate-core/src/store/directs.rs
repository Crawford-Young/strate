//! Director-launched units (#53): `directs` names, for a unit session, the
//! director session that launched it. A session sent a message when one of
//! its records (its subagents' too) holds the SendMessage result's msg_id;
//! another received it when a record of its own holds that msg_id as a peer
//! message. The edge set is a pure function of the stored events, rebuilt
//! whole, so any ingest order converges to a cold build.

use std::collections::BTreeMap;

use rusqlite::{Connection, params};

use super::Result;
use crate::names::is_director;

/// A message one session received from another: a candidate edge.
#[derive(Debug, Clone, PartialEq)]
struct Candidate {
    parent: String,
    child: String,
    msg_id: String,
}

/// Each child's parent and the msg_id that made the edge, from candidates in
/// message order: a child's first candidate wins, unless it would close a
/// cycle (its parent is the child or one of its descendants), when the next
/// one gets the chance.
fn choose(candidates: impl IntoIterator<Item = Candidate>) -> BTreeMap<String, (String, String)> {
    let mut chosen: BTreeMap<String, (String, String)> = BTreeMap::new();
    for c in candidates {
        if chosen.contains_key(&c.child) {
            continue;
        }
        // Walk up from the parent; the chosen edges form a forest, so this ends.
        let mut up = Some(c.parent.as_str());
        while let Some(at) = up
            && at != c.child
        {
            up = chosen.get(at).map(|(parent, _)| parent.as_str());
        }
        if up.is_none() {
            chosen.insert(c.child, (c.parent, c.msg_id));
        }
    }
    chosen
}

/// Rebuilds `directs` from the stored events: a session received a message
/// that another sent, from a sender whose name is a director's, in the
/// order of the received record's timestamp, then msg_id.
pub(super) fn rebuild(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT s.session_id, r.session_id, r.peer_msg_id, r.peer_name, r.timestamp
         FROM events AS r JOIN events AS s ON s.sent_msg_id = r.peer_msg_id
         WHERE r.peer_msg_id IS NOT NULL AND s.session_id <> r.session_id
         ORDER BY r.timestamp IS NULL, r.timestamp, r.peer_msg_id, s.session_id, r.session_id",
    )?;
    let mut candidates = Vec::new();
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let name: Option<String> = r.get(3)?;
        if name.as_deref().is_some_and(is_director) {
            candidates.push(Candidate {
                parent: r.get(0)?,
                child: r.get(1)?,
                msg_id: r.get(2)?,
            });
        }
    }
    conn.execute("DELETE FROM directs", [])?;
    let mut insert =
        conn.prepare_cached("INSERT INTO directs (child, parent, msg_id) VALUES (?1, ?2, ?3)")?;
    for (child, (parent, msg_id)) in choose(candidates) {
        insert.execute(params![child, parent, msg_id])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sent(parent: &str, child: &str, msg_id: &str) -> Candidate {
        Candidate {
            parent: parent.into(),
            child: child.into(),
            msg_id: msg_id.into(),
        }
    }

    fn edges(chosen: &BTreeMap<String, (String, String)>) -> Vec<(&str, &str, &str)> {
        chosen
            .iter()
            .map(|(child, (parent, msg))| (parent.as_str(), child.as_str(), msg.as_str()))
            .collect()
    }

    #[test]
    fn the_first_messager_wins() {
        let chosen = choose([
            sent("d1", "u1", "m1"),
            sent("d2", "u1", "m2"),
            sent("d2", "u2", "m3"),
        ]);
        assert_eq!(edges(&chosen), [("d1", "u1", "m1"), ("d2", "u2", "m3")]);
    }

    #[test]
    fn a_candidate_closing_a_cycle_is_refused_and_a_later_one_may_win() {
        // a -> b -> c, then c -> a closes a cycle; b -> a is refused too; z
        // -> a is the first that can parent a.
        let chosen = choose([
            sent("a", "b", "m1"),
            sent("b", "c", "m2"),
            sent("c", "a", "m3"),
            sent("b", "a", "m4"),
            sent("z", "a", "m5"),
        ]);
        assert_eq!(
            edges(&chosen),
            [("z", "a", "m5"), ("a", "b", "m1"), ("b", "c", "m2")]
        );
        assert!(choose([sent("a", "a", "m1")]).is_empty(), "no self-edge");
    }
}
