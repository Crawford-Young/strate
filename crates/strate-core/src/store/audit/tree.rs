//! Each cell's path of steps, and the rollup tree built by inserting every
//! cell along its path.

use std::collections::HashMap;

use super::cells::{Cell, Cells, Place, Share, Time, Window};
use super::gh::{Gh, GhPr};
use super::{AuditNode, GroupBy, Metrics, NodeKind, Progress, Tokens};

/// The rollup tree over `cells`, its top level grouped by `group_by`.
pub(crate) fn tree(group_by: GroupBy, cells: &Cells, gh: &Gh) -> AuditNode {
    let mut root = Node::root();
    for cell in &cells.list {
        root.insert(path(group_by, cell, cells, gh), &cell.share);
    }
    root.finish().0
}

/// How a node's elapsed time ends.
#[derive(Clone, Copy)]
enum End {
    /// Its own span (a session or agent).
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
    span: Option<(i64, i64)>,
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

    fn new(kind: NodeKind, key: String, label: String, order: String) -> Self {
        Self {
            kind,
            key,
            label,
            order,
            end: End::Derived,
            span: None,
            elapsed: None,
            progress: None,
            github: false,
        }
    }

    /// An epic or issue node from the cache. One the cache doesn't hold is
    /// `<repo> #N`, open: first window to last activity.
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

/// `cell`'s path from the root: its Group node under `group_by` (none under
/// Hierarchy), then the place of its window (Repo > Epic > Issue > PR, or
/// Unlinked), its window's Session node and its Agent node. The tree adds
/// the cell to every node on it; the cells under a node are those whose
/// path passes through that node's `(kind, key)` steps.
pub(crate) fn path(group_by: GroupBy, cell: &Cell, cells: &Cells, gh: &Gh) -> Vec<Step> {
    let window = cells.window(cell);
    let mut steps = Vec::with_capacity(7);
    steps.extend(group_step(group_by, cell, cells, gh));
    steps.extend(place_steps(&window.place, gh));
    steps.push(window_step(window));
    steps.push(agent_step(cell, cells));
    steps
}

/// `cell`'s Group node under `group_by`; Hierarchy has none.
fn group_step(group_by: GroupBy, cell: &Cell, cells: &Cells, gh: &Gh) -> Option<Step> {
    let (key, label) = match group_by {
        GroupBy::Hierarchy => return None,
        GroupBy::Day => (cell.day.clone(), cell.day.clone()),
        GroupBy::Model => match &cell.model {
            Some(m) => (m.clone(), m.clone()),
            None => (String::new(), "No requests".into()),
        },
        // Session time is on the orchestrator's cells, so a subagent type's
        // group shows 0 session-level active and waiting; its Agent nodes
        // carry their own.
        GroupBy::AgentType => {
            let kind = match &cell.agent_id {
                None => Some("orchestrator".to_string()),
                Some(id) => cells.agent(cell, id).and_then(|a| a.agent_type.clone()),
            };
            match kind {
                Some(k) => (k.clone(), k),
                None => (String::new(), "Unknown".into()),
            }
        }
        // Keyed by the PR's own repo, which may not be the issue's.
        GroupBy::Pr => match &cells.window(cell).place {
            Place::Linked {
                repo,
                issue,
                pr: Some(n),
            } => {
                let repo = gh.pr(repo, *issue, *n).map_or(repo, |p| &p.repo);
                (format!("{repo}#{n}"), format!("{repo} #{n}"))
            }
            _ => (String::new(), "No PR".into()),
        },
    };
    Some(Step::new(NodeKind::Group, key.clone(), label, key))
}

/// The steps a window's place makes: Repo > Epic > Issue > PR, or Unlinked.
fn place_steps(place: &Place, gh: &Gh) -> Vec<Step> {
    let (repo, issue, pr) = match place {
        Place::Unlinked => {
            return vec![Step::new(
                NodeKind::Unlinked,
                String::new(),
                "Unlinked".into(),
                String::new(),
            )];
        }
        Place::Linked { repo, issue, pr } => (repo, *issue, *pr),
    };
    let mut steps = Vec::with_capacity(4);
    steps.push(Step::new(
        NodeKind::Repo,
        repo.clone(),
        repo.clone(),
        repo.clone(),
    ));
    if let Some(epic) = gh.issue(repo, issue).and_then(|i| i.parent) {
        steps.push(Step::issue(NodeKind::Epic, gh, repo, epic));
        steps.push(Step::issue(NodeKind::Issue, gh, repo, issue));
    } else if gh.is_epic(repo, issue) {
        // An epic with a window of its own: one Epic node, the window under
        // its "Direct work" Issue node beside the sub-issues.
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

/// PR `number` closing `repo`#`issue`: closed at its merge, open while
/// GitHub says so.
fn pr_step(gh: &Gh, repo: &str, issue: i64, number: i64) -> Step {
    let mut step = Step::new(
        NodeKind::Pr,
        number.to_string(),
        format!("PR #{number}"),
        format!("{number:020}"),
    );
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

/// A window's Session node, sorted by its start.
fn window_step(window: &Window) -> Step {
    let start = window.span.map_or(0, |s| s.0);
    let mut step = Step::new(
        NodeKind::Session,
        window.key.clone(),
        window.label.clone(),
        format!("{start:020}/{}", window.key),
    );
    step.end = End::Own;
    step.span = window.span;
    step
}

/// `cell`'s Agent node: the orchestrator over its window's span, or a
/// subagent over its run.
fn agent_step(cell: &Cell, cells: &Cells) -> Step {
    let mut step = match &cell.agent_id {
        None => {
            let mut step = Step::new(
                NodeKind::Agent,
                String::new(),
                "orchestrator".into(),
                String::new(),
            );
            step.span = cells.window(cell).span;
            step
        }
        Some(id) => {
            let agent = cells.agent(cell, id);
            let label = agent.map_or_else(|| id.clone(), |a| a.label.clone());
            let mut step = Step::new(NodeKind::Agent, id.clone(), label, id.clone());
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
    children: Vec<Node>,
    index: HashMap<(NodeKind, String), usize>,
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
            children: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// Adds `share` to this node and to each node along `steps`, creating
    /// the missing ones.
    fn insert(&mut self, steps: Vec<Step>, share: &Share) {
        self.add(share);
        let mut node = self;
        for step in steps {
            let key = (step.kind, step.key.clone());
            let i = match node.index.get(&key) {
                Some(&i) => i,
                None => {
                    node.children.push(Node::new(step));
                    node.index.insert(key, node.children.len() - 1);
                    node.children.len() - 1
                }
            };
            node = &mut node.children[i];
            node.add(share);
        }
    }

    fn add(&mut self, s: &Share) {
        let m = &mut self.metrics;
        if let Some(c) = s.cost_usd {
            m.cost_usd += c;
        }
        m.unpriced += s.unpriced;
        m.tokens.add(&s.tokens);
        m.agent_ms += s.agent_ms;
        m.approximate |= s.approximate;
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
        m.rotations += s.rotations;
    }

    /// The finished node, its span and whether it is open.
    fn finish(self) -> (AuditNode, Option<(i64, i64)>, bool) {
        let Node {
            step,
            mut metrics,
            mut children,
            ..
        } = self;
        children.sort_by(|a, b| {
            (a.step.kind as u8, &a.step.order).cmp(&(b.step.kind as u8, &b.step.order))
        });
        let finished: Vec<_> = children.into_iter().map(Node::finish).collect();
        let start = finished.iter().filter_map(|f| f.1.map(|s| s.0)).min();
        let end = finished.iter().filter_map(|f| f.1.map(|s| s.1)).max();
        let any_open = finished.iter().any(|f| f.2);
        let (span, open) = match step.end {
            End::Own => (step.span, false),
            End::Closed(at) => (start.map(|s| (s, at)), false),
            End::Open => (start.zip(end), true),
            End::Derived => (start.zip(end), any_open),
        };
        metrics.elapsed_ms = step.elapsed.or(span.map(|(s, e)| (e - s).max(0)));
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

impl Tokens {
    pub(super) fn add(&mut self, other: &Self) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }
}
