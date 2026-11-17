use crate::{Function, Location};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize)]
pub struct Edge {
    pub caller: usize,
    pub callee: usize,
    pub confidence: String,
    pub location: Location,
}

#[derive(Clone, Debug, Serialize)]
pub struct UnresolvedCall {
    pub caller: usize,
    pub name: String,
    pub location: Location,
    pub reason: String,
    pub candidates: Vec<usize>,
}

#[derive(Default, Serialize)]
pub struct Graph {
    pub edges: Vec<Edge>,
    pub unresolved: Vec<UnresolvedCall>,
    #[serde(skip)]
    pub incoming: Vec<Vec<usize>>,
}

/// Name resolution is intentionally conservative. Even receiver annotations do
/// not establish Rust trait dispatch or generic monomorphization.
pub fn build(functions: &[Function]) -> Graph {
    let mut names: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, function) in functions.iter().enumerate() {
        names.entry(&function.name).or_default().push(index);
    }
    let mut graph = Graph {
        incoming: vec![Vec::new(); functions.len()],
        ..Default::default()
    };
    for (caller, function) in functions.iter().enumerate() {
        for call in &function.calls {
            let all = names.get(call.name.as_str()).cloned().unwrap_or_default();
            let package_candidates: Vec<_> = all
                .iter()
                .copied()
                .filter(|&index| {
                    let target = &functions[index];
                    target.package == function.package
                        || call.qualifier.split("::").next() == Some(target.package.as_str())
                        || call.receiver_type.as_ref().is_some_and(|ty| {
                            ty.split("::").next() == Some(target.package.as_str())
                        })
                })
                .collect();
            let mut candidates = package_candidates;
            let mut confidence = "unique_name_candidate";
            if let Some(receiver_type) = &call.receiver_type {
                let owner = receiver_type.split("::").last().unwrap_or(receiver_type);
                candidates.retain(|&index| functions[index].owner == owner);
                confidence = "annotated_receiver_candidate";
            } else if !call.method {
                let owner = call.qualifier.split("::").last().unwrap_or("");
                if !owner.is_empty() && candidates.iter().any(|&i| functions[i].owner == owner) {
                    candidates.retain(|&i| functions[i].owner == owner);
                    confidence = "qualified_owner_candidate";
                } else if candidates.iter().any(|&i| {
                    functions[i].owner.is_empty() && functions[i].namespace == function.namespace
                }) {
                    candidates.retain(|&i| {
                        functions[i].owner.is_empty()
                            && functions[i].namespace == function.namespace
                    });
                    confidence = "same_namespace_candidate";
                } else {
                    candidates.retain(|&i| functions[i].owner.is_empty());
                }
            }
            // Ambiguity is retained as an explicit gap, not expanded into
            // hundreds of invented call paths for common names like `close`.
            if candidates.len() == 1 {
                let callee = candidates[0];
                let edge = graph.edges.len();
                graph.edges.push(Edge {
                    caller,
                    callee,
                    confidence: confidence.into(),
                    location: call.location.clone(),
                });
                graph.incoming[callee].push(edge);
            } else {
                graph.unresolved.push(UnresolvedCall {
                    caller,
                    name: call.name.clone(),
                    location: call.location.clone(),
                    reason: if candidates.is_empty() {
                        "external, generated, aliased, field receiver, trait, or unindexed target"
                    } else {
                        "ambiguous name/receiver; no call edge asserted"
                    }
                    .into(),
                    candidates,
                });
            }
        }
    }
    graph
}

#[derive(Clone, Debug, Serialize)]
pub struct CallPath {
    pub functions: Vec<usize>,
    pub edges: Vec<usize>,
    pub bounded: bool,
}

pub fn paths(graph: &Graph, start: usize, depth: usize, limit: usize) -> (Vec<CallPath>, bool) {
    let mut result = Vec::new();
    let mut stack = vec![(vec![start], Vec::new())];
    let mut visited = BTreeSet::new();
    let mut truncated = false;
    while let Some((mut functions, mut edges)) = stack.pop() {
        let current = *functions.last().unwrap();
        if !visited.insert((current, functions.clone())) {
            continue;
        }
        let next: Vec<_> = graph.incoming[current]
            .iter()
            .copied()
            .filter(|&edge| !functions.contains(&graph.edges[edge].caller))
            .collect();
        if next.is_empty() || edges.len() >= depth {
            let bounded = !next.is_empty();
            truncated |= bounded;
            functions.reverse();
            edges.reverse();
            result.push(CallPath {
                functions,
                edges,
                bounded,
            });
            if result.len() >= limit {
                truncated |= !stack.is_empty();
                break;
            }
        } else {
            for edge in next.into_iter().rev() {
                let mut branch = functions.clone();
                branch.push(graph.edges[edge].caller);
                let mut branch_edges = edges.clone();
                branch_edges.push(edge);
                stack.push((branch, branch_edges));
            }
        }
    }
    (result, truncated)
}

pub fn stream_scope(function: &Function) -> bool {
    let package = function.package.trim_start_matches("amaterasu_");
    [
        "rtsp",
        "nvhttp",
        "stream_session",
        "session_lifecycle",
        "session",
        "launch",
        "media_transport",
        "control_transport",
        "gamestream",
    ]
    .contains(&package)
        || function.location.file.contains("/stream_lifecycle")
}
