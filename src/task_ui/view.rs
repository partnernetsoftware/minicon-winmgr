//! Presentation only: no native queries or management side effects.
use crate::task_service::{Identity, Process, Snapshot};
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
};

pub(super) struct Row {
    pub identity: Identity,
    pub cells: Vec<String>,
    pub children: bool,
}

pub(super) fn rows(
    snapshot: &Snapshot,
    filter: &str,
    network: bool,
    tree: bool,
    expanded: &HashSet<Identity>,
    sort: (usize, bool),
) -> Vec<Row> {
    let query = filter.to_lowercase();
    let matches = |p: &Process| {
        query.is_empty()
            || p.name.to_lowercase().contains(&query)
            || p.path
                .as_ref()
                .is_some_and(|path| path.to_lowercase().contains(&query))
            || p.identity.pid.to_string().contains(&query)
            || p.windows
                .iter()
                .any(|w| w.title.to_lowercase().contains(&query))
    };
    let by_id: HashMap<_, _> = snapshot.processes.iter().map(|p| (p.identity, p)).collect();
    if network {
        let mut result: Vec<Row> = snapshot
            .endpoints
            .iter()
            .filter_map(|e| {
                let p = by_id.get(&e.process)?;
                if !matches(p)
                    && !e.local.to_lowercase().contains(&query)
                    && !e
                        .remote
                        .as_ref()
                        .is_some_and(|r| r.to_lowercase().contains(&query))
                {
                    return None;
                }
                Some(Row {
                    identity: e.process,
                    children: false,
                    cells: vec![
                        p.name.clone(),
                        p.identity.pid.to_string(),
                        e.protocol.clone(),
                        e.local.clone(),
                        e.remote.clone().unwrap_or_else(|| "—".into()),
                        e.state.clone().unwrap_or_else(|| "—".into()),
                    ],
                })
            })
            .collect();
        result.sort_by(|a, b| {
            let column = sort.0.min(5);
            let order = if column == 1 {
                a.identity.pid.cmp(&b.identity.pid)
            } else {
                a.cells[column].cmp(&b.cells[column])
            };
            let a_session = by_id[&a.identity].session;
            let b_session = by_id[&b.identity].session;
            (a_session != Some(snapshot.session))
                .cmp(&(b_session != Some(snapshot.session)))
                .then(a_session.cmp(&b_session))
                .then(if sort.1 { order.reverse() } else { order })
                .then(a.identity.pid.cmp(&b.identity.pid))
                .then(a.cells.cmp(&b.cells))
        });
        return result;
    }
    let mut processes: Vec<_> = snapshot.processes.iter().filter(|p| matches(p)).collect();
    let comparison = |a: &&Process, b: &&Process| {
        let order = match sort.0 {
            1 => a.identity.pid.cmp(&b.identity.pid),
            2 => a.session.cmp(&b.session),
            3 => number(a.cpu_percent, b.cpu_percent, sort.1),
            4 => number(
                a.working_bytes.map(|n| n as f64),
                b.working_bytes.map(|n| n as f64),
                sort.1,
            ),
            5 => number(
                a.private_bytes.map(|n| n as f64),
                b.private_bytes.map(|n| n as f64),
                sort.1,
            ),
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        };
        let order = if sort.1 && !(3..=5).contains(&sort.0) {
            order.reverse()
        } else {
            order
        };
        (a.session != Some(snapshot.session))
            .cmp(&(b.session != Some(snapshot.session)))
            .then(a.session.cmp(&b.session))
            .then(order)
            .then(a.identity.pid.cmp(&b.identity.pid))
            .then(a.identity.created_100ns.cmp(&b.identity.created_100ns))
    };
    processes.sort_by(comparison);
    let make = |p: &Process, depth: usize, children: bool| Row {
        identity: p.identity,
        children,
        cells: vec![
            format!(
                "{}{}{}",
                "  ".repeat(depth),
                if children {
                    if expanded.contains(&p.identity) {
                        "▾ "
                    } else {
                        "▸ "
                    }
                } else {
                    ""
                },
                if p.session == Some(snapshot.session) {
                    p.name.clone()
                } else {
                    format!(
                        "[S{}] {}",
                        p.session.map_or_else(|| "?".into(), |n| n.to_string()),
                        p.name
                    )
                }
            ),
            p.identity.pid.to_string(),
            p.session.map_or_else(|| "—".into(), |n| n.to_string()),
            p.cpu_percent
                .map_or_else(|| "—".into(), |n| format!("{n:.1}")),
            mib(p.working_bytes),
            mib(p.private_bytes),
            p.windows
                .first()
                .map_or_else(String::new, |w| w.title.clone()),
        ],
    };
    if !tree || !query.is_empty() {
        return processes.into_iter().map(|p| make(p, 0, false)).collect();
    }
    let pid: HashMap<_, _> = processes.iter().map(|p| (p.identity.pid, *p)).collect();
    let mut children: HashMap<Identity, Vec<&Process>> = HashMap::new();
    let mut roots = Vec::new();
    for p in &processes {
        if let Some(parent) = pid.get(&p.parent_pid).filter(|parent| {
            parent.identity != p.identity
                && parent.identity.created_100ns < p.identity.created_100ns
                && parent.session == p.session
        }) {
            children.entry(parent.identity).or_default().push(*p);
        } else {
            roots.push(*p);
        }
    }
    let mut output = Vec::new();
    let mut stack: Vec<_> = roots.into_iter().rev().map(|p| (p, 0)).collect();
    while let Some((p, depth)) = stack.pop() {
        let kids = children.get(&p.identity);
        output.push(make(p, depth, kids.is_some()));
        if expanded.contains(&p.identity)
            && depth < 64
            && let Some(kids) = kids
        {
            for p in kids.iter().rev() {
                stack.push((*p, depth + 1));
            }
        }
    }
    output
}

fn number(a: Option<f64>, b: Option<f64>, descending: bool) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => {
            if descending {
                b.total_cmp(&a)
            } else {
                a.total_cmp(&b)
            }
        }
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        _ => Ordering::Equal,
    }
}
fn mib(bytes: Option<u64>) -> String {
    bytes.map_or_else(|| "—".into(), |n| format!("{:.1}", n as f64 / 1048576.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_metrics_stay_last_and_recycled_parent_is_not_a_tree_parent() {
        assert_eq!(number(None, Some(1.0), true), Ordering::Greater);
        let process = |pid, created, parent| Process {
            identity: Identity {
                pid,
                created_100ns: created,
            },
            parent_pid: parent,
            name: "fixture".into(),
            owner: Some("test".into()),
            session: Some(1),
            path: None,
            cpu_percent: None,
            working_bytes: None,
            private_bytes: None,
            windows: vec![],
        };
        let s = Snapshot {
            schema: 1,
            platform: "fixture".into(),
            sampled_unix_ms: 0,
            owner: "test".into(),
            execution_owner: "test".into(),
            scope: crate::task_service::Scope::OwnTasks,
            unavailable: vec![],
            session: 1,
            processes: vec![process(1, 200, 0), process(2, 100, 1)],
            endpoints: vec![],
            ownership_unavailable: 0,
            limitations: vec![],
        };
        let rows = rows(&s, "", false, true, &HashSet::new(), (1, false));
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| !r.children));
    }
}
