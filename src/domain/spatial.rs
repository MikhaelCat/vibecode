//! Nearest-neighbour search over geographic points.
//!
//! Every provider returns candidates around a place and the tools then need the
//! closest few of them: "ближайшее кафе", "поблизости", or simply the nearest
//! stops of a plan. A static k-d tree answers that in O(log n) instead of
//! re-sorting the whole candidate list, and it is reused by the mock catalogue
//! and the gazetteer.

/// Points closer than this are treated as one place by the tree lookups.
pub const KM_PER_DEGREE: f64 = 111.19;

/// Squared distance in "kilometre-ish" units. Only comparisons matter, so the
/// longitude is scaled by the latitude to keep the metric consistent.
fn squared_distance(lat_a: f64, lon_a: f64, lat_b: f64, lon_b: f64) -> f64 {
    let scale = lat_a.to_radians().cos().abs().max(0.2);
    let dy = (lat_a - lat_b) * KM_PER_DEGREE;
    let dx = (lon_a - lon_b) * KM_PER_DEGREE * scale;
    dy * dy + dx * dx
}

pub fn distance_km(lat_a: f64, lon_a: f64, lat_b: f64, lon_b: f64) -> f64 {
    squared_distance(lat_a, lon_a, lat_b, lon_b).sqrt()
}

#[derive(Debug, Clone, Copy)]
struct Node {
    point: (f64, f64),
    item: usize,
    /// Split axis: 0 = latitude, 1 = longitude.
    axis: u8,
    left: i32,
    right: i32,
}

/// Static k-d tree over a fixed set of points (places, settlements, venues).
#[derive(Debug, Default)]
pub struct KdTree {
    nodes: Vec<Node>,
    root: i32,
}

impl KdTree {
    /// Builds the tree from `(lat, lon)` points. `O(n log n)`.
    pub fn new(points: &[(f64, f64)]) -> Self {
        let mut order: Vec<usize> = (0..points.len()).collect();
        let mut tree = Self {
            nodes: Vec::with_capacity(points.len()),
            root: -1,
        };
        tree.root = tree.build(&mut order, points, 0);
        tree
    }

    fn build(&mut self, order: &mut [usize], points: &[(f64, f64)], depth: usize) -> i32 {
        if order.is_empty() {
            return -1;
        }
        let axis = (depth % 2) as u8;
        let key = |index: usize| {
            let point = points[index];
            if axis == 0 {
                point.0
            } else {
                point.1
            }
        };
        let middle = order.len() / 2;
        order.select_nth_unstable_by(middle, |left, right| {
            key(*left)
                .partial_cmp(&key(*right))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let item = order[middle];
        let node_index = self.nodes.len() as i32;
        self.nodes.push(Node {
            point: points[item],
            item,
            axis,
            left: -1,
            right: -1,
        });
        let (left_slice, rest) = order.split_at_mut(middle);
        let right_slice = &mut rest[1..];
        let left = self.build(left_slice, points, depth + 1);
        let right = self.build(right_slice, points, depth + 1);
        self.nodes[node_index as usize].left = left;
        self.nodes[node_index as usize].right = right;
        node_index
    }

    pub fn is_empty(&self) -> bool {
        self.root < 0
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// The `k` nearest points to `(lat, lon)`, closest first, with the distance
    /// in kilometres. Points are returned as their indices in the input slice.
    pub fn nearest(&self, lat: f64, lon: f64, k: usize) -> Vec<(usize, f64)> {
        if self.root < 0 || k == 0 {
            return Vec::new();
        }
        let mut best: Vec<(usize, f64)> = Vec::with_capacity(k + 1);
        self.search(self.root, lat, lon, k, &mut best);
        best.sort_by(|left, right| {
            left.1
                .partial_cmp(&right.1)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        best.truncate(k);
        // The search compares squared distances; callers get real kilometres,
        // exactly like `within` does.
        for (_, distance) in best.iter_mut() {
            *distance = distance.sqrt();
        }
        best
    }

    /// Every point within `radius_km`, closest first.
    pub fn within(&self, lat: f64, lon: f64, radius_km: f64) -> Vec<(usize, f64)> {
        let mut found: Vec<(usize, f64)> = Vec::new();
        if self.root < 0 {
            return found;
        }
        let mut stack = vec![(self.root, 0usize)];
        while let Some((node_index, depth)) = stack.pop() {
            let node = self.nodes[node_index as usize];
            let axis = (depth % 2) as u8;
            let distance = distance_km(lat, lon, node.point.0, node.point.1);
            if distance <= radius_km {
                found.push((node.item, distance));
            }
            let key = if axis == 0 {
                node.point.0
            } else {
                node.point.1
            };
            let query = if axis == 0 { lat } else { lon };
            let (near, far) = if query < key {
                (node.left, node.right)
            } else {
                (node.right, node.left)
            };
            if near >= 0 {
                stack.push((near, depth + 1));
            }
            // The far side only matters while the radius still reaches the split.
            if far >= 0 && (query - key).abs() * KM_PER_DEGREE <= radius_km {
                stack.push((far, depth + 1));
            }
        }
        found.sort_by(|left, right| {
            left.1
                .partial_cmp(&right.1)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        found
    }

    /// Returns `true` when any point lies within `radius_km`.
    pub fn any_within(&self, lat: f64, lon: f64, radius_km: f64) -> bool {
        if self.root < 0 {
            return false;
        }
        let mut stack = vec![(self.root, 0usize)];
        while let Some((node_index, depth)) = stack.pop() {
            let node = self.nodes[node_index as usize];
            let axis = (depth % 2) as u8;
            if distance_km(lat, lon, node.point.0, node.point.1) <= radius_km {
                return true;
            }
            let key = if axis == 0 {
                node.point.0
            } else {
                node.point.1
            };
            let query = if axis == 0 { lat } else { lon };
            let (near, far) = if query < key {
                (node.left, node.right)
            } else {
                (node.right, node.left)
            };
            if near >= 0 {
                stack.push((near, depth + 1));
            }
            if far >= 0 && (query - key).abs() * KM_PER_DEGREE <= radius_km {
                stack.push((far, depth + 1));
            }
        }
        false
    }

    fn search(&self, node_index: i32, lat: f64, lon: f64, k: usize, best: &mut Vec<(usize, f64)>) {
        let node = self.nodes[node_index as usize];
        let distance = squared_distance(lat, lon, node.point.0, node.point.1);
        if best.len() < k {
            best.push((node.item, distance));
        } else if let Some(worst) = best
            .iter()
            .enumerate()
            .max_by(|left, right| {
                left.1
                     .1
                    .partial_cmp(&right.1 .1)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(index, _)| index)
        {
            if distance < best[worst].1 {
                best[worst] = (node.item, distance);
            }
        }
        let axis = node.axis;
        let key = if axis == 0 {
            node.point.0
        } else {
            node.point.1
        };
        let query = if axis == 0 { lat } else { lon };
        let (near, far) = if query < key {
            (node.left, node.right)
        } else {
            (node.right, node.left)
        };
        if near >= 0 {
            self.search(near, lat, lon, k, best);
        }
        let threshold = if best.len() < k {
            f64::INFINITY
        } else {
            best.iter()
                .map(|(_, distance)| *distance)
                .fold(0.0_f64, f64::max)
        };
        // Visit the far side only when the splitting plane is closer than the
        // worst point already found.
        let plane = (query - key) * KM_PER_DEGREE;
        if far >= 0 && plane * plane <= threshold {
            self.search(far, lat, lon, k, best);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<(f64, f64)> {
        vec![
            (43.1155, 131.8855), // Владивосток
            (43.7978, 131.9497), // Уссурийск
            (42.8169, 132.8892), // Находка
            (44.1546, 133.2710), // Арсеньев
            (42.9833, 132.4000), // Фокино
        ]
    }

    #[test]
    fn finds_the_closest_point() {
        let points = sample();
        let tree = KdTree::new(&points);
        let nearest = tree.nearest(42.98, 132.41, 1);
        assert_eq!(nearest.len(), 1);
        assert_eq!(nearest[0].0, 4, "Фокино is the closest to that coordinate");
        assert!(nearest[0].1 < 5.0);
    }

    #[test]
    fn orders_by_distance_and_respects_k() {
        let points = sample();
        let tree = KdTree::new(&points);
        let nearest = tree.nearest(43.0, 132.5, 3);
        assert_eq!(nearest.len(), 3);
        assert!(nearest[0].1 <= nearest[1].1);
        assert!(nearest[1].1 <= nearest[2].1);
        assert_eq!(nearest[0].0, 4);
    }

    #[test]
    fn within_respects_the_radius() {
        let points = sample();
        let tree = KdTree::new(&points);
        let near_fokino = tree.within(42.9833, 132.4, 10.0);
        assert_eq!(near_fokino.len(), 1);
        assert_eq!(near_fokino[0].0, 4);
        let wide = tree.within(43.3, 132.0, 60.0);
        assert!(wide.len() >= 3);
        assert!(wide.windows(2).all(|pair| pair[0].1 <= pair[1].1));
    }

    #[test]
    fn matches_a_linear_scan() {
        let points = sample();
        let tree = KdTree::new(&points);
        let query = (43.5, 132.9);
        let mut linear: Vec<(usize, f64)> = points
            .iter()
            .enumerate()
            .map(|(index, (lat, lon))| (index, distance_km(query.0, query.1, *lat, *lon)))
            .collect();
        linear.sort_by(|left, right| {
            left.1
                .partial_cmp(&right.1)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let nearest = tree.nearest(query.0, query.1, 3);
        assert_eq!(
            nearest.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
            linear
                .iter()
                .take(3)
                .map(|(index, _)| *index)
                .collect::<Vec<_>>()
        );
        // `nearest` returns kilometres, not the squared values used internally.
        for (found, expected) in nearest.iter().zip(linear.iter().take(3)) {
            assert!(
                (found.1 - expected.1).abs() < 0.01,
                "distance mismatch: {} vs {}",
                found.1,
                expected.1
            );
        }
    }

    #[test]
    fn empty_tree_is_safe() {
        let tree = KdTree::new(&[]);
        assert!(tree.is_empty());
        assert!(tree.nearest(43.0, 132.0, 5).is_empty());
        assert!(tree.within(43.0, 132.0, 50.0).is_empty());
        assert!(!tree.any_within(43.0, 132.0, 50.0));
    }
}
