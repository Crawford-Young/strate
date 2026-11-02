//! Each cell's path of steps, and the rollup tree built by inserting every
//! cell along its path.

use std::collections::HashMap;

use super::cells::{Cell, Cells, Place, Share, Time};
use super::gh::{Gh, GhPr};
use super::{AuditNode, Metrics, NodeKind, Progress, Slice, Tokens};

/// The rollup tree over `cells`.
pub(crate) fn tree(cells: &Cells, gh: &Gh) -> AuditNode {
    let mut root = Node::root();
    for cell in &cells.list {
        let keys = Keys {
            day: &cell.day,
            role: cells.role(cell),
            model: cell.model.as_deref().unwrap_or_default(),
        };
        root.insert(path(cell, cells, gh), &cell.share, &keys);
    }
    root.finish().0
}

/// A cell's key in each split; `""` where it has none.
struct Keys<'a> {
    day: &'a str,
    role: &'a str,
    model: &'a str,
}

/// How a node's elapsed time ends.
#[derive(Clone, Copy)]
enum End {
    /// Its own spans' (an agent): its elapsed is the time they cover.
    Own,
    /// Closed or merged at.
    Closed(i64),
    /// An open epic, issue or PR: at its last activity.
    Open,
    /// Its children's: earliest start to latest end, open if any is.
    Derived,
}

/// One level of a cell's path, as the node it makes. A node is the step
/// whose `(kind, key)` is unique among its siblings.
pub(crate) struct Step {
    pub(crate) kind: NodeKind,
    pub(crate) key: String,
    label: String,
    /// Sorts siblings of one kind.
    order: String,
    end: End,
    /// The sessions or runs it spans, each once.
    spans: Vec<(i64, i64)>,
    elapsed: Option<i64>,
    progress: Option<Progress>,
    /// Read from the GitHub cache.
    github: bool,
}

impl Step {
    /// When an epic, issue or PR closed or merged, if GitHub gave a time.
    pub(crate) fn closed_at(&self) -> Option<i64> {
        match self.end {
            End::Closed(at) => Some(at),
            _ => None,
        }
    }

    /// Another cell's copy of this step: a main agent spans each of its
    /// sessions. Every other step's copies are the same.
    fn widen(&mut self, other: &Self) {
        for span in &other.spans {
            if !self.spans.contains(span) {
                self.spans.push(*span);
            }
        }
    }

    fn new(kind: NodeKind, key: String, label: String, order: String) -> Self {
        Self {
            kind,
            key,
            label,
            order,
            end: End::Derived,
            spans: Vec::new(),
            elapsed: None,
            progress: None,
            github: false,
        }
    }

    /// An epic or issue node from the cache. One the cache doesn't hold is
    /// `<repo> #N`, open: first session to last activity.
    fn issue(kind: NodeKind, gh: &Gh, repo: &str, number: i64) -> Self {
        let cached = gh.issue(repo, number);
        let mut step = Self::new(
            kind,
            number.to_string(),
            match cached {
                Some(i) => format!("#{number} {}", i.title),
                None => format!("{repo} #{number}"),
            },
            format!("{number:020}"),
        );
        step.end = match cached.map(|i| i.closed) {
            Some(Some(Some(at))) => End::Closed(at),
            // Closed with no time given.
            Some(Some(None)) => End::Derived,
            Some(None) | None => End::Open,
        };
        step.progress = cached.and_then(|i| i.progress.clone());
        step.github = cached.is_some();
        step
    }
}

/// `cell`'s path from the root: its chain row's place (see
/// [`place_steps`]) and its Agent node. The tree adds the cell to every
/// node on it; the cells under a node are those whose path passes through
/// that node's `(kind, key)` steps.
pub(crate) fn path(cell: &Cell, cells: &Cells, gh: &Gh) -> Vec<Step> {
    let mut steps = place_steps(&cells.row_of(cell).place, gh);
    steps.push(agent_step(cell, cells));
    steps
}

/// The steps a chain row is placed by: Unlinked; a Topic; a Repo; Repo >
/// Epic for an epic; Repo > Epic > Issue > PR for an issue (an issue sits
/// under each of its epics, outermost first, and an epic's own issue work
/// under its "Direct work" Issue node, beside its sub-issues); Repo > PR
/// for a PR the cache doesn't hold.
fn place_steps(place: &Place, gh: &Gh) -> Vec<Step> {
    let repo_step = |repo: &str| {
        Step::new(
            NodeKind::Repo,
            repo.to_string(),
            repo.to_string(),
            repo.to_string(),
        )
    };
    let (repo, issue, pr) = match place {
        Place::Unlinked => {
            return vec![Step::new(
                NodeKind::Unlinked,
                String::new(),
                "Unlinked".into(),
                String::new(),
            )];
        }
        Place::Topic(topic) => {
            return vec![Step::new(
                NodeKind::Topic,
                topic.clone(),
                topic.clone(),
                topic.clone(),
            )];
        }
        Place::Repo(repo) => return vec![repo_step(repo)],
        Place::Epic { repo, number } => {
            let mut steps = vec![repo_step(repo)];
            for epic in epics_over(gh, repo, *number) {
                steps.push(Step::issue(NodeKind::Epic, gh, repo, epic));
            }
            steps.push(Step::issue(NodeKind::Epic, gh, repo, *number));
            return steps;
        }
        Place::Pr { repo, number } => return vec![repo_step(repo), uncached_pr_step(*number)],
        Place::Issue { repo, issue, pr } => (repo, *issue, *pr),
    };
    let mut steps = Vec::with_capacity(4);
    steps.push(repo_step(repo));
    for epic in epics_over(gh, repo, issue) {
        steps.push(Step::issue(NodeKind::Epic, gh, repo, epic));
    }
    if gh.is_epic(repo, issue) {
        steps.push(Step::issue(NodeKind::Epic, gh, repo, issue));
        let mut direct = Step::issue(NodeKind::Issue, gh, repo, issue);
        direct.label = "Direct work".into();
        direct.progress = None;
        steps.push(direct);
    } else {
        steps.push(Step::issue(NodeKind::Issue, gh, repo, issue));
    }
    if let Some(number) = pr {
        steps.push(pr_step(gh, repo, issue, number));
    }
    steps
}

/// The epics `repo`#`issue` is under, outermost first: its parent, the
/// parent's parent, and on while the cache holds the next. GitHub allows no
/// cycle, but the cache could hold one (an issue moved during a paged
/// refresh), so the walk stops at the first repeat.
fn epics_over(gh: &Gh, repo: &str, issue: i64) -> Vec<i64> {
    let mut epics = Vec::new();
    let mut at = issue;
    while let Some(parent) = gh.issue(repo, at).and_then(|i| i.parent) {
        if parent == issue || epics.contains(&parent) {
            break;
        }
        epics.push(parent);
        at = parent;
    }
    epics.reverse();
    epics
}

/// PR `number` closing `repo`#`issue`: closed at its merge, open while
/// GitHub says so.
fn pr_step(gh: &Gh, repo: &str, issue: i64, number: i64) -> Step {
    let mut step = uncached_pr_step(number);
    step.end = match gh.pr(repo, issue, number) {
        Some(GhPr {
            merged_at: Some(at),
            ..
        }) => End::Closed(*at),
        Some(p) if p.state == "OPEN" => End::Open,
        _ => End::Derived,
    };
    step.github = true;
    step
}

/// PR `number` as a charge names it, the cache holding nothing of it:
/// open, first session to last activity.
fn uncached_pr_step(number: i64) -> Step {
    let mut step = Step::new(
        NodeKind::Pr,
        number.to_string(),
        format!("PR #{number}"),
        format!("{number:020}"),
    );
    step.end = End::Open;
    step
}

/// `cell`'s Agent node. A main agent's cell: its chain row's main agent,
/// keyed and labelled by the row, over its session. A subagent's: keyed
/// `<session id>/<agent id>`, over its run. Siblings sort by their chain
/// row's start, then its key, the main agent first, then the row's
/// subagents by run start (none: 0), then key. The parts are joined by NUL,
/// which no key holds, so the order is the parts' in turn.
fn agent_step(cell: &Cell, cells: &Cells) -> Step {
    let row = cells.row_of(cell);
    let lead = format!("{:020}\0{}\0", row.start.unwrap_or(0), row.key);
    let mut step = match &cell.agent_id {
        None => {
            let mut step = Step::new(NodeKind::Agent, row.key.clone(), row.label.clone(), lead);
            step.spans.extend(cells.session(cell).span);
            step
        }
        Some(id) => {
            let agent = cells.agent(cell, id);
            let key = format!("{}/{id}", cell.session_id);
            let label = agent.map_or_else(|| id.clone(), |a| a.label.clone());
            let run_start = agent.and_then(|a| a.span).map_or(0, |s| s.0);
            let order = format!("{lead}{run_start:020}\0{key}");
            let mut step = Step::new(NodeKind::Agent, key, label, order);
            step.spans.extend(agent.and_then(|a| a.span));
            step.elapsed = agent.and_then(|a| a.run_ms);
            step
        }
    };
    step.end = End::Own;
    step
}

/// A node while the tree is built.
struct Node {
    step: Step,
    metrics: Metrics,
    by_day: Split,
    by_role: Split,
    by_model: Split,
    children: Vec<Node>,
    index: HashMap<(NodeKind, String), usize>,
}

/// A node's $ and agent-hours per key of one split, while the tree is built.
#[derive(Default)]
struct Split(HashMap<String, Slice>);

impl Split {
    fn add(&mut self, key: &str, s: &Share) {
        let slice = match self.0.get_mut(key) {
            Some(slice) => slice,
            None => self.0.entry(key.to_string()).or_insert_with(|| Slice {
                key: key.to_string(),
                ..Slice::default()
            }),
        };
        slice.cost_usd += s.cost_usd.unwrap_or(0.0);
        slice.agent_ms += s.agent_ms;
    }

    /// The slices, in key order.
    fn finish(self) -> Vec<Slice> {
        let mut slices: Vec<Slice> = self.0.into_values().collect();
        slices.sort_unstable_by(|a, b| a.key.cmp(&b.key));
        slices
    }
}

impl Node {
    fn root() -> Self {
        Self::new(Step::new(
            NodeKind::Root,
            String::new(),
            "All".into(),
            String::new(),
        ))
    }

    fn new(step: Step) -> Self {
        Self {
            step,
            metrics: Metrics::default(),
            by_day: Split::default(),
            by_role: Split::default(),
            by_model: Split::default(),
            children: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// Adds `share` to this node and to each node along `steps`, under
    /// `keys` in each split, creating the missing ones and widening the rest.
    fn insert(&mut self, steps: Vec<Step>, share: &Share, keys: &Keys) {
        self.add(share, keys);
        let mut node = self;
        for step in steps {
            let key = (step.kind, step.key.clone());
            let i = match node.index.get(&key) {
                Some(&i) => {
                    node.children[i].step.widen(&step);
                    i
                }
                None => {
                    node.children.push(Node::new(step));
                    node.index.insert(key, node.children.len() - 1);
                    node.children.len() - 1
                }
            };
            node = &mut node.children[i];
            node.add(share, keys);
        }
    }

    fn add(&mut self, s: &Share, keys: &Keys) {
        self.by_day.add(keys.day, s);
        self.by_role.add(keys.role, s);
        self.by_model.add(keys.model, s);
        let m = &mut self.metrics;
        if let Some(c) = s.cost_usd {
            m.cost_usd += c;
        }
        m.unpriced += s.unpriced;
        m.tokens.add(&s.tokens);
        m.agent_ms += s.agent_ms;
        let Time {
            active_ms, wait_ms, ..
        } = if self.step.kind == NodeKind::Agent {
            s.own
        } else {
            s.line
        };
        m.active_ms += active_ms;
        m.wait_ms += wait_ms;
        m.sessions += s.sessions;
        m.agents += s.agents;
    }

    /// The finished node, its span and whether it is open.
    fn finish(self) -> (AuditNode, Option<(i64, i64)>, bool) {
        let Node {
            step,
            mut metrics,
            by_day,
            by_role,
            by_model,
            mut children,
            ..
        } = self;
        metrics.by_day = by_day.finish();
        metrics.by_role = by_role.finish();
        metrics.by_model = by_model.finish();
        children.sort_by(|a, b| {
            (a.step.kind as u8, &a.step.order).cmp(&(b.step.kind as u8, &b.step.order))
        });
        let finished: Vec<_> = children.into_iter().map(Node::finish).collect();
        let start = finished.iter().filter_map(|f| f.1.map(|s| s.0)).min();
        let end = finished.iter().filter_map(|f| f.1.map(|s| s.1)).max();
        let any_open = finished.iter().any(|f| f.2);
        let (span, open) = match step.end {
            End::Own => {
                let start = step.spans.iter().map(|s| s.0).chain(start).min();
                let end = step.spans.iter().map(|s| s.1).chain(end).max();
                (start.zip(end), false)
            }
            End::Closed(at) => (start.map(|s| (s, at)), false),
            End::Open => (start.zip(end), true),
            End::Derived => (start.zip(end), any_open),
        };
        metrics.elapsed_ms = step.elapsed.or(match step.end {
            End::Own => covered(step.spans),
            _ => span.map(|(s, e)| (e - s).max(0)),
        });
        metrics.open = open;
        metrics.progress = step.progress;
        let node = AuditNode {
            kind: step.kind,
            key: step.key,
            label: step.label,
            github: step.github,
            metrics,
            children: finished.into_iter().map(|f| f.0).collect(),
        };
        (node, span, open)
    }
}

/// The time `spans` cover, each instant once; `None` with none.
fn covered(mut spans: Vec<(i64, i64)>) -> Option<i64> {
    spans.sort_unstable();
    let mut total = None;
    let mut reached = i64::MIN;
    for (start, end) in spans {
        let start = start.max(reached);
        *total.get_or_insert(0) += (end - start).max(0);
        reached = reached.max(end);
    }
    total
}

impl Tokens {
    pub(super) fn add(&mut self, other: &Self) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }
}
