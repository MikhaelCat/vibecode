//! Point-to-point routing over the stops of one day.
//!
//! The planners (the deterministic fallback and the offline model) used to take
//! a day's stops in "distance from the trip centre" order, which zig-zags: two
//! consecutive stops could sit on opposite sides of the region and a two-day
//! plan showed 1700 km per day, so the daily-mileage rule rejected it. A day is
//! now a shortest-path problem on a complete graph of its own stops — every
//! pair is weighted by road distance — and the visit order is built with
//! Dijkstra followed by a 2-opt improvement.
//!
//! Distances for the graph come from [`haversine_km`] scaled by a road factor,
//! because that is what is available offline; the plan's own legs are still
//! filled from OSRM or the estimation fallback in `domain::plan::build_leg`.

use crate::domain::geo::haversine_km;

/// Straight-line distances underestimate roads by roughly a fifth on Primorye
/// routes; the same factor the estimation fallback uses.
pub const ROAD_FACTOR: f64 = 1.2;

/// Complete graph of a day's stops with road-distance edge weights.
pub struct RouteGraph {
    points: Vec<(f64, f64)>,
    weights: Vec<Vec<f64>>,
}

impl RouteGraph {
    pub fn new(points: &[(f64, f64)]) -> Self {
        let mut weights = vec![vec![0.0; points.len()]; points.len()];
        for (left_index, left) in points.iter().enumerate() {
            for (right_index, right) in points.iter().enumerate() {
                if left_index != right_index {
                    weights[left_index][right_index] = haversine_km(*left, *right) * ROAD_FACTOR;
                }
            }
        }
        Self {
            points: points.to_vec(),
            weights,
        }
    }

    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Classic Dijkstra from `from`: shortest road distance to every node and
    /// the predecessor used to reach it. The graph is tiny (a day of stops), so
    /// the O(n²) scan beats a heap on overhead; both are correct.
    fn dijkstra(&self, from: usize) -> (Vec<f64>, Vec<Option<usize>>) {
        let size = self.len();
        let mut distances = vec![f64::INFINITY; size];
        let mut previous: Vec<Option<usize>> = vec![None; size];
        let mut visited = vec![false; size];
        if from >= size {
            return (distances, previous);
        }
        distances[from] = 0.0;
        for _ in 0..size {
            let mut current: Option<usize> = None;
            for node in 0..size {
                if visited[node] {
                    continue;
                }
                if current.is_none() || distances[node] < distances[current.unwrap()] {
                    current = Some(node);
                }
            }
            let Some(current) = current else { break };
            if distances[current].is_infinite() {
                break;
            }
            visited[current] = true;
            for neighbour in 0..size {
                if visited[neighbour] {
                    continue;
                }
                let candidate = distances[current] + self.weights[current][neighbour];
                if candidate < distances[neighbour] {
                    distances[neighbour] = candidate;
                    previous[neighbour] = Some(current);
                }
            }
        }
        (distances, previous)
    }

    /// Shortest path from `from` to `to` as node indices, empty when there is
    /// no path (never for a complete graph).
    pub fn path(&self, from: usize, to: usize) -> Vec<usize> {
        let (_, previous) = self.dijkstra(from);
        let mut path = vec![to];
        let mut node = to;
        while let Some(step) = previous[node] {
            path.push(step);
            node = step;
            if path.len() > self.len() {
                break;
            }
        }
        path.reverse();
        if path.first() == Some(&from) {
            path
        } else {
            Vec::new()
        }
    }

    /// Visit order for every stop, starting at `start`: repeatedly take the
    /// stop with the smallest Dijkstra path distance from the current one, then
    /// improve the tour with 2-opt. Returns the order and its road length.
    pub fn best_order(&self, start: usize) -> (Vec<usize>, f64) {
        let size = self.len();
        if size <= 1 {
            return ((0..size).collect(), 0.0);
        }
        let start = start.min(size - 1);
        let mut unvisited: Vec<usize> = (0..size).collect();
        unvisited.retain(|node| *node != start);
        let mut order = vec![start];
        let mut current = start;
        while !unvisited.is_empty() {
            let (distances, _) = self.dijkstra(current);
            let next = unvisited
                .iter()
                .min_by(|left, right| {
                    distances[**left]
                        .partial_cmp(&distances[**right])
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .copied()
                .unwrap_or(unvisited[0]);
            order.push(next);
            unvisited.retain(|node| *node != next);
            current = next;
        }
        self.two_opt(&mut order);
        let length = self.length_km(&order);
        (order, length)
    }

    /// 2-opt: reverse any segment whose reversal shortens the tour.
    pub fn two_opt(&self, order: &mut [usize]) {
        let size = order.len();
        if size < 4 {
            return;
        }
        let mut improved = true;
        let mut guard = 0;
        while improved && guard < 32 {
            improved = false;
            guard += 1;
            for left in 0..size - 2 {
                for right in left + 2..size {
                    if right + 1 == size {
                        continue;
                    }
                    let before = self.weights[order[left]][order[left + 1]]
                        + self.weights[order[right]][order[right + 1]];
                    let after = self.weights[order[left]][order[right]]
                        + self.weights[order[left + 1]][order[right + 1]];
                    if after + 1e-9 < before {
                        order[left + 1..=right].reverse();
                        improved = true;
                    }
                }
            }
        }
    }

    pub fn length_km(&self, order: &[usize]) -> f64 {
        order
            .windows(2)
            .map(|pair| self.weights[pair[0]][pair[1]])
            .sum()
    }
}

/// Orders the given points as one route starting from the one nearest to
/// `start_near`. Returns indices into `points` and the road length.
pub fn best_route_from(points: &[(f64, f64)], start_near: (f64, f64)) -> (Vec<usize>, f64) {
    if points.is_empty() {
        return (Vec::new(), 0.0);
    }
    let start = points
        .iter()
        .enumerate()
        .min_by(|left, right| {
            haversine_km(*left.1, start_near)
                .partial_cmp(&haversine_km(*right.1, start_near))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(index, _)| index)
        .unwrap_or(0);
    RouteGraph::new(points).best_order(start)
}

/// Visit order that starts from the point nearest to `start_near` and then
/// repeatedly takes the nearest unvisited point **inside the best remaining
/// priority rank**. Rank 0 keeps what the user explicitly asked for in front,
/// while each rank is walked as a route instead of a zig-zag across the region.
pub fn priority_route(points: &[(f64, f64)], ranks: &[u8], start_near: (f64, f64)) -> Vec<usize> {
    let size = points.len().min(ranks.len());
    let mut order = Vec::with_capacity(size);
    let mut visited = vec![false; size];
    let mut current = start_near;
    while order.len() < size {
        let Some(best_rank) = (0..size)
            .filter(|index| !visited[*index])
            .map(|index| ranks[index])
            .min()
        else {
            break;
        };
        let next = (0..size)
            .filter(|index| !visited[*index] && ranks[*index] == best_rank)
            .min_by(|left, right| {
                haversine_km(current, points[*left])
                    .partial_cmp(&haversine_km(current, points[*right]))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        let Some(next) = next else { break };
        visited[next] = true;
        order.push(next);
        current = points[next];
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    fn points() -> Vec<(f64, f64)> {
        vec![
            (43.1155, 131.8855), // Владивосток
            (43.1155, 131.95),   // рядом с ним
            (43.12, 131.90),     // тоже рядом
            (44.5500, 135.5667), // Дальнегорск, далеко
        ]
    }

    #[test]
    fn dijkstra_finds_the_shortest_path() {
        let graph = RouteGraph::new(&points());
        let path = graph.path(0, 3);
        assert_eq!(path.first(), Some(&0));
        assert_eq!(path.last(), Some(&3));
        assert_eq!(path.len(), 2, "a complete graph reaches any node directly");
        let (distances, _) = graph.dijkstra(0);
        assert!(distances[3] > distances[1], "Дальнегорск is the farthest");
        assert_eq!(distances[0], 0.0);
    }

    #[test]
    fn order_keeps_nearby_stops_together() {
        let points = points();
        let (order, length) = best_route_from(&points, (43.1155, 131.8855));
        // The two nearby stops must come before the long jump to Дальнегорск.
        assert_eq!(order.last(), Some(&3));
        let naive: Vec<usize> = (0..points.len()).collect();
        let naive_length = RouteGraph::new(&points).length_km(&naive);
        assert!(length <= naive_length, "{length} vs {naive_length}");
    }

    #[test]
    fn two_opt_shortens_a_zigzag() {
        // Order 0 → 1 → 2 → 3 zig-zags between two clusters.
        let points = vec![
            (43.0, 131.0),
            (45.0, 135.0),
            (43.01, 131.01),
            (45.01, 135.01),
        ];
        let graph = RouteGraph::new(&points);
        let mut order = vec![0, 1, 2, 3];
        let before = graph.length_km(&order);
        graph.two_opt(&mut order);
        let after = graph.length_km(&order);
        assert!(
            after < before * 0.6,
            "2-opt must fold the zig-zag: {before} -> {after}"
        );
    }

    #[test]
    fn priority_route_keeps_ranks_and_walks_a_route() {
        let points = vec![
            (43.00, 131.00), // rank 1, cluster A
            (45.00, 135.00), // rank 1, cluster B
            (43.01, 131.01), // rank 0, cluster A
            (45.01, 135.01), // rank 0, cluster B
        ];
        let ranks = vec![1, 1, 0, 0];
        let order = priority_route(&points, &ranks, (43.0, 131.0));
        assert_eq!(order.len(), 4);
        // Rank 0 (what the user asked for) comes first, nearest first from the
        // start point.
        assert_eq!(&order[..2], &[2, 3]);
        // The rest continues from where the route stands: cluster B is closer
        // to the last rank-0 stop than cluster A is.
        assert_eq!(&order[2..], &[1, 0]);
    }

    #[test]
    fn empty_and_single_point_are_safe() {
        assert_eq!(best_route_from(&[], (43.0, 131.0)), (Vec::new(), 0.0));
        let single = best_route_from(&[(43.0, 131.0)], (43.0, 131.0));
        assert_eq!(single.0, vec![0]);
        assert_eq!(single.1, 0.0);
    }
}
