//! Which network Python may run: an allow-list by content
//! (`docs/python-ops.md`, PRD A67).
//!
//! Network Python is a Python-tier parameter expression (anything outside
//! the native subset, PRD A22) or a Script node's code. Each is an [`Item`]
//! keyed by its node (in the engine's flattened network, so a Pack's
//! promoted parameter is its inner target) and its full text. An item runs
//! only while exactly that content is on the list: a changed expression or
//! script, a pasted or merged one, or one an undo brings back is a new item
//! and is blocked on its own; items already allowed keep running.
//!
//! The list is process-wide, like the project: one is open at a time.
//! Opening a project clears it; the app's Allow dialog adds what it lists;
//! Python the user types in the parameter panel is added as it is typed;
//! headless `--allow-python-ops` adds the project's items as opened. The
//! expression worker and the Script worker check again before running.
//! Readers check it when their configuration or [`generation`] changes,
//! never per frame.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{PoisonError, RwLock};

use atd_core::graph::{Network, NodeId};
use atd_core::registry::Registry;

/// One piece of network Python, by content.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Item {
    /// A Python-tier expression on `node`'s `param`.
    Expr {
        node: NodeId,
        param: String,
        source: String,
    },
    /// A Script node's `code`.
    Script { node: NodeId, code: String },
}

impl Item {
    /// How the Allow dialog lists it: the node by name and the full text.
    pub fn describe(&self, net: &Network) -> String {
        let name = |id: &NodeId| net.node(*id).map_or("?", |n| n.name.as_str()).to_owned();
        match self {
            Self::Expr {
                node,
                param,
                source,
            } => format!("Python expression {}.{param} = {source}", name(node)),
            Self::Script { node, code } => format!("Script node {}:\n{code}", name(node)),
        }
    }
}

static LIST: RwLock<Vec<Item>> = RwLock::new(Vec::new());
/// Bumped by every change to the list.
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn changed() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
}

/// Bumped by every change to the list: readers check again when it moves.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

/// Empties the list (another project opened).
pub fn clear() {
    let mut list = LIST.write().unwrap_or_else(PoisonError::into_inner);
    if !list.is_empty() {
        list.clear();
        changed();
    }
}

/// Adds `items` to the list.
pub fn allow(items: impl IntoIterator<Item = Item>) {
    let mut list = LIST.write().unwrap_or_else(PoisonError::into_inner);
    let before = list.len();
    for item in items {
        if !list.contains(&item) {
            list.push(item);
        }
    }
    if list.len() != before {
        changed();
    }
}

/// Adds every item of `net` (headless `--allow-python-ops`).
pub fn allow_network(net: &Network, registry: &Registry) {
    allow(items(net, registry));
}

/// Whether the expression `source` on `node`'s `param` may run.
pub fn allows_expr(node: NodeId, param: &str, source: &str) -> bool {
    LIST.read()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .any(|i| {
            matches!(i, Item::Expr { node: n, param: p, source: s }
                if *n == node && p == param && s == source)
        })
}

/// Whether Script node `node` may run `code`.
pub fn allows_script(node: NodeId, code: &str) -> bool {
    LIST.read()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .any(|i| matches!(i, Item::Script { node: n, code: c } if *n == node && c == code))
}

/// Whether `item` is on the list.
pub fn allows(item: &Item) -> bool {
    LIST.read()
        .unwrap_or_else(PoisonError::into_inner)
        .contains(item)
}

/// Every piece of network Python in `net`, by node then parameter.
pub fn items(net: &Network, registry: &Registry) -> Vec<Item> {
    let mut nodes: Vec<&atd_core::graph::Node> = net.nodes().collect();
    nodes.sort_by_key(|n| n.id);
    nodes
        .into_iter()
        .flat_map(|n| node_items(n, registry))
        .collect()
}

/// The network Python of one node: its Script code (as it runs: the
/// stored value or the default) and its Python-tier expressions.
pub fn node_items(n: &atd_core::graph::Node, registry: &Registry) -> Vec<Item> {
    let mut out = Vec::new();
    if n.type_id == crate::script::TYPE_ID {
        let default = registry
            .get(&n.type_id)
            .and_then(|s| s.params.iter().find(|p| p.name == "code"))
            .map(|p| &p.default);
        let code = n
            .params
            .get("code")
            .map(|p| &p.value)
            .or(default)
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        out.push(Item::Script {
            node: n.id,
            code: code.to_owned(),
        });
    }
    for (param, p) in &n.params {
        if let Some(source) = p.expr.as_deref()
            && atd_core::expr::Expr::parse(source).is_err()
        {
            out.push(Item::Expr {
                node: n.id,
                param: param.clone(),
                source: source.to_owned(),
            });
        }
    }
    out
}

/// The items of `net` not on the list: what the Allow dialog asks about.
pub fn blocked(net: &Network, registry: &Registry) -> Vec<Item> {
    let mut items = items(net, registry);
    items.retain(|i| !allows(i));
    items
}
