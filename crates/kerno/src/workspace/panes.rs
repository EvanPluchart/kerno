//! The split tab area (K19, D-092): the window's tabs in groups, side by side or one above the
//! other. Pure data: the workspace owns the tabs and draws this tree; every rule is here, tested
//! without a window.
//!
//! A group is a strip of tabs with one active tab; a split lays its children along one axis, each
//! with its share of the space. A split never holds a single child nor a child split along its own
//! axis (both are folded into it), so the tree is the simplest one that draws the same.

use serde::{Deserialize, Serialize};

/// A group's id: stable while the group lives, never given twice in a run.
pub type GroupId = usize;

/// The most groups shown at once: four is already a 2 × 2 grid, and a group narrower than a
/// quarter of the window shows little.
pub const MAX_GROUPS: usize = 4;

/// How children are laid out in a split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    /// Side by side.
    Row,
    /// One above the other.
    Column,
}

/// The edge of a group a tab is split to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    fn axis(self) -> Axis {
        match self {
            Edge::Left | Edge::Right => Axis::Row,
            Edge::Top | Edge::Bottom => Axis::Column,
        }
    }

    /// The new group comes after the one split (right of it, or below).
    fn after(self) -> bool {
        matches!(self, Edge::Right | Edge::Bottom)
    }
}

/// Where a tab dragged over a group's content lands, from the pointer's place as a fraction of
/// the content's width and height: within 35 % of a side it splits there, in the middle it joins
/// the group (the kit's dock uses the same zones).
pub fn zone_at(x: f32, y: f32) -> Option<Edge> {
    if x < 0.35 {
        Some(Edge::Left)
    } else if x > 0.65 {
        Some(Edge::Right)
    } else if y < 0.35 {
        Some(Edge::Top)
    } else if y > 0.65 {
        Some(Edge::Bottom)
    } else {
        None
    }
}

/// Why a tab cannot be split off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// It is the only tab of its group: the group would just move.
    Alone,
    /// [`MAX_GROUPS`] are shown already.
    TooMany,
}

impl Refusal {
    /// The reason, as a menu entry or a notification says it.
    pub fn reason(self) -> &'static str {
        match self {
            Refusal::Alone => "the tab is alone in its group",
            Refusal::TooMany => "four groups at most: close the tabs of one first",
        }
    }
}

/// A strip of tabs.
#[derive(Debug, Clone, PartialEq)]
pub struct Group<K> {
    pub id: GroupId,
    /// In the strip's order.
    pub tabs: Vec<K>,
    /// The position of the tab shown, in `tabs`.
    pub active: usize,
}

impl<K: Copy + Eq> Group<K> {
    fn new(id: GroupId) -> Self {
        Self {
            id,
            tabs: Vec::new(),
            active: 0,
        }
    }

    /// The tab shown, if the group has any.
    pub fn active_tab(&self) -> Option<K> {
        self.tabs.get(self.active).copied()
    }

    /// Takes `tab` out; the tab after it is shown when it was (else the one before), as when a
    /// tab is closed.
    fn take(&mut self, tab: K) -> bool {
        let Some(position) = self.tabs.iter().position(|each| *each == tab) else {
            return false;
        };
        self.tabs.remove(position);
        if self.active > position || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        true
    }
}

/// A node of the tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Pane<K> {
    Tabs(Group<K>),
    Split {
        axis: Axis,
        children: Vec<Pane<K>>,
        /// Each child's part of the split, summing to 1.
        shares: Vec<f32>,
    },
}

impl<K: Copy + Eq> Pane<K> {
    fn collect<'a>(&'a self, out: &mut Vec<&'a Group<K>>) {
        match self {
            Pane::Tabs(group) => out.push(group),
            Pane::Split { children, .. } => children.iter().for_each(|child| child.collect(out)),
        }
    }

    fn group_mut(&mut self, id: GroupId) -> Option<&mut Group<K>> {
        match self {
            Pane::Tabs(group) => (group.id == id).then_some(group),
            Pane::Split { children, .. } => {
                children.iter_mut().find_map(|child| child.group_mut(id))
            }
        }
    }

    fn first_group(&self) -> GroupId {
        match self {
            Pane::Tabs(group) => group.id,
            Pane::Split { children, .. } => children[0].first_group(),
        }
    }

    fn last_group(&self) -> GroupId {
        match self {
            Pane::Tabs(group) => group.id,
            Pane::Split { children, .. } => children[children.len() - 1].last_group(),
        }
    }

    /// Takes the group `id` out of the tree; its share goes to the child before it (the one
    /// after when it was first). Returns the group nearest to where it was.
    fn take_group(&mut self, id: GroupId) -> Option<GroupId> {
        let Pane::Split {
            children, shares, ..
        } = self
        else {
            return None;
        };
        let direct = children
            .iter()
            .position(|child| matches!(child, Pane::Tabs(group) if group.id == id));
        let Some(index) = direct else {
            return children.iter_mut().find_map(|child| child.take_group(id));
        };
        children.remove(index);
        let share = shares.remove(index);
        let neighbour = index.saturating_sub(1);
        shares[neighbour] += share;
        Some(if index > 0 {
            children[neighbour].last_group()
        } else {
            children[0].first_group()
        })
    }

    /// Puts `new` at `edge` of the group `beside`: a split of the two, halves each, in
    /// `beside`'s place ([`Self::normalize`] then folds it into a parent split of that axis).
    fn insert_beside(&mut self, beside: GroupId, edge: Edge, new: &mut Option<Group<K>>) -> bool {
        match self {
            Pane::Tabs(group) if group.id == beside => {
                let Some(new) = new.take() else { return false };
                let old = std::mem::replace(self, Pane::Tabs(Group::new(usize::MAX)));
                let children = if edge.after() {
                    vec![old, Pane::Tabs(new)]
                } else {
                    vec![Pane::Tabs(new), old]
                };
                *self = Pane::Split {
                    axis: edge.axis(),
                    children,
                    shares: vec![0.5, 0.5],
                };
                true
            }
            Pane::Tabs(_) => false,
            Pane::Split { children, .. } => children
                .iter_mut()
                .any(|child| child.insert_beside(beside, edge, new)),
        }
    }

    /// Folds a split of one child into that child and a child split along its parent's axis
    /// into the parent, with the shares it had.
    fn normalize(&mut self) {
        let Pane::Split {
            axis,
            children,
            shares,
        } = self
        else {
            return;
        };
        children.iter_mut().for_each(Pane::normalize);
        let mut flat = Vec::with_capacity(children.len());
        let mut flat_shares = Vec::with_capacity(children.len());
        for (child, share) in children.drain(..).zip(shares.drain(..)) {
            match child {
                Pane::Split {
                    axis: inner,
                    children: inner_children,
                    shares: inner_shares,
                } if inner == *axis => {
                    flat.extend(inner_children);
                    flat_shares.extend(inner_shares.into_iter().map(|each| each * share));
                }
                other => {
                    flat.push(other);
                    flat_shares.push(share);
                }
            }
        }
        if flat.len() == 1 {
            *self = flat.pop().expect("one child");
        } else {
            *children = flat;
            *shares = flat_shares;
        }
    }
}

/// The tab area: a tree of groups and the group the keyboard commands act on.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout<K> {
    root: Pane<K>,
    focused: GroupId,
    next: GroupId,
}

impl<K: Copy + Eq> Default for Layout<K> {
    fn default() -> Self {
        Self {
            root: Pane::Tabs(Group::new(0)),
            focused: 0,
            next: 1,
        }
    }
}

impl<K: Copy + Eq> Layout<K> {
    /// The tree, to draw it.
    pub fn root(&self) -> &Pane<K> {
        &self.root
    }

    /// Every group, left to right and top to bottom.
    pub fn groups(&self) -> Vec<&Group<K>> {
        let mut groups = Vec::new();
        self.root.collect(&mut groups);
        groups
    }

    pub fn group(&self, id: GroupId) -> Option<&Group<K>> {
        self.groups().into_iter().find(|group| group.id == id)
    }

    fn group_mut(&mut self, id: GroupId) -> Option<&mut Group<K>> {
        self.root.group_mut(id)
    }

    /// The group that holds `tab`.
    pub fn group_of(&self, tab: K) -> Option<GroupId> {
        self.groups()
            .into_iter()
            .find(|group| group.tabs.contains(&tab))
            .map(|group| group.id)
    }

    /// The group the keyboard commands act on, and where new tabs open.
    pub fn focused(&self) -> GroupId {
        self.focused
    }

    pub fn focus(&mut self, id: GroupId) -> bool {
        let known = self.group(id).is_some();
        if known {
            self.focused = id;
        }
        known
    }

    /// The tab shown in the focused group.
    pub fn active(&self) -> Option<K> {
        self.group(self.focused)?.active_tab()
    }

    /// Every tab, group after group, each in its strip's order.
    pub fn order(&self) -> Vec<K> {
        self.groups()
            .into_iter()
            .flat_map(|group| group.tabs.iter().copied())
            .collect()
    }

    /// The group after (or before) the group `from`, around.
    pub fn next_group(&self, from: GroupId, forward: bool) -> GroupId {
        let groups = self.groups();
        let at = groups
            .iter()
            .position(|group| group.id == from)
            .unwrap_or(0);
        let count = groups.len();
        let next = if forward {
            (at + 1) % count
        } else {
            (at + count - 1) % count
        };
        groups[next].id
    }

    /// Adds `tab` at the end of the focused group and shows it.
    pub fn add(&mut self, tab: K) {
        let focused = self.focused;
        let group = self.group_mut(focused).expect("the focused group exists");
        group.tabs.push(tab);
        group.active = group.tabs.len() - 1;
    }

    /// Shows `tab` and focuses its group.
    pub fn activate(&mut self, tab: K) -> bool {
        let Some(id) = self.group_of(tab) else {
            return false;
        };
        let group = self.group_mut(id).expect("found above");
        group.active = group
            .tabs
            .iter()
            .position(|each| *each == tab)
            .expect("found");
        self.focused = id;
        true
    }

    /// Takes `tab` out. A group left empty closes and its space goes to its neighbour, unless it
    /// is the last group, which stays (empty).
    pub fn remove(&mut self, tab: K) -> bool {
        let Some(id) = self.group_of(tab) else {
            return false;
        };
        let group = self.group_mut(id).expect("found above");
        group.take(tab);
        if group.tabs.is_empty() {
            self.close_group(id);
        }
        true
    }

    /// Closes the empty group `id` (never the last one).
    fn close_group(&mut self, id: GroupId) {
        if let Some(neighbour) = self.root.take_group(id) {
            if self.focused == id {
                self.focused = neighbour;
            }
            self.root.normalize();
        }
    }

    /// Moves `tab` right after `after`, in `after`'s group, leaving the tab shown in each group
    /// as it is.
    pub fn place_after(&mut self, tab: K, after: K) {
        if tab == after {
            return;
        }
        let Some(to) = self.group_of(after) else {
            return;
        };
        let from = self.group_of(tab);
        let shown = self.group(to).and_then(Group::active_tab);
        if let Some(from) = from {
            let source = self.group_mut(from).expect("found above");
            source.take(tab);
            if source.tabs.is_empty() && from != to {
                self.close_group(from);
            }
        }
        let group = self.group_mut(to).expect("found above");
        let at = group
            .tabs
            .iter()
            .position(|each| *each == after)
            .expect("found")
            + 1;
        group.tabs.insert(at, tab);
        if let Some(position) =
            shown.and_then(|shown| group.tabs.iter().position(|each| *each == shown))
        {
            group.active = position;
        }
    }

    /// Moves `tab` into the group `to` at `at` (clamped: past the end is the end), shows it there
    /// and focuses that group; within its own group it is a reorder. A group left empty closes.
    pub fn move_tab(&mut self, tab: K, to: GroupId, at: usize) -> bool {
        let (Some(from), true) = (self.group_of(tab), self.group(to).is_some()) else {
            return false;
        };
        let source = self.group_mut(from).expect("found above");
        let old = source
            .tabs
            .iter()
            .position(|each| *each == tab)
            .expect("found");
        source.take(tab);
        let at = if from == to && at > old { at - 1 } else { at };
        let left_empty = source.tabs.is_empty() && from != to;
        let target = self.group_mut(to).expect("found above");
        let at = at.min(target.tabs.len());
        target.tabs.insert(at, tab);
        target.active = at;
        self.focused = to;
        if left_empty {
            self.close_group(from);
        }
        true
    }

    /// Whether `tab` can go into a new group at an edge of the group `beside`.
    pub fn can_split(&self, tab: K, beside: GroupId) -> Result<(), Refusal> {
        let from = self.group_of(tab).ok_or(Refusal::Alone)?;
        let alone = self.group(from).is_some_and(|group| group.tabs.len() == 1);
        if alone && from == beside {
            return Err(Refusal::Alone);
        }
        let count = self.groups().len() + 1 - usize::from(alone);
        if count > MAX_GROUPS {
            return Err(Refusal::TooMany);
        }
        Ok(())
    }

    /// Puts `tab` in a new group at `edge` of the group `beside`, shows it there and focuses it.
    pub fn split(&mut self, tab: K, beside: GroupId, edge: Edge) -> Result<GroupId, Refusal> {
        self.can_split(tab, beside)?;
        if self.group(beside).is_none() {
            return Err(Refusal::Alone);
        }
        let from = self.group_of(tab).expect("checked by can_split");
        let source = self.group_mut(from).expect("found above");
        source.take(tab);
        if source.tabs.is_empty() {
            self.close_group(from);
        }
        let id = self.next;
        self.next += 1;
        let mut new = Some(Group {
            id,
            tabs: vec![tab],
            active: 0,
        });
        self.root.insert_beside(beside, edge, &mut new);
        self.root.normalize();
        self.focused = id;
        Ok(id)
    }

    /// Moves the divider after child `divider` of the split at `path` (child indices from the
    /// root) to `at`, a fraction of the split's length; neither side gets less than `min` of it.
    pub fn resize(&mut self, path: &[usize], divider: usize, at: f32, min: f32) -> bool {
        let mut pane = &mut self.root;
        for &step in path {
            let Pane::Split { children, .. } = pane else {
                return false;
            };
            let Some(child) = children.get_mut(step) else {
                return false;
            };
            pane = child;
        }
        let Pane::Split { shares, .. } = pane else {
            return false;
        };
        if divider + 1 >= shares.len() || !at.is_finite() {
            return false;
        }
        let start: f32 = shares[..divider].iter().sum();
        let pair = shares[divider] + shares[divider + 1];
        let min = min.clamp(0., pair / 2.);
        let left = (at - start).clamp(min, pair - min);
        if (left - shares[divider]).abs() < f32::EPSILON {
            return false;
        }
        shares[divider] = left;
        shares[divider + 1] = pair - left;
        true
    }

    /// What is stored: each group's tabs as indices into the stored tabs (`index_of` gives a
    /// tab's, `None` for a tab not stored).
    pub fn save(&self, index_of: impl Fn(K) -> Option<usize>) -> SavedPane {
        fn save<K: Copy + Eq>(pane: &Pane<K>, index_of: &dyn Fn(K) -> Option<usize>) -> SavedPane {
            match pane {
                Pane::Tabs(group) => SavedPane::Tabs {
                    tabs: group.tabs.iter().filter_map(|tab| index_of(*tab)).collect(),
                    active: group.active_tab().and_then(index_of),
                },
                Pane::Split {
                    axis,
                    children,
                    shares,
                } => SavedPane::Split {
                    axis: *axis,
                    children: children.iter().map(|child| save(child, index_of)).collect(),
                    shares: shares.clone(),
                },
            }
        }
        save(&self.root, &index_of)
    }

    /// The layout of the tabs opened again: `tabs[i]` is the stored tab `i` as it opened, `None`
    /// when it did not (its connection is gone). `saved` is what [`Self::save`] wrote, `None`
    /// before K19 or when it could not be read: one group then. A layout that no longer fits
    /// is mended, never refused: unknown and repeated tabs are dropped, empty groups closed, the
    /// tabs it does not place go to the first group, shares that make no sense are made even, and
    /// more than [`MAX_GROUPS`] groups become one. The stored tab `active` is shown and its group
    /// focused (the next tab that opened when it did not, else the one before).
    pub fn restore(saved: Option<&SavedPane>, tabs: &[Option<K>], active: usize) -> Self {
        fn build<K: Copy + Eq>(
            saved: &SavedPane,
            tabs: &[Option<K>],
            used: &mut [bool],
            next: &mut GroupId,
        ) -> Option<Pane<K>> {
            match saved {
                SavedPane::Tabs {
                    tabs: stored,
                    active,
                } => {
                    let mut group = Group::new(*next);
                    *next += 1;
                    for &index in stored {
                        let (Some(Some(tab)), Some(false)) = (tabs.get(index), used.get(index))
                        else {
                            continue;
                        };
                        used[index] = true;
                        if *active == Some(index) {
                            group.active = group.tabs.len();
                        }
                        group.tabs.push(*tab);
                    }
                    (!group.tabs.is_empty()).then_some(Pane::Tabs(group))
                }
                SavedPane::Split {
                    axis,
                    children,
                    shares,
                } => {
                    let mut built = Vec::new();
                    let mut kept = Vec::new();
                    for (index, child) in children.iter().enumerate() {
                        if let Some(child) = build(child, tabs, used, next) {
                            built.push(child);
                            kept.push(shares.get(index).copied().unwrap_or(f32::NAN));
                        }
                    }
                    if kept.iter().any(|share| !share.is_finite() || *share <= 0.) {
                        kept.iter_mut().for_each(|share| *share = 1.);
                    }
                    let total: f32 = kept.iter().sum();
                    kept.iter_mut().for_each(|share| *share /= total);
                    match built.len() {
                        0 => None,
                        1 => built.pop(),
                        _ => Some(Pane::Split {
                            axis: *axis,
                            children: built,
                            shares: kept,
                        }),
                    }
                }
            }
        }

        let mut used = vec![false; tabs.len()];
        let mut next = 0;
        let mut root = saved
            .and_then(|saved| build(saved, tabs, &mut used, &mut next))
            .unwrap_or_else(|| {
                next += 1;
                Pane::Tabs(Group::new(next - 1))
            });
        root.normalize();
        let mut layout = Self {
            focused: root.first_group(),
            root,
            next,
        };
        // Past the limit, every tab in one group; the tabs the layout does not place go to the
        // first group.
        if layout.groups().len() > MAX_GROUPS {
            let first = Group {
                id: layout.next,
                tabs: layout.order(),
                active: 0,
            };
            layout.next += 1;
            layout.focused = first.id;
            layout.root = Pane::Tabs(first);
        }
        let first = layout.root.first_group();
        for (index, tab) in tabs.iter().enumerate() {
            if let (Some(tab), false) = (tab, used[index]) {
                layout
                    .group_mut(first)
                    .expect("the first group")
                    .tabs
                    .push(*tab);
            }
        }
        let shown = tabs[active.min(tabs.len())..]
            .iter()
            .flatten()
            .next()
            .or_else(|| tabs[..active.min(tabs.len())].iter().flatten().next_back());
        if let Some(shown) = shown {
            layout.activate(*shown);
        }
        layout
    }
}

/// A stored layout: as [`Pane`], with each tab as its index in the stored tabs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedPane {
    Tabs {
        tabs: Vec<usize>,
        /// The tab shown, when it is stored.
        active: Option<usize>,
    },
    Split {
        axis: Axis,
        children: Vec<SavedPane>,
        shares: Vec<f32>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The groups' tabs, in order.
    fn strips(layout: &Layout<u32>) -> Vec<Vec<u32>> {
        layout
            .groups()
            .into_iter()
            .map(|group| group.tabs.clone())
            .collect()
    }

    fn with(tabs: &[u32]) -> Layout<u32> {
        let mut layout = Layout::default();
        tabs.iter().for_each(|tab| layout.add(*tab));
        layout
    }

    fn shares(layout: &Layout<u32>) -> Vec<f32> {
        match layout.root() {
            Pane::Split { shares, .. } => shares.clone(),
            Pane::Tabs(_) => vec![1.],
        }
    }

    #[test]
    fn one_group_behaves_as_the_old_strip() {
        let mut layout = with(&[1, 2, 3]);
        assert_eq!(layout.active(), Some(3));
        layout.activate(2);
        // Closing the tab shown shows the one after it, then the one before at the end.
        layout.remove(2);
        assert_eq!((layout.order(), layout.active()), (vec![1, 3], Some(3)));
        layout.remove(3);
        assert_eq!(layout.active(), Some(1));
        layout.remove(1);
        assert_eq!((strips(&layout), layout.active()), (vec![vec![]], None));
        // Placed after another tab, the tab shown stays.
        let mut layout = with(&[1, 2, 3]);
        layout.activate(1);
        layout.place_after(3, 1);
        assert_eq!((layout.order(), layout.active()), (vec![1, 3, 2], Some(1)));
    }

    #[test]
    fn a_split_makes_a_group_at_the_edge_and_focuses_it() {
        let mut layout = with(&[1, 2, 3]);
        let first = layout.focused();
        let right = layout.split(3, first, Edge::Right).unwrap();
        assert_eq!(strips(&layout), [vec![1, 2], vec![3]]);
        assert_eq!((layout.focused(), layout.active()), (right, Some(3)));
        assert_eq!(layout.group(first).unwrap().active_tab(), Some(2));
        // Again along the same axis: a third column, halving the one split.
        layout.split(2, first, Edge::Left).unwrap();
        assert_eq!(strips(&layout), [vec![2], vec![1], vec![3]]);
        assert_eq!(shares(&layout), [0.25, 0.25, 0.5]);
        // Down: the column of 3 becomes a column of two groups.
        layout.split(1, right, Edge::Bottom).unwrap();
        assert_eq!(strips(&layout), [vec![2], vec![3], vec![1]]);
        let Pane::Split { axis, children, .. } = layout.root() else {
            panic!("a split")
        };
        assert_eq!(*axis, Axis::Row);
        assert!(matches!(
            &children[1],
            Pane::Split {
                axis: Axis::Column,
                ..
            }
        ));
    }

    #[test]
    fn a_split_is_refused_for_a_lone_tab_and_past_four_groups() {
        let mut layout = with(&[1]);
        let first = layout.focused();
        assert_eq!(layout.split(1, first, Edge::Right), Err(Refusal::Alone));
        let mut layout = with(&[1, 2, 3, 4, 5]);
        let first = layout.focused();
        for tab in [2, 3, 4] {
            layout.split(tab, first, Edge::Right).unwrap();
        }
        assert_eq!(layout.groups().len(), MAX_GROUPS);
        assert_eq!(layout.split(5, first, Edge::Bottom), Err(Refusal::TooMany));
        assert_eq!(strips(&layout).concat().len(), 5, "nothing moved");
        // A lone tab moved to another group's edge leaves its group: no group more.
        let lone = layout.group_of(4).unwrap();
        assert_eq!(layout.can_split(4, first), Ok(()));
        layout.split(4, first, Edge::Top).unwrap();
        assert!(layout.group(lone).is_none());
        assert_eq!(layout.groups().len(), MAX_GROUPS);
    }

    #[test]
    fn a_group_left_empty_closes_and_its_neighbour_takes_its_space() {
        let mut layout = with(&[1, 2, 3]);
        let first = layout.focused();
        layout.split(2, first, Edge::Right).unwrap();
        let third = layout.split(3, first, Edge::Right).unwrap();
        assert_eq!(strips(&layout), [vec![1], vec![3], vec![2]]);
        assert_eq!(shares(&layout), [0.25, 0.25, 0.5]);
        layout.remove(3);
        assert!(layout.group(third).is_none());
        assert_eq!(strips(&layout), [vec![1], vec![2]]);
        assert_eq!(shares(&layout), [0.5, 0.5]);
        assert_eq!(layout.focused(), first, "the focus goes to the neighbour");
        // The last group stays, empty.
        layout.remove(2);
        layout.remove(1);
        assert_eq!(strips(&layout), [Vec::<u32>::new()]);
        layout.add(4);
        assert_eq!(layout.active(), Some(4));
    }

    #[test]
    fn a_nested_split_folds_back_when_it_loses_a_group() {
        let mut layout = with(&[1, 2, 3, 4]);
        let first = layout.focused();
        let right = layout.split(2, first, Edge::Right).unwrap();
        layout.split(3, right, Edge::Bottom).unwrap();
        layout.split(4, right, Edge::Right).unwrap();
        // Row[1, Column[Row[2, 4], 3]]
        layout.remove(3);
        // The column of one row folds into the outer row: Row[1, 2, 4].
        assert_eq!(strips(&layout), [vec![1], vec![2], vec![4]]);
        let Pane::Split {
            axis,
            children,
            shares,
        } = layout.root()
        else {
            panic!("a split")
        };
        assert_eq!((*axis, children.len()), (Axis::Row, 3));
        assert!((shares.iter().sum::<f32>() - 1.).abs() < 1e-5);
        assert_eq!(shares, &[0.5, 0.25, 0.25]);
    }

    #[test]
    fn a_tab_moves_between_groups_and_reorders_in_its_own() {
        let mut layout = with(&[1, 2, 3]);
        let first = layout.focused();
        let right = layout.split(3, first, Edge::Right).unwrap();
        // Into the other group, before its tab: that group is focused.
        layout.focus(first);
        assert!(layout.move_tab(1, right, 0));
        assert_eq!(strips(&layout), [vec![2], vec![1, 3]]);
        assert_eq!((layout.focused(), layout.active()), (right, Some(1)));
        // Within the group: 1 to the end.
        layout.move_tab(1, right, 2);
        assert_eq!(strips(&layout), [vec![2], vec![3, 1]]);
        layout.move_tab(1, right, 0);
        assert_eq!(strips(&layout), [vec![2], vec![1, 3]]);
        // The last tab of a group moved out closes it.
        layout.move_tab(2, right, 99);
        assert_eq!(strips(&layout), [vec![1, 3, 2]]);
        assert!(layout.group(first).is_none());
        assert_eq!(layout.next_group(right, true), right);
    }

    #[test]
    fn the_focus_goes_round_the_groups() {
        let mut layout = with(&[1, 2, 3]);
        let first = layout.focused();
        let second = layout.split(2, first, Edge::Right).unwrap();
        let third = layout.split(3, second, Edge::Bottom).unwrap();
        assert_eq!(layout.focused(), third);
        assert_eq!(layout.next_group(third, true), first);
        assert_eq!(layout.next_group(third, false), second);
        assert!(layout.focus(first));
        assert!(!layout.focus(99));
        assert_eq!(layout.active(), Some(1));
        // A tab opened goes to the focused group.
        layout.add(4);
        assert_eq!(strips(&layout), [vec![1, 4], vec![2], vec![3]]);
    }

    #[test]
    fn a_divider_moves_within_its_pair_and_keeps_a_minimum() {
        let mut layout = with(&[1, 2, 3]);
        let first = layout.focused();
        layout.split(2, first, Edge::Right).unwrap();
        layout.split(3, first, Edge::Right).unwrap();
        // [0.25, 0.25, 0.5]: the second divider (between 3 and 2) to 0.75.
        assert!(layout.resize(&[], 1, 0.75, 0.1));
        assert_eq!(shares(&layout), [0.25, 0.5, 0.25]);
        // Past its neighbour: stopped at the minimum.
        layout.resize(&[], 0, 0.95, 0.1);
        let got = shares(&layout);
        assert!(
            (got[0] - 0.65).abs() < 1e-5 && (got[1] - 0.1).abs() < 1e-5,
            "{got:?}"
        );
        assert!(!layout.resize(&[3], 0, 0.5, 0.1), "no such split");
        assert!(!layout.resize(&[], 2, 0.5, 0.1), "no such divider");
        assert!(!layout.resize(&[], 0, f32::NAN, 0.1));
    }

    #[test]
    fn a_layout_is_stored_and_restored() {
        let mut layout = with(&[10, 20, 30, 40]);
        let first = layout.focused();
        let right = layout.split(30, first, Edge::Right).unwrap();
        layout.move_tab(40, right, 9);
        layout.split(20, right, Edge::Bottom).unwrap();
        layout.resize(&[], 0, 0.3, 0.1);
        layout.activate(30);
        // Tab 20 is not stored (a table form): its group is lost on restore.
        let order = [10, 30, 40];
        let saved = layout.save(|tab| order.iter().position(|each| *each == tab));
        let json = serde_json::to_string(&saved).unwrap();
        let read: SavedPane = serde_json::from_str(&json).unwrap();
        let tabs: Vec<Option<u32>> = order.iter().map(|tab| Some(*tab)).collect();
        let restored = Layout::restore(Some(&read), &tabs, 1);
        assert_eq!(strips(&restored), [vec![10], vec![30, 40]]);
        assert_eq!(restored.active(), Some(30));
        assert!(
            (shares(&restored)[0] - 0.3).abs() < 1e-5,
            "{:?}",
            shares(&restored)
        );
    }

    #[test]
    fn a_layout_from_before_k19_or_broken_restores_as_one_group() {
        let tabs = [Some(1u32), None, Some(3), Some(4)];
        // None stored: one group, the active one shown (the next that opened when it did not).
        let layout = Layout::restore(None, &tabs, 1);
        assert_eq!(
            (strips(&layout), layout.active()),
            (vec![vec![1, 3, 4]], Some(3))
        );
        let layout = Layout::restore(None, &tabs, 9);
        assert_eq!(layout.active(), Some(4));
        let layout = Layout::<u32>::restore(None, &[], 0);
        assert_eq!((strips(&layout), layout.active()), (vec![vec![]], None));
        // Unknown, repeated and missing tabs, a group of none, shares that make no sense.
        let broken = SavedPane::Split {
            axis: Axis::Column,
            children: vec![
                SavedPane::Tabs {
                    tabs: vec![0, 7, 0],
                    active: Some(7),
                },
                SavedPane::Tabs {
                    tabs: vec![1],
                    active: None,
                },
                SavedPane::Tabs {
                    tabs: vec![3],
                    active: Some(3),
                },
            ],
            shares: vec![f32::NAN, -1.],
        };
        let layout = Layout::restore(Some(&broken), &tabs, 0);
        assert_eq!(strips(&layout), [vec![1, 3], vec![4]]);
        assert_eq!(shares(&layout), [0.5, 0.5]);
        assert_eq!(layout.active(), Some(1));
        // More groups than shown at once: one group.
        let many = SavedPane::Split {
            axis: Axis::Row,
            children: (0..5)
                .map(|tab| SavedPane::Tabs {
                    tabs: vec![tab],
                    active: None,
                })
                .collect(),
            shares: vec![0.2; 5],
        };
        let tabs: Vec<Option<u32>> = (0..5).map(Some).collect();
        let layout = Layout::restore(Some(&many), &tabs, 2);
        assert_eq!(
            (strips(&layout), layout.active()),
            (vec![vec![0, 1, 2, 3, 4]], Some(2))
        );
        // Text that is not a layout does not parse: the store gives none, so one group.
        assert!(serde_json::from_str::<SavedPane>("{\"tabs\": 3}").is_err());
    }

    #[test]
    fn the_drop_zones_are_the_outer_thirds() {
        assert_eq!(zone_at(0.1, 0.5), Some(Edge::Left));
        assert_eq!(zone_at(0.9, 0.5), Some(Edge::Right));
        assert_eq!(zone_at(0.5, 0.1), Some(Edge::Top));
        assert_eq!(zone_at(0.5, 0.9), Some(Edge::Bottom));
        assert_eq!(zone_at(0.5, 0.5), None);
        // Up to 35 % of a side; the sides win over the top and the bottom in the corners.
        assert_eq!(zone_at(0.34, 0.5), Some(Edge::Left));
        assert_eq!(zone_at(0.5, 0.66), Some(Edge::Bottom));
        assert_eq!(zone_at(0.36, 0.64), None);
        assert_eq!(zone_at(0.1, 0.1), Some(Edge::Left));
    }
}
