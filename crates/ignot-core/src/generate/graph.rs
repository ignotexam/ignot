use std::collections::BTreeMap;

use super::{Generated, Level};
use crate::grade::GradeKind;
use crate::rng::Rng;
use crate::{Result, invalid};

/// Nodes, edges and the limit of edges on a route, per level.
const SIZE: [(u32, usize, u32); 5] = [(15, 40, 3), (25, 80, 4), (40, 160, 5), (60, 260, 6), (90, 420, 7)];

/// Directed edges and their weights, keyed by (from, to).
type Edges = BTreeMap<(u32, u32), u32>;

/// The cheapest way from `from` to `to` over at most `limit` edges: one round of relaxation per edge allowed.
fn cheapest_within(nodes: u32, edges: &Edges, from: u32, to: u32, limit: u32) -> Option<u32> {
    let mut reach: Vec<Option<u32>> = vec![None; nodes as usize];
    reach[from as usize] = Some(0);
    let mut best = if from == to { Some(0) } else { None };
    for _ in 0..limit {
        let mut next: Vec<Option<u32>> = vec![None; nodes as usize];
        for (&(u, v), &weight) in edges {
            if let Some(cost) = reach[u as usize] {
                let cost = cost + weight;
                if next[v as usize].is_none_or(|known| cost < known) {
                    next[v as usize] = Some(cost);
                }
            }
        }
        reach = next;
        if let Some(cost) = reach[to as usize] {
            best = Some(best.map_or(cost, |known| known.min(cost)));
        }
    }
    best
}

/// The cheapest way from `from` to `to` over any number of edges.
fn cheapest(nodes: u32, edges: &Edges, from: u32, to: u32) -> Option<u32> {
    cheapest_within(nodes, edges, from, to, nodes.saturating_sub(1))
}

pub(super) fn graph(rng: &mut Rng, level: Level) -> Result<Generated> {
    let (nodes, edge_count, limit) = level.pick(SIZE);
    let all: Vec<u32> = (0..nodes).collect();
    let (from, to) = (0, nodes - 1);
    for _ in 0..5000 {
        let mut edges = Edges::new();
        while edges.len() < edge_count {
            let pair = rng.sample(&all, 2);
            let (u, v) = (pair[0], pair[1]);
            if u != to && v != from {
                edges.insert((u, v), rng.int(1, 30));
            }
        }
        // Keep only graphs where the limit matters: the cheapest route overall must need too many edges.
        let Some(answer) = cheapest_within(nodes, &edges, from, to, limit) else {
            continue;
        };
        if cheapest(nodes, &edges, from, to) == Some(answer) {
            continue;
        }
        let list: Vec<String> = edges.iter().map(|(&(u, v), weight)| format!("N{u} -> N{v}: {weight}")).collect();
        return Ok(Generated {
            prompt: format!(
                "Directed edges with weights:\n{}\n\nWhat is the smallest total weight of a path from N{from} to N{to} that uses at most {limit} edges? Answer with the number only.",
                list.join("\n")
            ),
            answer: answer.to_string(),
            grade: GradeKind::Num,
        });
    }
    invalid("graph: no graph where the limit of edges matters")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tries every route of at most `limit` edges.
    fn by_trying_every_route(edges: &Edges, at: u32, to: u32, limit: u32) -> Option<u32> {
        let mut best = if at == to { Some(0) } else { None };
        if limit > 0 {
            for (&(u, v), &weight) in edges {
                if u == at
                    && let Some(rest) = by_trying_every_route(edges, v, to, limit - 1)
                {
                    best = Some(best.map_or(rest + weight, |known| known.min(rest + weight)));
                }
            }
        }
        best
    }

    fn parse(prompt: &str) -> (Edges, u32, u32, u32) {
        let mut edges = Edges::new();
        for line in prompt.lines().filter(|line| line.contains(" -> ")) {
            let (pair, weight) = line.split_once(": ").unwrap();
            let (u, v) = pair.split_once(" -> ").unwrap();
            let previous = edges.insert((u[1..].parse().unwrap(), v[1..].parse().unwrap()), weight.parse().unwrap());
            assert!(previous.is_none(), "an edge is listed twice");
        }
        let number = |after: &str| -> u32 {
            let rest = &prompt[prompt.find(after).unwrap() + after.len()..];
            rest[..rest.find(|c: char| !c.is_ascii_digit()).unwrap()].parse().unwrap()
        };
        (edges, number("path from N"), number(" to N"), number("at most "))
    }

    #[test]
    fn small_graphs_match_trying_every_route() {
        let mut rng = Rng::from_text("small graphs");
        for _ in 0..300 {
            let nodes = rng.int(2, 7);
            let mut edges = Edges::new();
            for _ in 0..rng.int(0, 18) {
                let (u, v) = (rng.int(0, nodes - 1), rng.int(0, nodes - 1));
                if u != v {
                    edges.insert((u, v), rng.int(1, 30));
                }
            }
            for limit in 0..=6 {
                assert_eq!(
                    cheapest_within(nodes, &edges, 0, nodes - 1, limit),
                    by_trying_every_route(&edges, 0, nodes - 1, limit)
                );
            }
            assert_eq!(cheapest(nodes, &edges, 0, nodes - 1), by_trying_every_route(&edges, 0, nodes - 1, nodes));
        }
    }

    #[test]
    fn the_key_is_the_cheapest_route_in_the_question_and_the_limit_matters() {
        for level in Level::ALL {
            for i in 0..4 {
                let question = graph(&mut Rng::from_text(&format!("graph {i}")), level).unwrap();
                let (edges, from, to, limit) = parse(&question.prompt);
                let (nodes, edge_count, expected_limit) = level.pick(SIZE);
                assert_eq!((edges.len(), from, to, limit), (edge_count, 0, nodes - 1, expected_limit));
                let answer = cheapest_within(nodes, &edges, from, to, limit).unwrap();
                assert_eq!(question.answer, answer.to_string());
                assert!(cheapest(nodes, &edges, from, to).unwrap() < answer, "a route with more edges must be cheaper");
                assert_eq!(by_dijkstra(nodes, &edges, from, to, limit), answer);
            }
        }
    }

    /// A second method for the big graphs: shortest paths over (node, edges used), settled cheapest first.
    fn by_dijkstra(nodes: u32, edges: &Edges, from: u32, to: u32, limit: u32) -> u32 {
        let mut settled = std::collections::BTreeSet::new();
        let mut open = std::collections::BTreeSet::from([(0u32, from, 0u32)]);
        while let Some((cost, node, used)) = open.pop_first() {
            if node == to {
                return cost;
            }
            if !settled.insert((node, used)) || used == limit {
                continue;
            }
            for (&(_, v), &weight) in edges.range((node, 0)..=(node, nodes)) {
                open.insert((cost + weight, v, used + 1));
            }
        }
        u32::MAX
    }
}
