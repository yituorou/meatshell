//! Runtime state of the docked-panel edge stacks (#dock-stack).
//!
//! Window-level panels (sidebar / welcome / quick) can share one window
//! edge: `DockStacks` keeps, per edge, the ordered list of simultaneously-
//! expanded panels and how the edge's secondary axis is divided between them.
//! Pure logic — no Slint — so it is unit-testable; `app.rs` pushes the stacks
//! into the UI and persists them via `crate::config`.

use crate::config::{DockEdgeSer, DockSlotSer};

/// One stack slot: which panel and how much of the edge it owns (0..1).
/// A non-positive ratio is a sentinel for "just joined" — [`rebalance`]
/// hands such members an even share and squeezes the established ones
/// around them.
#[derive(Clone, Debug, PartialEq)]
pub struct DockSlotInfo {
    pub kind: &'static str,
    pub ratio: f32,
}

/// Smallest share a panel keeps along the stacking axis (ratio, not px).
const MIN_RATIO: f32 = 0.08;

/// Visible thickness of a divider between two stacked panels, in px.
pub const DIVIDER: f32 = 4.0;

/// Clamp bounds for a stacked panel's thickness along its edge's normal
/// (its width on a left/right edge, height on a top/bottom one).
pub const MIN_THICK: f32 = 120.0;
/// Max share of the dock-area an edge stack may take. Kept well under half so
/// the terminal never fully disappears even with opposite edges both maxed.
const MAX_THICK_FRAC: f32 = 0.38;
/// Outer band an edge reserves for the collapsed-panel ToolStrip (a folded
/// panel still shows its 36px icon strip; stacked panels live inside it).
pub const STRIP: f32 = 36.0;

/// Absolute rectangle in dock-area logical px.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RectGeom {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// One rendered, expanded dock panel placed by [`DockStacks::compute_geom`].
#[derive(Clone, Debug, PartialEq)]
pub struct PanelGeom {
    pub kind: &'static str,
    /// The dock edge this panel sits on ("left|right|top|bottom") — used by
    /// the UI to paint the in-edge resize handle on the correct side.
    pub edge: &'static str,
    pub rect: RectGeom,
}

/// A draggable divider between two stacked panels; dragging updates the
/// boundary via [`DockStacks::set_ratio`] with `index` = the slot before it.
#[derive(Clone, Debug, PartialEq)]
pub struct DividerGeom {
    pub edge: &'static str,
    pub index: usize,
    pub rect: RectGeom,
    /// True when the handle is vertical (left/right edge → drag up/down).
    pub vertical: bool,
}

/// Full layout output: the central content rect (after every edge stack is
/// carved off) plus absolute geoms for every expanded panel and divider.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DockGeom {
    pub central: RectGeom,
    pub panels: Vec<PanelGeom>,
    pub dividers: Vec<DividerGeom>,
}

/// Known panel kinds, in the order they appear in the config / UI.
pub const KINDS: [&str; 3] = ["sidebar", "welcome", "quick"];

fn edges() -> impl Iterator<Item = &'static str> {
    ["left", "right", "top", "bottom"].into_iter()
}

#[derive(Clone, Debug, Default)]
pub struct DockStacks {
    pub left: Vec<DockSlotInfo>,
    pub right: Vec<DockSlotInfo>,
    pub top: Vec<DockSlotInfo>,
    pub bottom: Vec<DockSlotInfo>,
}

impl DockStacks {
    pub fn table_mut(&mut self, edge: &str) -> Option<&mut Vec<DockSlotInfo>> {
        match edge {
            "left" => Some(&mut self.left),
            "right" => Some(&mut self.right),
            "top" => Some(&mut self.top),
            "bottom" => Some(&mut self.bottom),
            _ => None,
        }
    }

    fn table(&self, edge: &str) -> Option<&[DockSlotInfo]> {
        match edge {
            "left" => Some(&self.left),
            "right" => Some(&self.right),
            "top" => Some(&self.top),
            "bottom" => Some(&self.bottom),
            _ => None,
        }
    }

    /// The edge a panel currently stacks on (None when it is not in any stack).
    #[allow(dead_code)] // dock rearrange API; only exercised by tests for now
    pub fn edge_of(&self, kind: &str) -> Option<&'static str> {
        for (e, table) in [
            ("left", self.table("left")),
            ("right", self.table("right")),
            ("top", self.table("top")),
            ("bottom", self.table("bottom")),
        ] {
            if table.is_some_and(|t| t.iter().any(|s| s.kind == kind)) {
                return Some(e);
            }
        }
        None
    }

    /// Move (or add) `kind` onto `edge`, taking it off whatever edge it was on.
    /// Ratios are rebalanced evenly for the edge's new member count.
    #[allow(dead_code)] // dock rearrange API; only exercised by tests for now
    pub fn dock_to(&mut self, edge: &str, kind: &'static str) {
        if let Some(old_edge) = self.edge_of(kind) {
            if old_edge == edge {
                return;
            }
            if let Some(t) = self.table_mut(old_edge) {
                t.retain(|s| s.kind != kind);
            }
        }
        let Some(t) = self.table_mut(edge) else { return };
        if t.iter().any(|s| s.kind == kind) {
            return;
        }
        t.push(DockSlotInfo { kind, ratio: 0.0 });
        rebalance(t);
    }

    /// Remove `kind` from `edge` (a fold / close).
    #[allow(dead_code)] // dock rearrange API; only exercised by tests for now
    pub fn remove(&mut self, edge: &str, kind: &str) {
        if let Some(t) = self.table_mut(edge) {
            t.retain(|s| s.kind != kind);
        }
    }

    /// Set the split boundary after slot `index` so slot `index` holds
    /// approximately `ratio` of the edge, sliding the two groups around it.
    pub fn set_ratio(&mut self, edge: &str, index: usize, ratio: f32) {
        if let Some(t) = self.table_mut(edge) {
            if index + 1 >= t.len() {
                return;
            }
            let r = ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO);
            let group_a: f32 = t.iter().take(index + 1).map(|s| s.ratio).sum();
            let group_b: f32 = t.iter().skip(index + 1).map(|s| s.ratio).sum();
            let total = group_a + group_b;
            if total <= 0.0 {
                return;
            }
            let (a, b) = (r, 1.0 - r);
            let len = t.len();
            for (i, s) in t.iter_mut().enumerate() {
                s.ratio = if i <= index {
                    a * if group_a > 0.0 { s.ratio / group_a } else { 1.0 / (index + 1) as f32 }
                } else {
                    b * if group_b > 0.0 { s.ratio / group_b } else { 1.0 / (len - index - 1) as f32 }
                };
                s.ratio = s.ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO);
            }
            rebalance(t);
        }
    }

    /// Serialize the current stacks for config persistence (2+ slots only).
    pub fn to_saved(&self) -> Vec<DockEdgeSer> {
        edges()
            .filter_map(|edge| {
                let slots = self.table(edge)?;
                if slots.len() < 2 {
                    return None;
                }
                Some(DockEdgeSer {
                    edge: edge.to_string(),
                    slots: slots
                        .iter()
                        .map(|s| DockSlotSer {
                            kind: s.kind.to_string(),
                            ratio: s.ratio,
                        })
                        .collect(),
                })
            })
            .collect()
    }

    /// Replace the state from persisted config (already sanitised by the
    /// config layer).
    pub fn from_saved<'a>(&mut self, stacks: impl IntoIterator<Item = &'a DockEdgeSer>) {
        self.left.clear();
        self.right.clear();
        self.top.clear();
        self.bottom.clear();
        for e in stacks {
            let Some(t) = self.table_mut(&e.edge) else { continue };
            for s in &e.slots {
                let Some(kind) = KINDS.iter().copied().find(|k| *k == s.kind) else {
                    continue;
                };
                t.push(DockSlotInfo { kind, ratio: s.ratio });
            }
            rebalance(t);
        }
    }

    /// Rebuild the stacks from the current *expanded* panel set, so the saved
    /// (kind, edge, ratio) triples survive while panels folded/closed drop out
    /// and newly-expanded ones join their edge with an even share. `expanded`
    /// returns the edge a panel is currently docked to (`None` = folded/off).
    /// Reads order from the previous state first, then new panels afterwards —
    /// stable so an untouched layout keeps its exact split.
    pub fn rebuild_from(
        &mut self,
        saved: &DockStacks,
        expanded: &dyn Fn(&str) -> Option<&'static str>,
    ) {
        for edge in ["left", "right", "top", "bottom"] {
            let mut order: Vec<&'static str> = Vec::new();
            if let Some(slots) = saved.table(edge) {
                // Keep the persisted order and ratio for panels still on this
                // edge and still expanded.
                let mut kept: Vec<&'static str> =
                    slots.iter().map(|s| s.kind).collect();
                kept.retain(|k| expanded(k) == Some(edge));
                order.append(&mut kept);
            }
            // Any newly-expanded panel on this edge joins behind the kept ones.
            for k in KINDS {
                if expanded(k) == Some(edge) && !order.contains(&k) {
                    order.push(k);
                }
            }
            if order.is_empty() {
                if let Some(t) = self.table_mut(edge) {
                    t.clear();
                }
                continue;
            }
            // Inherit the saved ratio for panels we already knew; a new
            // panel takes the leftover of the edge, or an even share of the
            // whole edge when nothing is left over.
            let mut slots: Vec<DockSlotInfo> = Vec::with_capacity(order.len());
            let mut fresh: Vec<&'static str> = Vec::new();
            let mut known: f32 = 0.0;
            for k in &order {
                let ratio = saved
                    .table(edge)
                    .and_then(|sl| sl.iter().find(|s| s.kind == *k))
                    .map(|s| s.ratio);
                match ratio {
                    Some(r) => {
                        known += r;
                        slots.push(DockSlotInfo { kind: k, ratio: r });
                    }
                    None => fresh.push(k),
                }
            }
            if !fresh.is_empty() {
                // A comfortable leftover keeps the established split intact
                // (a user-tuned ratio never gets wiped by an unrelated
                // stack). But a leftover at or below MIN_RATIO means the
                // edge is already spoken for — handing newcomers the crumbs
                // would strand them, so push the sentinel instead and let
                // rebalance squeeze everyone to even shares.
                let leftover = 1.0 - known;
                let each = if leftover > MIN_RATIO {
                    leftover / fresh.len() as f32
                } else {
                    0.0
                };
                for k in fresh {
                    slots.push(DockSlotInfo { kind: k, ratio: each });
                }
            }
            if let Some(target) = self.table_mut(edge) {
                *target = slots;
                rebalance(target);
            }
        }
    }

    /// Compute absolute rectangles for every expanded panel (and its
    /// dividers) plus the central content rect, for the given dock-area size
    /// in logical px. `extent` returns a panel's preferred thickness along its
    /// edge's normal (width on a left/right edge, height on a top/bottom one).
    /// `has_strip` marks edges whose collapsed panels still show the outer
    /// 36px ToolStrip band: the band stays at the very edge and the whole
    /// stack is carved INSIDE it.
    ///
    /// Stacks never overlap (#582). Every edge's band is reserved first (the
    /// ToolStrips span their whole edge), then the left and right stacks run
    /// the full height between the top and bottom bands, and the top and
    /// bottom stacks only span the width left between the side stacks. Each
    /// stack's ratios divide that span, not the whole dock-area edge.
    pub fn compute_geom(
        &self,
        extent: &dyn Fn(&str) -> f32,
        has_strip: &dyn Fn(&str) -> bool,
        w: f32,
        h: f32,
    ) -> DockGeom {
        let (cw, ch) = (w.max(1.0), h.max(1.0));
        let band = |edge: &str| if has_strip(edge) { STRIP } else { 0.0 };
        let (band_l, band_r, band_t, band_b) = (band("left"), band("right"), band("top"), band("bottom"));
        let slots_of = |edge: &str| self.table(edge).filter(|t| !t.is_empty());
        // All stacked panels on an edge share its thickness; clamp so the
        // central area keeps most of the dock-area. Tiny dock-areas (the
        // pre-show 0×0 pass, or a user-shrunk window) put the cap below
        // MIN_THICK — f32::clamp panics when min > max, so floor the cap at
        // MIN_THICK; the next real resize pass overwrites the transient
        // geometry.
        let thickness = |edge: &str, own_band: f32| -> f32 {
            let Some(slots) = slots_of(edge) else {
                return 0.0;
            };
            let full = if matches!(edge, "left" | "right") { cw } else { ch };
            let cap = ((full - own_band).max(0.0) * MAX_THICK_FRAC).max(MIN_THICK);
            slots
                .iter()
                .map(|s| extent(s.kind))
                .fold(0.0, f32::max)
                .clamp(MIN_THICK, cap)
        };
        let (thick_l, thick_r) = (thickness("left", band_l), thickness("right", band_r));
        let (thick_t, thick_b) = (thickness("top", band_t), thickness("bottom", band_b));

        // Span along each edge that its stack divides: (start, length).
        let side_span = (band_t, (ch - band_t - band_b).max(0.0));
        let end_span = (band_l + thick_l, (cw - band_l - thick_l - band_r - thick_r).max(0.0));

        let mut g = DockGeom {
            central: RectGeom {
                x: band_l + thick_l,
                y: band_t + thick_t,
                w: (cw - band_l - thick_l - band_r - thick_r).max(0.0),
                h: (ch - band_t - thick_t - band_b - thick_b).max(0.0),
            },
            ..Default::default()
        };
        for edge in ["left", "right", "top", "bottom"] {
            let Some(slots) = slots_of(edge) else {
                continue;
            };
            let horizontal = matches!(edge, "left" | "right");
            let (start, axis) = if horizontal { side_span } else { end_span };
            let mut pos = start;
            for (i, s) in slots.iter().enumerate() {
                let seg = if i == slots.len() - 1 {
                    (start + axis - pos).max(0.0)
                } else {
                    (s.ratio * axis).max(0.0)
                };
                let rect = match edge {
                    "left" => RectGeom { x: band_l, y: pos, w: thick_l, h: seg },
                    "right" => RectGeom { x: cw - band_r - thick_r, y: pos, w: thick_r, h: seg },
                    "top" => RectGeom { x: pos, y: band_t, w: seg, h: thick_t },
                    _ => RectGeom { x: pos, y: ch - band_b - thick_b, w: seg, h: thick_b },
                };
                g.panels.push(PanelGeom { kind: s.kind, edge, rect });
                if i < slots.len() - 1 {
                    let drect = if horizontal {
                        RectGeom { x: rect.x, y: pos + seg, w: rect.w, h: DIVIDER }
                    } else {
                        RectGeom { x: pos + seg, y: rect.y, w: DIVIDER, h: rect.h }
                    };
                    g.dividers.push(DividerGeom {
                        edge,
                        index: i,
                        rect: drect,
                        vertical: horizontal,
                    });
                }
                pos += seg;
            }
        }
        g
    }
}

/// The split ratio for a divider dragged to `pos` (dock-area px along the
/// edge's split axis), given the span `(start, len)` its stack divides.
/// Without a known span (no panel rows yet) the whole `fallback_axis` is
/// used, as before #582.
pub fn split_ratio_at(pos: f32, span: Option<(f32, f32)>, fallback_axis: f32) -> f32 {
    let (start, len) = span.filter(|(_, len)| *len > 0.0).unwrap_or((0.0, fallback_axis));
    if len > 0.0 {
        ((pos - start) / len).clamp(0.02, 0.98)
    } else {
        0.5
    }
}

fn rebalance(t: &mut [DockSlotInfo]) {
    if t.is_empty() {
        return;
    }
    let n = t.len() as f32;
    // Sentinels: slots that just joined the edge (see `DockSlotInfo`). Give
    // each an even 1/n share and squeeze the established members around
    // them — otherwise a merge onto a fully-occupied edge clamps the
    // newcomer to MIN_RATIO and strands it as an 8% sliver.
    let fresh = t.iter().filter(|s| s.ratio <= 0.0).count() as f32;
    if fresh > 0.0 {
        let each = 1.0 / n;
        let known: f32 = t.iter().map(|s| s.ratio.max(0.0)).sum();
        let scale = if known > 0.0 {
            (1.0 - each * fresh).max(0.0) / known
        } else {
            0.0
        };
        for s in t.iter_mut() {
            if s.ratio <= 0.0 {
                s.ratio = each;
            } else {
                s.ratio *= scale;
            }
        }
    }
    let total: f32 = t.iter().map(|s| s.ratio).sum();
    if total <= 0.0 {
        for s in t.iter_mut() {
            s.ratio = 1.0 / n;
        }
        return;
    }
    for s in t.iter_mut() {
        s.ratio = (s.ratio / total).clamp(MIN_RATIO, 1.0 - MIN_RATIO);
    }
    // Absorb the rounding/clamping drift into the last slot: shares are
    // absolute edge fractions (what compute_geom multiplies by the axis),
    // so each keeps its own share and only the tail takes the remainder —
    // a summed 1.0 means an exactly 50/50 two-panel merge.
    let mut rem: f32 = 1.0;
    let n = t.len();
    for (i, s) in t.iter_mut().enumerate() {
        if i == n - 1 {
            // Floor the tail: a clamped-up overflow elsewhere could leave
            // less than MIN here, and a 0.0 ratio persisted by to_saved
            // would read back as a sentinel — or get dropped by the config
            // sanitiser — silently losing the panel on the next load.
            s.ratio = rem.max(MIN_RATIO);
        } else {
            let share = s.ratio.min(rem);
            s.ratio = share;
            rem -= share;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dock_to_moves_between_edges() {
        let mut s = DockStacks::default();
        s.dock_to("left", "sidebar");
        s.dock_to("left", "quick");
        assert_eq!(s.left.len(), 2);
        s.dock_to("right", "sidebar");
        assert_eq!(s.left.len(), 1);
        assert_eq!(s.right.len(), 1);
        assert_eq!(s.right[0].kind, "sidebar");
        assert_eq!(s.edge_of("sidebar"), Some("right"));
    }

    #[test]
    fn dock_to_is_idempotent() {
        let mut s = DockStacks::default();
        s.dock_to("left", "quick");
        s.dock_to("left", "quick");
        assert_eq!(s.left.len(), 1);
    }

    #[test]
    fn remove_folds_panel() {
        let mut s = DockStacks::default();
        s.dock_to("left", "sidebar");
        s.dock_to("left", "quick");
        s.remove("left", "sidebar");
        assert_eq!(s.left.len(), 1);
        assert_eq!(s.left[0].kind, "quick");
        assert_eq!(s.edge_of("sidebar"), None);
    }

    #[test]
    fn ratios_rebalance_evenly_on_add() {
        let mut s = DockStacks::default();
        s.dock_to("left", "sidebar");
        s.dock_to("left", "quick");
        // A merge onto an occupied edge splits it in halves, not slivers.
        assert!((s.left[0].ratio - 0.5).abs() < 1e-5);
        assert!((s.left[1].ratio - 0.5).abs() < 1e-5);
        s.dock_to("left", "welcome");
        assert_eq!(s.left.len(), 3);
        let sum: f32 = s.left.iter().map(|x| x.ratio).sum();
        assert!((sum - 1.0).abs() < 1e-5);
        for x in &s.left {
            assert!((x.ratio - 1.0 / 3.0).abs() < 1e-5);
        }
    }

    #[test]
    fn merge_keeps_established_split_proportional() {
        let mut s = DockStacks::default();
        s.dock_to("left", "sidebar");
        s.dock_to("left", "quick");
        s.set_ratio("left", 0, 0.7);
        s.dock_to("left", "welcome");
        // The 70/30 tuning survives as its original 7:3 ratio inside the
        // share the newcomer did NOT take; the newcomer itself gets an
        // even third.
        assert!((s.left[2].ratio - 1.0 / 3.0).abs() < 1e-3);
        assert!((s.left[0].ratio / s.left[1].ratio - 7.0 / 3.0).abs() < 0.05);
        let sum: f32 = s.left.iter().map(|x| x.ratio).sum();
        assert!((sum - 1.0).abs() < 1e-5);
    }

    #[test]
    fn rebuild_from_full_edge_expands_evenly() {
        // Left holds a lone sidebar (ratio 1.0); quick newly expands there.
        let mut saved = DockStacks::default();
        saved.dock_to("left", "sidebar");
        let expanded = |k: &str| match k {
            "sidebar" | "quick" => Some("left"),
            _ => None,
        };
        let mut cur = DockStacks::default();
        cur.rebuild_from(&saved, &expanded);
        assert_eq!(cur.left.len(), 2);
        assert!((cur.left[0].ratio - 0.5).abs() < 1e-5);
        assert!((cur.left[1].ratio - 0.5).abs() < 1e-5);
    }

    #[test]
    fn rebuild_heals_legacy_sliver_on_reexpand() {
        // A 92/8 pair (the old merge bug's persisted shape) with the sliver
        // folded; re-expanding a panel onto the edge normalises to halves.
        let mut saved = DockStacks::default();
        saved.left.push(DockSlotInfo {
            kind: "sidebar",
            ratio: 0.92,
        });
        let expanded = |k: &str| match k {
            "sidebar" | "welcome" => Some("left"),
            _ => None,
        };
        let mut cur = DockStacks::default();
        cur.rebuild_from(&saved, &expanded);
        assert_eq!(cur.left.len(), 2);
        assert!((cur.left[0].ratio - 0.5).abs() < 1e-5);
        assert!((cur.left[1].ratio - 0.5).abs() < 1e-5);
    }

    #[test]
    fn set_ratio_moves_boundary_and_stays_normalised() {
        let mut s = DockStacks::default();
        s.dock_to("left", "sidebar");
        s.dock_to("left", "quick");
        s.dock_to("left", "welcome");
        s.set_ratio("left", 0, 0.6);
        assert!((s.left[0].ratio - 0.6).abs() < 1e-5);
        let sum: f32 = s.left.iter().map(|x| x.ratio).sum();
        assert!((sum - 1.0).abs() < 1e-5);
        // Out-of-range index is ignored.
        let before = s.left.clone();
        s.set_ratio("left", 99, 0.5);
        assert_eq!(s.left, before);
    }

    #[test]
    fn saved_roundtrip_keeps_stack() {
        let mut s = DockStacks::default();
        s.dock_to("bottom", "sidebar");
        s.dock_to("bottom", "welcome");
        let saved = s.to_saved();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].edge, "bottom");
        let mut t = DockStacks::default();
        t.from_saved(&saved);
        assert_eq!(t.bottom.len(), 2);
        assert_eq!(t.bottom[0].kind, "sidebar");
        assert_eq!(t.bottom[1].kind, "welcome");
        assert!((t.bottom.iter().map(|x| x.ratio).sum::<f32>() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn single_panel_stack_is_not_persisted() {
        let mut s = DockStacks::default();
        s.dock_to("left", "sidebar");
        assert!(s.to_saved().is_empty());
    }

    #[test]
    fn from_saved_ignores_unknown_kind_and_edge() {
        let saved = vec![DockEdgeSer {
            edge: "left".into(),
            slots: vec![
                DockSlotSer { kind: "bogus".into(), ratio: 0.5 },
                DockSlotSer { kind: "quick".into(), ratio: 0.5 },
            ],
        }];
        let mut t = DockStacks::default();
        t.from_saved(&saved);
        assert_eq!(t.left.len(), 1);
        assert_eq!(t.left[0].kind, "quick");
    }

    fn extent_220(_: &str) -> f32 {
        220.0
    }

    fn no_strip(_: &str) -> bool {
        false
    }

    #[test]
    fn geom_splits_two_panels_vertically_on_left_edge() {
        let mut s = DockStacks::default();
        s.dock_to("left", "sidebar");
        s.dock_to("left", "quick");
        let g = s.compute_geom(&extent_220, &no_strip, 800.0, 600.0);
        // Both panels share the left-edge thickness (220) and split the height.
        assert_eq!(g.panels.len(), 2);
        assert_eq!(g.panels[0].kind, "sidebar");
        assert_eq!(g.panels[0].rect, RectGeom { x: 0.0, y: 0.0, w: 220.0, h: 300.0 });
        assert_eq!(g.panels[1].kind, "quick");
        assert_eq!(g.panels[1].rect, RectGeom { x: 0.0, y: 300.0, w: 220.0, h: 300.0 });
        assert_eq!(g.dividers.len(), 1);
        assert!(g.dividers[0].vertical);
        assert_eq!(g.dividers[0].rect, RectGeom { x: 0.0, y: 300.0, w: 220.0, h: DIVIDER });
        assert_eq!(g.central, RectGeom { x: 220.0, y: 0.0, w: 580.0, h: 600.0 });
    }

    #[test]
    fn geom_single_panel_carves_edge_full_height() {
        let mut s = DockStacks::default();
        s.dock_to("right", "welcome");
        let g = s.compute_geom(&extent_220, &no_strip, 800.0, 600.0);
        assert_eq!(g.panels.len(), 1);
        assert_eq!(g.panels[0].rect, RectGeom { x: 580.0, y: 0.0, w: 220.0, h: 600.0 });
        assert!(g.dividers.is_empty());
        assert_eq!(g.central, RectGeom { x: 0.0, y: 0.0, w: 580.0, h: 600.0 });
    }

    #[test]
    fn geom_top_edge_splits_horizontally() {
        let mut s = DockStacks::default();
        s.dock_to("top", "sidebar");
        s.dock_to("top", "welcome");
        let g = s.compute_geom(&extent_220, &no_strip, 800.0, 600.0);
        assert_eq!(g.panels.len(), 2);
        assert_eq!(g.panels[0].rect, RectGeom { x: 0.0, y: 0.0, w: 400.0, h: 220.0 });
        assert_eq!(g.panels[1].rect, RectGeom { x: 400.0, y: 0.0, w: 400.0, h: 220.0 });
        assert!(!g.dividers[0].vertical);
        assert_eq!(g.dividers[0].rect, RectGeom { x: 400.0, y: 0.0, w: DIVIDER, h: 220.0 });
        assert_eq!(g.central, RectGeom { x: 0.0, y: 220.0, w: 800.0, h: 380.0 });
    }

    #[test]
    fn geom_no_panels_leaves_central_untouched() {
        let s = DockStacks::default();
        let g = s.compute_geom(&extent_220, &no_strip, 800.0, 600.0);
        assert!(g.panels.is_empty());
        assert!(g.dividers.is_empty());
        assert_eq!(g.central, RectGeom { x: 0.0, y: 0.0, w: 800.0, h: 600.0 });
    }

    #[test]
    fn geom_clamps_thickness_so_central_survives() {
        let mut s = DockStacks::default();
        s.dock_to("left", "sidebar");
        let g = s.compute_geom(&|_| 900.0, &no_strip, 800.0, 600.0);
        // cap = 800 → max thickness 304 (0.38 × 800); central keeps ≥ 62%.
        assert_eq!(g.panels[0].rect.w, 304.0);
        assert_eq!(g.central.w, 496.0);
    }

    #[test]
    fn geom_survives_tiny_dock_area() {
        let mut s = DockStacks::default();
        s.dock_to("left", "sidebar");
        // The pre-show 0×0 pass: before the cap floor, clamp(120, 0.38)
        // panicked with "min > max" and killed the app at startup.
        let g = s.compute_geom(&extent_220, &no_strip, 0.0, 0.0);
        assert_eq!(g.panels[0].rect.w, 120.0);
        assert_eq!(g.central.w, 0.0);
        assert_eq!(g.central.h, 1.0);
    }

    #[test]
    fn geom_reserves_outer_strip_band() {
        let mut s = DockStacks::default();
        s.dock_to("left", "sidebar");
        s.dock_to("left", "quick");
        // Left edge stacks behind its band; the right edge is strip-only
        // (folded panels, nothing expanded) yet still takes its 36px.
        let strip = |e: &str| e == "left" || e == "right";
        let g = s.compute_geom(&extent_220, &strip, 800.0, 600.0);
        assert_eq!(g.panels.len(), 2);
        assert_eq!(g.panels[0].rect.x, 36.0);
        // The band runs ALONG the edge: split axis and ratios untouched.
        assert_eq!(g.panels[0].rect.h, 300.0);
        assert_eq!(g.panels[1].rect.y, 300.0);
        assert_eq!(g.central.x, 256.0);
        assert_eq!(g.central.w, 800.0 - 256.0 - 36.0);
    }

    #[test]
    fn geom_bottom_stack_sits_between_side_stacks() {
        // #582: a sidebar on the left plus quick commands on the bottom used
        // to overlap in the bottom-left corner (both spanned their full edge).
        let mut s = DockStacks::default();
        s.dock_to("left", "sidebar");
        s.dock_to("left", "welcome");
        s.dock_to("bottom", "quick");
        let g = s.compute_geom(&extent_220, &no_strip, 1000.0, 800.0);
        let rect = |k: &str| g.panels.iter().find(|p| p.kind == k).unwrap().rect;
        // The left stack keeps the full height ...
        assert_eq!(rect("sidebar"), RectGeom { x: 0.0, y: 0.0, w: 220.0, h: 400.0 });
        assert_eq!(rect("welcome"), RectGeom { x: 0.0, y: 400.0, w: 220.0, h: 400.0 });
        // ... and the bottom panel starts where it ends.
        assert_eq!(rect("quick"), RectGeom { x: 220.0, y: 580.0, w: 780.0, h: 220.0 });
        assert_eq!(g.central, RectGeom { x: 220.0, y: 0.0, w: 780.0, h: 580.0 });
        assert_no_overlap(&g);
    }

    #[test]
    fn geom_side_stack_stays_inside_end_strips() {
        // #582: with quick commands folded to a bottom strip, the left stack
        // ran down over that strip.
        let mut s = DockStacks::default();
        s.dock_to("left", "welcome");
        let strip = |e: &str| e == "bottom";
        let g = s.compute_geom(&extent_220, &strip, 1000.0, 800.0);
        assert_eq!(g.panels[0].rect, RectGeom { x: 0.0, y: 0.0, w: 220.0, h: 764.0 });
        assert_eq!(g.central, RectGeom { x: 220.0, y: 0.0, w: 780.0, h: 764.0 });
    }

    #[test]
    fn geom_never_overlaps_with_every_edge_busy() {
        let mut s = DockStacks::default();
        s.dock_to("left", "sidebar");
        s.dock_to("right", "welcome");
        s.dock_to("top", "quick");
        let strip = |e: &str| e == "bottom";
        let g = s.compute_geom(&extent_220, &strip, 1200.0, 900.0);
        assert_no_overlap(&g);
        let top = g.panels.iter().find(|p| p.edge == "top").unwrap().rect;
        assert_eq!(top, RectGeom { x: 220.0, y: 0.0, w: 760.0, h: 220.0 });
        assert_eq!(g.central, RectGeom { x: 220.0, y: 220.0, w: 760.0, h: 644.0 });
    }

    #[test]
    fn end_stack_dividers_split_their_own_span() {
        let mut s = DockStacks::default();
        s.dock_to("left", "sidebar");
        s.dock_to("bottom", "welcome");
        s.dock_to("bottom", "quick");
        let g = s.compute_geom(&extent_220, &no_strip, 1020.0, 800.0);
        // The bottom span is 220..1020 (800px), halved.
        let d = &g.dividers[0];
        assert_eq!(d.edge, "bottom");
        assert_eq!(d.rect, RectGeom { x: 620.0, y: 580.0, w: DIVIDER, h: 220.0 });
        // Dragging that divider maps back through the same span.
        assert!((split_ratio_at(620.0, Some((220.0, 800.0)), 1020.0) - 0.5).abs() < 1e-6);
        assert!((split_ratio_at(420.0, Some((220.0, 800.0)), 1020.0) - 0.25).abs() < 1e-6);
        assert!((split_ratio_at(510.0, None, 1020.0) - 0.5).abs() < 1e-6);
        assert_eq!(split_ratio_at(10.0, Some((220.0, 0.0)), 0.0), 0.5);
    }

    fn assert_no_overlap(g: &DockGeom) {
        let mut rects: Vec<RectGeom> = g.panels.iter().map(|p| p.rect).collect();
        rects.push(g.central);
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                let overlap_w = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
                let overlap_h = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
                assert!(overlap_w <= 0.0 || overlap_h <= 0.0, "{a:?} overlaps {b:?}");
            }
        }
    }

    fn expanded_left_sidebar_welcome(k: &str) -> Option<&'static str> {
        matches!(k, "sidebar" | "welcome").then_some("left")
    }

    fn expanded_none(_: &str) -> Option<&'static str> {
        None
    }

    #[test]
    fn rebuild_inherits_ratio_and_appends_new_panel() {
        let mut saved = DockStacks::default();
        saved.dock_to("left", "sidebar");
        saved.dock_to("left", "quick");
        saved.set_ratio("left", 0, 0.7);
        // Quick folds; welcome joins the left edge.
        let mut cur = DockStacks::default();
        cur.rebuild_from(&saved, &expanded_left_sidebar_welcome);
        assert_eq!(cur.left.len(), 2);
        assert_eq!(cur.left[0].kind, "sidebar");
        assert_eq!(cur.left[1].kind, "welcome");
        // The tuned 0.70 split for the surface panel is kept.
        assert!((cur.left[0].ratio - 0.7).abs() < 1e-3);
        assert!((cur.left.iter().map(|s| s.ratio).sum::<f32>() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn rebuild_drops_folded_panels() {
        let mut saved = DockStacks::default();
        saved.dock_to("left", "sidebar");
        saved.dock_to("left", "quick");
        let mut cur = DockStacks::default();
        cur.rebuild_from(&saved, &expanded_none);
        assert!(cur.left.is_empty());
        assert!(cur.edge_of("sidebar").is_none());
    }

    #[test]
    fn rebuild_moves_panel_to_its_new_edge() {
        let mut saved = DockStacks::default();
        saved.dock_to("left", "sidebar");
        let expanded = |k: &str| if k == "sidebar" { Some("right") } else { None };
        let mut cur = DockStacks::default();
        cur.rebuild_from(&saved, &expanded);
        assert!(cur.left.is_empty());
        assert_eq!(cur.right.len(), 1);
        assert_eq!(cur.right[0].kind, "sidebar");
    }
}