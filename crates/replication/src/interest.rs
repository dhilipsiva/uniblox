//! Spatial interest management (AOI) — the per-peer visibility layer.
//!
//! A peer replicates only the entities within its **area of interest**: a
//! circle (center + radius) in world space. This is BOTH a bandwidth layer
//! (out-of-range entities aren't sent) and the Mode-3 **read-cheat defense**
//! (a modified client can't read entities it structurally never receives).
//!
//! The [`SpatialGrid`] is a uniform spatial hash rebuilt every tick from the
//! sender's owned+alive entities (remote proxies never enter it). An AOI query
//! touches only the cells its bounding box overlaps, then exact-distance
//! filters — so cost scales with the AOI area, not the world.

use std::collections::HashMap;

use bevy_ecs::entity::Entity;

/// Default grid cell size in world units. Independent of any peer's AOI radius
/// (which is per-peer) — a query iterates `ceil(radius / cell)` cells per axis.
pub(crate) const DEFAULT_CELL: f32 = 16.0;

/// A peer's area of interest: a circle in world space with a HYSTERESIS band
/// (ADR-0023 b). An entity ENTERS the AOI at `dist ≤ radius_inner` and EXITS
/// only at `dist > radius_outer`; in the band (`radius_inner < dist ≤
/// radius_outer`) a known entity stays and an unknown one is withheld — so an
/// entity oscillating across the boundary doesn't churn Spawn/Despawn. A
/// single-radius AOI (`set_aoi`) sets `radius_inner == radius_outer` (the
/// degenerate band = the pre-hysteresis single boundary). Absent (no AOI set for
/// a peer) means "unbounded" — that peer sees every owned entity. Invariant:
/// `radius_inner ≤ radius_outer`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Aoi {
    pub center: (f32, f32),
    pub radius_inner: f32,
    pub radius_outer: f32,
}

/// Integer grid cell coordinate.
type Cell = (i32, i32);
/// One cell's occupants: `(entity, world position)`.
type Bucket = Vec<(Entity, (f32, f32))>;

/// A uniform spatial-hash grid over owned entities, rebuilt each tick. Buckets
/// `(entity, position)` by cell so a circle query scans only the cells its
/// bounding box overlaps, then keeps entities within the exact radius.
pub(crate) struct SpatialGrid {
    cell: f32,
    cells: HashMap<Cell, Bucket>,
}

impl SpatialGrid {
    pub(crate) fn new(cell: f32) -> Self {
        SpatialGrid {
            cell,
            cells: HashMap::new(),
        }
    }

    /// The cell a world point falls in. **`.floor()`, not `as i32`** — an `as
    /// i32` cast truncates toward zero, which mis-cells negative coordinates
    /// (e.g. -1.0 would land in cell 0 instead of -1).
    fn cell_of(&self, x: f32, y: f32) -> (i32, i32) {
        (
            (x / self.cell).floor() as i32,
            (y / self.cell).floor() as i32,
        )
    }

    pub(crate) fn insert(&mut self, entity: Entity, pos: (f32, f32)) {
        let key = self.cell_of(pos.0, pos.1);
        self.cells.entry(key).or_default().push((entity, pos));
    }

    /// Entities within `radius` of `center` (Euclidean, boundary INCLUSIVE:
    /// `dist² <= radius²`). Scans the cell bounding box of the circle, then
    /// exact-distance filters — a corner entity in an overlapping cell but
    /// outside the circle is excluded.
    ///
    /// Cost is `O((2·radius/cell)²)` cell lookups — it scans EVERY cell in the
    /// bbox, empty or not, so a pathological radius is expensive regardless of
    /// entity count (there is no clamp; AOI is server-controlled today). If AOI
    /// radii ever grow unbounded, iterate occupied buckets instead (auditor NIT).
    pub(crate) fn in_radius(&self, center: (f32, f32), radius: f32) -> Vec<Entity> {
        let (cx, cy) = center;
        let r2 = radius * radius;
        let min = self.cell_of(cx - radius, cy - radius);
        let max = self.cell_of(cx + radius, cy + radius);
        let mut out = Vec::new();
        for gx in min.0..=max.0 {
            for gy in min.1..=max.1 {
                if let Some(bucket) = self.cells.get(&(gx, gy)) {
                    for &(e, (px, py)) in bucket {
                        let (dx, dy) = (px - cx, py - cy);
                        if dx * dx + dy * dy <= r2 {
                            out.push(e);
                        }
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::world::World;
    use std::collections::HashSet;

    /// Spawn `n` empty entities in a throwaway world for distinct handles.
    fn entities(n: usize) -> (World, Vec<Entity>) {
        let mut world = World::new();
        let es = (0..n).map(|_| world.spawn_empty().id()).collect();
        (world, es)
    }

    fn set(v: Vec<Entity>) -> HashSet<Entity> {
        v.into_iter().collect()
    }

    /// G1 — boundary is inclusive (`<=`): an entity at exactly `radius` is in;
    /// just beyond is out. (3,4) is distance 5 from the origin.
    #[test]
    fn in_radius_boundary_exact() {
        let (_w, e) = entities(1);
        let mut g = SpatialGrid::new(DEFAULT_CELL);
        g.insert(e[0], (3.0, 4.0));
        assert_eq!(g.in_radius((0.0, 0.0), 5.0), vec![e[0]], "at radius => in");
        assert!(
            g.in_radius((0.0, 0.0), 4.99).is_empty(),
            "just beyond radius => out"
        );
    }

    /// G2 — an entity in a bbox-overlapping cell but outside the circle is
    /// excluded (proves the exact dist² filter runs, not just the cell scan).
    #[test]
    fn in_radius_excludes_corner_outside_circle() {
        let (_w, e) = entities(1);
        let mut g = SpatialGrid::new(DEFAULT_CELL);
        // (9,9): dist ≈ 12.73 > 10, but its cell (0,0) IS in the query bbox.
        g.insert(e[0], (9.0, 9.0));
        assert!(
            g.in_radius((0.0, 0.0), 10.0).is_empty(),
            "corner in-cell but outside-circle must be filtered out"
        );
    }

    /// G3 — negative coordinates cell by FLOOR, not truncation. Entity at
    /// (-1,-1) queried with a tiny radius around itself: floor puts it in cell
    /// (-1,-1) (scanned); truncation would put it in (0,0) (missed).
    #[test]
    fn in_radius_negative_coordinates_floor() {
        let (_w, e) = entities(1);
        let mut g = SpatialGrid::new(DEFAULT_CELL);
        g.insert(e[0], (-1.0, -1.0));
        assert_eq!(
            g.in_radius((-1.0, -1.0), 0.5),
            vec![e[0]],
            "floor-celling finds a negative-coord entity a truncate-cell would miss"
        );
    }

    /// G4 — a radius larger than the cell size spans multiple cells; every
    /// in-circle entity is found and none beyond the radius.
    #[test]
    fn in_radius_spans_multiple_cells() {
        let (_w, e) = entities(4);
        let mut g = SpatialGrid::new(DEFAULT_CELL); // cell 16
        g.insert(e[0], (0.0, 0.0));
        g.insert(e[1], (20.0, 0.0));
        g.insert(e[2], (35.0, 0.0));
        g.insert(e[3], (50.0, 0.0)); // beyond r=40
        assert_eq!(
            set(g.in_radius((0.0, 0.0), 40.0)),
            set(vec![e[0], e[1], e[2]]),
            "radius > cell finds all in-circle, excludes the far one"
        );
    }

    /// G5 — an empty grid returns nothing; radius 0 matches only exact-position
    /// entities.
    #[test]
    fn in_radius_empty_and_zero_radius() {
        let empty = SpatialGrid::new(DEFAULT_CELL);
        assert!(empty.in_radius((0.0, 0.0), 100.0).is_empty(), "empty grid");

        let (_w, e) = entities(2);
        let mut g = SpatialGrid::new(DEFAULT_CELL);
        g.insert(e[0], (0.0, 0.0));
        g.insert(e[1], (0.001, 0.0));
        assert_eq!(
            g.in_radius((0.0, 0.0), 0.0),
            vec![e[0]],
            "radius 0 matches only the exact-position entity"
        );
    }
}
