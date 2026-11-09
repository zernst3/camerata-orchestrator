//! Which nodes are open, which sites are loaded, and where the cursor is (spec section 5).
//!
//! Sites load per node, [`SITES_PAGE_SIZE`] at a time, through the sites page cursor (spec U1).
//! Opening a sub-group asks for exactly one page; "Show next 500" asks for the next one. A node
//! is never asked for twice while its first page is in flight or loaded, and the state never
//! holds a site it has not been given.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use camerata_api_types::review_view::{
    SiteRow, SitesPage, TreeLevel, TreeNodeView, TreeView, SITES_PAGE_SIZE,
};

use crate::triage::FocusMove;

use super::is_outlier;

// ── The tree's shape ──────────────────────────────────────────────────────────────────────

/// A [`TreeView`] indexed for walking: children by parent, nodes by id.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TreeIndex {
    nodes: Vec<TreeNodeView>,
    by_id: HashMap<String, usize>,
    children: HashMap<String, Vec<usize>>,
    roots: Vec<usize>,
}

impl TreeIndex {
    pub fn new(view: &TreeView) -> Self {
        let nodes = view.nodes.clone();
        let mut by_id = HashMap::with_capacity(nodes.len());
        let mut children: HashMap<String, Vec<usize>> = HashMap::new();
        let mut roots = Vec::new();
        for (i, n) in nodes.iter().enumerate() {
            by_id.insert(n.id.clone(), i);
        }
        for (i, n) in nodes.iter().enumerate() {
            match n.parent.as_ref().filter(|p| by_id.contains_key(*p)) {
                Some(parent) => children.entry(parent.clone()).or_default().push(i),
                None => roots.push(i),
            }
        }
        Self {
            nodes,
            by_id,
            children,
            roots,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn node(&self, id: &str) -> Option<&TreeNodeView> {
        self.by_id.get(id).map(|&i| &self.nodes[i])
    }

    /// The top level nodes, in tree order.
    pub fn roots(&self) -> impl Iterator<Item = &TreeNodeView> {
        self.roots.iter().map(|&i| &self.nodes[i])
    }

    /// The nodes directly under `id`, in tree order.
    pub fn children(&self, id: &str) -> impl Iterator<Item = &TreeNodeView> {
        self.children
            .get(id)
            .into_iter()
            .flatten()
            .map(|&i| &self.nodes[i])
    }

    pub fn parent_of(&self, id: &str) -> Option<&str> {
        self.node(id)
            .and_then(|n| n.parent.as_deref())
            .filter(|p| self.by_id.contains_key(*p))
    }

    /// Whether the node holds sites directly (a sub-group) and not other nodes.
    pub fn holds_sites(&self, id: &str) -> bool {
        self.node(id)
            .is_some_and(|n| n.level == TreeLevel::Subgroup)
    }

    /// The labels from the top level node down to `id`, as `(id, label)`, for the breadcrumb.
    pub fn breadcrumb(&self, id: &str) -> Vec<(String, String)> {
        self.path_to(id)
            .into_iter()
            .map(|node| {
                let label = self
                    .node(&node)
                    .map_or_else(String::new, |n| n.label.clone());
                (node, label)
            })
            .collect()
    }

    /// `id` and every node above it, root first.
    pub fn path_to(&self, id: &str) -> Vec<String> {
        let mut chain = vec![id.to_string()];
        let mut at = id;
        while let Some(parent) = self.parent_of(at) {
            chain.push(parent.to_string());
            at = parent;
        }
        chain.reverse();
        chain
    }

    /// Whether `node` is `ancestor` or sits under it.
    pub fn is_under(&self, node: &str, ancestor: &str) -> bool {
        self.path_to(node).iter().any(|n| n == ancestor)
    }

    /// Every sub-group at or under `id`, in tree order.
    pub fn subgroups_under(&self, id: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![id.to_string()];
        while let Some(at) = stack.pop() {
            if self.holds_sites(&at) {
                out.push(at.clone());
            }
            let kids: Vec<String> = self.children(&at).map(|n| n.id.clone()).collect();
            stack.extend(kids.into_iter().rev());
        }
        out
    }
}

// ── What is loaded ────────────────────────────────────────────────────────────────────────

/// Where a node's sites stand.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Load {
    #[default]
    Idle,
    /// A page was asked for and has not come back.
    Loading,
    /// The last page failed, with the server's reason. The sites that did load stay.
    Failed(String),
}

/// The sites of one node that have been loaded so far.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LoadedSites {
    pub rows: Vec<SiteRow>,
    /// Sites under the node in all, as the last page said.
    pub total: usize,
    /// Pass back for the next page; `None` once the last page is in (or before the first).
    pub next_cursor: Option<String>,
    pub load: Load,
    /// The first page has arrived at least once.
    pub first_page_in: bool,
}

impl LoadedSites {
    /// How many more sites the node has than are loaded.
    pub fn remaining(&self) -> usize {
        self.total.saturating_sub(self.rows.len())
    }

    /// The label of the "Show next 500" row, when there is a next page.
    pub fn show_more_label(&self) -> Option<String> {
        self.next_cursor.as_ref()?;
        let next = self.remaining().min(SITES_PAGE_SIZE);
        Some(format!(
            "Show next {} of {} remaining",
            super::thousands(next),
            super::thousands(self.remaining())
        ))
    }
}

/// One read of a node's sites a host must make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SitesRequest {
    pub node: String,
    pub cursor: Option<String>,
    pub limit: usize,
}

// ── The cursor ────────────────────────────────────────────────────────────────────────────

/// One stop of the keyboard cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeRef {
    Node(String),
    Site { node: String, key: String },
}

impl TreeRef {
    /// The node the stop is at, or the node a site is under.
    pub fn node_id(&self) -> &str {
        match self {
            Self::Node(id) | Self::Site { node: id, .. } => id,
        }
    }
}

// ── The state ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default)]
pub struct TreeState {
    /// Node ids that are open, ancestors included.
    pub open: BTreeSet<String>,
    pub loaded: BTreeMap<String, LoadedSites>,
    pub cursor: Option<TreeRef>,
    /// Outlier site keys the reviewer ticked into a batch (spec section 5, "Tick an outlier").
    pub ticked: BTreeSet<String>,
    /// Carried rules the reviewer unticked out of the next place decision (D2).
    pub place_unticked: BTreeSet<String>,
    /// A sub-group the reviewer asked to enter with Enter before its first page was in: the
    /// cursor moves to its first site when the page arrives.
    pub enter_when_loaded: Option<String>,
}

impl TreeState {
    /// The stops of the cursor, top to bottom: every top level node, and under an open node its
    /// children (or, for a sub-group, its loaded sites).
    pub fn visible(&self, index: &TreeIndex) -> Vec<TreeRef> {
        let mut out = Vec::new();
        let mut stack: Vec<String> = index.roots().map(|n| n.id.clone()).collect();
        stack.reverse();
        while let Some(id) = stack.pop() {
            out.push(TreeRef::Node(id.clone()));
            if !self.open.contains(&id) {
                continue;
            }
            if index.holds_sites(&id) {
                if let Some(loaded) = self.loaded.get(&id) {
                    out.extend(loaded.rows.iter().map(|r| TreeRef::Site {
                        node: id.clone(),
                        key: r.site_key.clone(),
                    }));
                }
            } else {
                let kids: Vec<String> = index.children(&id).map(|n| n.id.clone()).collect();
                stack.extend(kids.into_iter().rev());
            }
        }
        out
    }

    pub fn is_open(&self, id: &str) -> bool {
        self.open.contains(id)
    }

    /// The loaded site with `key` under `node`.
    pub fn site(&self, node: &str, key: &str) -> Option<&SiteRow> {
        self.loaded
            .get(node)?
            .rows
            .iter()
            .find(|r| r.site_key == key)
    }

    /// The site the cursor is on, when it is on one.
    pub fn cursor_site(&self) -> Option<&SiteRow> {
        match self.cursor.as_ref()? {
            TreeRef::Site { node, key } => self.site(node, key),
            TreeRef::Node(_) => None,
        }
    }

    /// What kind of stop the cursor is on, for the key map.
    pub fn cursor_kind(&self) -> super::keys::CursorKind {
        use super::keys::CursorKind;
        match self.cursor.as_ref() {
            None => CursorKind::None,
            Some(TreeRef::Node(_)) => CursorKind::Node,
            Some(TreeRef::Site { .. }) => {
                if self.cursor_site().is_some_and(is_outlier) {
                    CursorKind::OutlierSite
                } else {
                    CursorKind::Site
                }
            }
        }
    }

    // ── open and close ────────────────────────────────────────────────────────────────────

    /// Open `id`. A sub-group with nothing loaded asks for exactly one page (spec U1); one that
    /// is loaded or loading asks for nothing; one whose last read failed asks again.
    pub fn open_node(&mut self, index: &TreeIndex, id: &str) -> Vec<SitesRequest> {
        if index.node(id).is_none() {
            return Vec::new();
        }
        self.open.insert(id.to_string());
        if !index.holds_sites(id) {
            return Vec::new();
        }
        match self.loaded.get_mut(id) {
            None => {
                self.loaded.insert(
                    id.to_string(),
                    LoadedSites {
                        load: Load::Loading,
                        ..LoadedSites::default()
                    },
                );
                vec![SitesRequest {
                    node: id.to_string(),
                    cursor: None,
                    limit: SITES_PAGE_SIZE,
                }]
            }
            Some(entry) if matches!(entry.load, Load::Failed(_)) => {
                entry.load = Load::Loading;
                vec![SitesRequest {
                    node: id.to_string(),
                    cursor: entry.next_cursor.clone(),
                    limit: SITES_PAGE_SIZE,
                }]
            }
            Some(_) => Vec::new(),
        }
    }

    /// Open every node above `id` and `id` itself (to reach a site a door names).
    pub fn open_path(&mut self, index: &TreeIndex, id: &str) -> Vec<SitesRequest> {
        let mut requests = Vec::new();
        for node in index.path_to(id) {
            requests.extend(self.open_node(index, &node));
        }
        requests
    }

    /// Close `id`. A cursor on something under it moves to `id`. What was loaded stays, so
    /// opening it again asks for nothing.
    pub fn close_node(&mut self, index: &TreeIndex, id: &str) {
        self.open.remove(id);
        let under = self
            .cursor
            .as_ref()
            .is_some_and(|c| c.node_id() != id && index.is_under(c.node_id(), id))
            || matches!(&self.cursor, Some(TreeRef::Site { node, .. }) if node == id);
        if under {
            self.cursor = Some(TreeRef::Node(id.to_string()));
        }
    }

    /// Close everything (`Shift Left`). The cursor goes to the top level node it was under.
    pub fn collapse_all(&mut self, index: &TreeIndex) {
        let top = self
            .cursor
            .as_ref()
            .and_then(|c| index.path_to(c.node_id()).into_iter().next());
        self.open.clear();
        if let Some(top) = top {
            self.cursor = Some(TreeRef::Node(top));
        }
    }

    // ── paging ────────────────────────────────────────────────────────────────────────────

    /// "Show next 500": the next page of `node`, when there is one and none is in flight.
    pub fn request_more(&mut self, node: &str) -> Option<SitesRequest> {
        let entry = self.loaded.get_mut(node)?;
        if entry.load == Load::Loading {
            return None;
        }
        let cursor = entry.next_cursor.clone()?;
        entry.load = Load::Loading;
        Some(SitesRequest {
            node: node.to_string(),
            cursor: Some(cursor),
            limit: SITES_PAGE_SIZE,
        })
    }

    /// A page came back. The first page replaces what was there; a later page is appended
    /// (a row already held is not added twice).
    pub fn apply_page(&mut self, page: &SitesPage) {
        let entry = self.loaded.entry(page.node.clone()).or_default();
        if page.offset == 0 {
            entry.rows.clear();
        }
        let have: BTreeSet<String> = entry.rows.iter().map(|r| r.site_key.clone()).collect();
        entry.rows.extend(
            page.rows
                .iter()
                .filter(|r| !have.contains(&r.site_key))
                .cloned(),
        );
        entry.total = page.total;
        entry.next_cursor.clone_from(&page.next_cursor);
        entry.load = Load::Idle;
        entry.first_page_in = true;
        if self.enter_when_loaded.as_deref() == Some(page.node.as_str()) {
            self.enter_when_loaded = None;
            if let Some(first) = page.rows.first() {
                self.cursor = Some(TreeRef::Site {
                    node: page.node.clone(),
                    key: first.site_key.clone(),
                });
            }
        }
    }

    /// A read failed. The sites that did load stay; the node shows the reason and a Retry.
    pub fn apply_failure(&mut self, node: &str, message: &str) {
        let entry = self.loaded.entry(node.to_string()).or_default();
        entry.load = Load::Failed(message.to_string());
        if self.enter_when_loaded.as_deref() == Some(node) {
            self.enter_when_loaded = None;
        }
    }

    /// Forget what is loaded at and under `id` (a decision changed those rows) and ask again for
    /// each sub-group that is open: one page each.
    pub fn reload_under(&mut self, index: &TreeIndex, id: &str) -> Vec<SitesRequest> {
        let mut requests = Vec::new();
        for sub in index.subgroups_under(id) {
            if self.loaded.remove(&sub).is_some() && self.open.contains(&sub) {
                self.loaded.insert(
                    sub.clone(),
                    LoadedSites {
                        load: Load::Loading,
                        ..LoadedSites::default()
                    },
                );
                requests.push(SitesRequest {
                    node: sub,
                    cursor: None,
                    limit: SITES_PAGE_SIZE,
                });
            }
        }
        requests
    }

    // ── the cursor ────────────────────────────────────────────────────────────────────────

    /// Where the cursor is among `visible`: its index, or the first stop when it is on nothing
    /// that shows.
    pub fn resolve_cursor(&self, visible: &[TreeRef]) -> usize {
        self.cursor
            .as_ref()
            .and_then(|c| visible.iter().position(|v| v == c))
            .unwrap_or(0)
    }

    /// Move the cursor one stop. No wrap-around (the Queue's rule: a held key must not restart
    /// at the top and read as if it were still making progress).
    pub fn move_cursor(&mut self, index: &TreeIndex, dir: FocusMove) {
        let visible = self.visible(index);
        if visible.is_empty() {
            return;
        }
        // With no cursor, or one on something that no longer shows, the first press lands on the
        // first stop in either direction.
        let on_screen = self
            .cursor
            .as_ref()
            .and_then(|c| visible.iter().position(|v| v == c));
        let Some(at) = on_screen else {
            self.cursor = Some(visible[0].clone());
            return;
        };
        let to = match dir {
            FocusMove::Up => at.saturating_sub(1),
            FocusMove::Down => (at + 1).min(visible.len() - 1),
        };
        self.cursor = Some(visible[to].clone());
    }

    /// Put the cursor on `node`.
    pub fn focus_node(&mut self, node: &str) {
        self.cursor = Some(TreeRef::Node(node.to_string()));
    }

    /// Put the cursor on a site.
    pub fn focus_site(&mut self, node: &str, key: &str) {
        self.cursor = Some(TreeRef::Site {
            node: node.to_string(),
            key: key.to_string(),
        });
    }

    /// `Right`: open a closed node; on an open node, step into its first child (its first node,
    /// or for a sub-group its first site).
    pub fn press_right(&mut self, index: &TreeIndex) -> Vec<SitesRequest> {
        let Some(TreeRef::Node(id)) = self.cursor.clone() else {
            return Vec::new();
        };
        if !self.is_open(&id) {
            return self.open_node(index, &id);
        }
        self.step_into(index, &id);
        Vec::new()
    }

    /// `Left`: close an open node; on a closed node go to its parent; on a site go to its node.
    pub fn press_left(&mut self, index: &TreeIndex) {
        match self.cursor.clone() {
            Some(TreeRef::Site { node, .. }) => self.focus_node(&node),
            Some(TreeRef::Node(id)) => {
                if self.is_open(&id) {
                    self.close_node(index, &id);
                } else if let Some(parent) = index.parent_of(&id) {
                    let parent = parent.to_string();
                    self.focus_node(&parent);
                }
            }
            None => {}
        }
    }

    /// `Enter` on a node: open it when closed, then move to its first child. A sub-group whose
    /// page has not come back moves in when it does.
    pub fn press_enter(&mut self, index: &TreeIndex) -> Vec<SitesRequest> {
        let Some(TreeRef::Node(id)) = self.cursor.clone() else {
            return Vec::new();
        };
        let mut requests = Vec::new();
        if !self.is_open(&id) {
            requests = self.open_node(index, &id);
        }
        if index.holds_sites(&id)
            && self
                .loaded
                .get(&id)
                .is_none_or(|l| l.rows.is_empty() && l.load != Load::Idle)
        {
            self.enter_when_loaded = Some(id);
        } else {
            self.step_into(index, &id);
        }
        requests
    }

    fn step_into(&mut self, index: &TreeIndex, id: &str) {
        let visible = self.visible(index);
        let Some(at) = visible
            .iter()
            .position(|v| matches!(v, TreeRef::Node(n) if n == id))
        else {
            return;
        };
        if let Some(next) = visible.get(at + 1) {
            let child = match next {
                TreeRef::Node(n) => index.parent_of(n) == Some(id),
                TreeRef::Site { node, .. } => node == id,
            };
            if child {
                self.cursor = Some(next.clone());
            }
        }
    }

    // ── ticks ─────────────────────────────────────────────────────────────────────────────

    /// Tick or untick a carried rule into the next place decision. A held rule is locked out.
    pub fn toggle_carried(
        &mut self,
        rule: &camerata_api_types::review_view::CarriedRuleView,
    ) -> bool {
        let mut ticks = super::place::PlaceTicks {
            unticked: std::mem::take(&mut self.place_unticked),
        };
        let changed = ticks.toggle(rule);
        self.place_unticked = ticks.unticked;
        changed
    }

    /// Tick or untick an outlier into the batch. A site that is not an outlier is not ticked.
    pub fn toggle_tick(&mut self, key: &str, row: &SiteRow) -> bool {
        if !is_outlier(row) {
            return false;
        }
        if !self.ticked.remove(key) {
            self.ticked.insert(key.to_string());
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::review_fixtures::dataset_30k;
    use crate::tree::tests::data;
    use camerata_api_types::review_view::Marker;

    fn index() -> TreeIndex {
        TreeIndex::new(&data().tree)
    }

    fn first_subgroup() -> String {
        data()
            .tree
            .nodes
            .iter()
            .find(|n| n.level == TreeLevel::Subgroup && !n.thrown_out && n.counts.sites > 3)
            .map(|n| n.id.clone())
            .expect("a sub-group")
    }

    fn fulfil(state: &mut TreeState, req: &SitesRequest) {
        let page = data()
            .sites_page(&req.node, req.cursor.as_deref(), req.limit)
            .expect("the node exists");
        state.apply_page(&page);
    }

    #[test]
    fn opening_a_category_or_a_rule_asks_for_no_sites() {
        let idx = index();
        let mut st = TreeState::default();
        let cat = idx.roots().next().unwrap().id.clone();
        assert!(st.open_node(&idx, &cat).is_empty());
        let rule = idx.children(&cat).next().unwrap().id.clone();
        assert!(st.open_node(&idx, &rule).is_empty());
        assert!(
            st.loaded.is_empty(),
            "nothing is loaded until a sub-group opens"
        );
    }

    #[test]
    fn tree_never_requests_more_than_one_sites_page_per_open() {
        // The biggest node of the 30,000 site set: far more than one page of sites.
        let ds = dataset_30k();
        let idx = TreeIndex::new(&ds.tree);
        let big = ds
            .tree
            .nodes
            .iter()
            .filter(|n| n.level == TreeLevel::Subgroup)
            .max_by_key(|n| n.counts.sites)
            .unwrap()
            .id
            .clone();
        let total = ds
            .tree
            .nodes
            .iter()
            .find(|n| n.id == big)
            .unwrap()
            .counts
            .sites;
        let mut st = TreeState::default();
        let reqs = st.open_node(&idx, &big);
        assert_eq!(reqs.len(), 1, "one page per open");
        assert_eq!(reqs[0].limit, SITES_PAGE_SIZE);
        assert_eq!(reqs[0].cursor, None, "the first page");
        // While the page is in flight, opening again asks for nothing.
        assert!(st.open_node(&idx, &big).is_empty());
        assert!(
            st.request_more(&big).is_none(),
            "no second page while one is in flight"
        );
        let page = ds.sites_page(&big, None, SITES_PAGE_SIZE).unwrap();
        st.apply_page(&page);
        assert!(st.loaded[&big].rows.len() <= SITES_PAGE_SIZE);
        if total > SITES_PAGE_SIZE {
            // Loaded: opening again, closing and opening again ask for nothing.
            assert!(st.open_node(&idx, &big).is_empty());
            st.close_node(&idx, &big);
            assert!(st.open_node(&idx, &big).is_empty());
            // "Show next 500" asks for exactly the next page.
            let more = st.request_more(&big).expect("a next page");
            assert_eq!(more.cursor, page.next_cursor);
            assert!(st.request_more(&big).is_none(), "and not twice");
        }
    }

    #[test]
    fn the_show_more_row_counts_what_is_left() {
        let ds = dataset_30k();
        let idx = TreeIndex::new(&ds.tree);
        let big = ds
            .tree
            .nodes
            .iter()
            .filter(|n| n.level == TreeLevel::Subgroup)
            .max_by_key(|n| n.counts.sites)
            .unwrap()
            .id
            .clone();
        let mut st = TreeState::default();
        st.open_node(&idx, &big);
        // A node of 600 sites, standing in for a big one.
        let mut page = ds.sites_page(&big, None, 5).unwrap();
        page.total = 600;
        page.next_cursor = Some("c".to_string());
        st.apply_page(&page);
        let label = st.loaded[&big].show_more_label().unwrap();
        assert_eq!(label, "Show next 500 of 595 remaining");
    }

    #[test]
    fn a_failed_page_keeps_what_loaded_and_asks_again_on_the_next_open() {
        let idx = index();
        let sub = first_subgroup();
        let mut st = TreeState::default();
        let req = st.open_node(&idx, &sub).remove(0);
        fulfil(&mut st, &req);
        let loaded = st.loaded[&sub].rows.len();
        assert!(loaded > 0);
        st.apply_failure(&sub, "The request failed with status 500.");
        assert_eq!(st.loaded[&sub].rows.len(), loaded, "the sites stay");
        assert!(matches!(st.loaded[&sub].load, Load::Failed(_)));
        assert_eq!(st.open_node(&idx, &sub).len(), 1, "a Retry asks once");
        assert!(st.open_node(&idx, &sub).is_empty());
    }

    #[test]
    fn only_the_top_level_shows_until_a_node_opens_and_a_sub_group_shows_its_loaded_sites() {
        let idx = index();
        let sub = first_subgroup();
        let mut st = TreeState::default();
        let collapsed = st.visible(&idx);
        assert_eq!(collapsed.len(), idx.roots().count());
        assert!(collapsed.iter().all(|r| matches!(r, TreeRef::Node(_))));

        let reqs = st.open_path(&idx, &sub);
        assert_eq!(reqs.len(), 1);
        fulfil(&mut st, &reqs[0]);
        let visible = st.visible(&idx);
        let sites = visible
            .iter()
            .filter(|r| matches!(r, TreeRef::Site { .. }))
            .count();
        assert_eq!(sites, st.loaded[&sub].rows.len());
        // The sub-group's sites sit right under it.
        let at = visible
            .iter()
            .position(|r| *r == TreeRef::Node(sub.clone()))
            .unwrap();
        assert!(matches!(&visible[at + 1], TreeRef::Site { node, .. } if *node == sub));
    }

    #[test]
    fn the_cursor_moves_one_stop_and_does_not_wrap() {
        let idx = index();
        let mut st = TreeState::default();
        st.move_cursor(&idx, FocusMove::Down);
        let first = idx.roots().next().unwrap().id.clone();
        let second = idx.roots().nth(1).unwrap().id.clone();
        assert_eq!(
            st.cursor,
            Some(TreeRef::Node(first.clone())),
            "from nothing: the first stop"
        );
        st.move_cursor(&idx, FocusMove::Down);
        assert_eq!(st.cursor, Some(TreeRef::Node(second)));
        st.move_cursor(&idx, FocusMove::Up);
        st.move_cursor(&idx, FocusMove::Up);
        assert_eq!(
            st.cursor,
            Some(TreeRef::Node(first)),
            "no wrap past the top"
        );
        let last = st.visible(&idx).last().cloned().unwrap();
        st.cursor = Some(last.clone());
        st.move_cursor(&idx, FocusMove::Down);
        assert_eq!(st.cursor, Some(last), "no wrap past the bottom");
    }

    #[test]
    fn right_opens_then_steps_in_and_left_steps_out_then_closes_then_goes_up() {
        let idx = index();
        let sub = first_subgroup();
        let rule = idx.parent_of(&sub).unwrap().to_string();
        let cat = idx.parent_of(&rule).unwrap().to_string();
        let mut st = TreeState::default();
        st.focus_node(&cat);
        assert!(
            st.press_right(&idx).is_empty(),
            "a category opens with no read"
        );
        assert!(st.is_open(&cat));
        st.press_right(&idx);
        let first_child = idx.children(&cat).next().unwrap().id.clone();
        assert_eq!(
            st.cursor,
            Some(TreeRef::Node(first_child.clone())),
            "steps into the first rule"
        );
        // Left on that closed rule goes to its category; Left on the open category closes it.
        st.press_left(&idx);
        assert_eq!(st.cursor, Some(TreeRef::Node(cat.clone())));
        st.press_left(&idx);
        assert!(!st.is_open(&cat));

        // A sub-group's Right asks for one page; Left on a site goes to its sub-group.
        let reqs = st.open_path(&idx, &sub);
        fulfil(&mut st, &reqs[0]);
        st.focus_node(&sub);
        st.press_right(&idx);
        let TreeRef::Site { node, key } = st.cursor.clone().unwrap() else {
            panic!("the cursor steps onto the first site");
        };
        assert_eq!(node, sub);
        assert_eq!(key, st.loaded[&sub].rows[0].site_key);
        st.press_left(&idx);
        assert_eq!(st.cursor, Some(TreeRef::Node(sub)));
    }

    #[test]
    fn enter_on_a_sub_group_whose_page_is_in_flight_moves_in_when_it_arrives() {
        let idx = index();
        let sub = first_subgroup();
        let mut st = TreeState::default();
        st.open_path(&idx, idx.parent_of(&sub).unwrap());
        st.focus_node(&sub);
        let reqs = st.press_enter(&idx);
        assert_eq!(reqs.len(), 1);
        assert_eq!(
            st.cursor,
            Some(TreeRef::Node(sub.clone())),
            "nothing to enter yet"
        );
        fulfil(&mut st, &reqs[0]);
        assert!(
            matches!(st.cursor, Some(TreeRef::Site { .. })),
            "the first site is focused"
        );
        assert_eq!(st.enter_when_loaded, None);
    }

    #[test]
    fn closing_a_node_pulls_the_cursor_up_from_what_it_hid_and_keeps_what_loaded() {
        let idx = index();
        let sub = first_subgroup();
        let rule = idx.parent_of(&sub).unwrap().to_string();
        let mut st = TreeState::default();
        let reqs = st.open_path(&idx, &sub);
        fulfil(&mut st, &reqs[0]);
        let key = st.loaded[&sub].rows[0].site_key.clone();
        st.focus_site(&sub, &key);
        st.close_node(&idx, &rule);
        assert_eq!(st.cursor, Some(TreeRef::Node(rule.clone())));
        assert!(st.loaded.contains_key(&sub), "the page is kept");
        assert!(st.open_node(&idx, &rule).is_empty());
    }

    #[test]
    fn shift_left_closes_everything_and_lands_on_the_category() {
        let idx = index();
        let sub = first_subgroup();
        let cat = idx.path_to(&sub)[0].clone();
        let mut st = TreeState::default();
        let reqs = st.open_path(&idx, &sub);
        fulfil(&mut st, &reqs[0]);
        st.focus_node(&sub);
        st.collapse_all(&idx);
        assert!(st.open.is_empty());
        assert_eq!(st.cursor, Some(TreeRef::Node(cat)));
    }

    #[test]
    fn a_decision_reloads_only_the_open_sub_groups_under_the_node() {
        let idx = index();
        let sub = first_subgroup();
        let rule = idx.parent_of(&sub).unwrap().to_string();
        let mut st = TreeState::default();
        let reqs = st.open_path(&idx, &sub);
        fulfil(&mut st, &reqs[0]);
        let again = st.reload_under(&idx, &rule);
        assert_eq!(again.len(), 1, "the one open sub-group asks again");
        assert_eq!(again[0].node, sub);
        assert_eq!(again[0].cursor, None);
        st.close_node(&idx, &sub);
        let mut st2 = TreeState::default();
        let reqs = st2.open_path(&idx, &sub);
        fulfil(&mut st2, &reqs[0]);
        st2.close_node(&idx, &sub);
        assert!(
            st2.reload_under(&idx, &rule).is_empty(),
            "a closed sub-group asks for nothing"
        );
        assert!(!st2.loaded.contains_key(&sub), "and forgets its stale rows");
    }

    #[test]
    fn only_an_outlier_can_be_ticked_and_ticking_twice_unticks() {
        let outlier = data()
            .rows
            .iter()
            .find(|r| r.markers.contains(&Marker::Outlier))
            .unwrap();
        let plain = data().rows.iter().find(|r| r.markers.is_empty()).unwrap();
        let mut st = TreeState::default();
        assert!(!st.toggle_tick(&plain.site_key, plain));
        assert!(st.ticked.is_empty());
        assert!(st.toggle_tick(&outlier.site_key, outlier));
        assert!(st.ticked.contains(&outlier.site_key));
        assert!(st.toggle_tick(&outlier.site_key, outlier));
        assert!(st.ticked.is_empty());
    }

    #[test]
    fn the_cursor_kind_tells_a_node_a_site_and_an_outlier_apart() {
        use crate::tree::keys::CursorKind;
        let idx = index();
        let st = TreeState::default();
        assert_eq!(st.cursor_kind(), CursorKind::None);
        let mut st = TreeState::default();
        let cat = idx.roots().next().unwrap().id.clone();
        st.focus_node(&cat);
        assert_eq!(st.cursor_kind(), CursorKind::Node);
        // A sub-group that holds an outlier on its first page.
        let (sub, outlier) = data()
            .tree
            .nodes
            .iter()
            .filter(|n| n.level == TreeLevel::Subgroup && n.counts.outliers > 0)
            .find_map(|n| {
                let page = data().sites_page(&n.id, None, SITES_PAGE_SIZE)?;
                let r = page
                    .rows
                    .iter()
                    .find(|r| r.markers.contains(&Marker::Outlier))?;
                Some((n.id.clone(), r.site_key.clone()))
            })
            .expect("an outlier on a first page");
        let reqs = st.open_path(&idx, &sub);
        fulfil(&mut st, &reqs[0]);
        st.focus_site(&sub, &outlier);
        assert_eq!(st.cursor_kind(), CursorKind::OutlierSite);
        let plain = st.loaded[&sub]
            .rows
            .iter()
            .find(|r| !r.markers.contains(&Marker::Outlier))
            .map(|r| r.site_key.clone());
        if let Some(plain) = plain {
            st.focus_site(&sub, &plain);
            assert_eq!(st.cursor_kind(), CursorKind::Site);
        }
    }
}
