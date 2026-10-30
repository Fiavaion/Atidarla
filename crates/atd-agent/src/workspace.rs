//! What the agent works on: a network, its registry and an engine to cook
//! it. The app (over its live engine) and `atidarla serve` (headless)
//! implement it; tests use a network-only fake.

use std::path::Path;

use atd_core::Registry;
use atd_core::graph::{Edit, Network, NodeId};

pub trait Workspace {
    fn network(&self) -> &Network;
    fn registry(&self) -> &Registry;
    /// Applies `edits` as one undo step.
    fn apply(&mut self, label: &str, edits: Vec<Edit>) -> Result<(), String>;
    /// Cooks the network once and returns every node error.
    fn cook_errors(&mut self) -> Vec<(NodeId, String)>;
    /// The node's status line, if it has one.
    fn status(&self, _node: NodeId) -> Option<String> {
        None
    }
    /// A PNG of `node`'s image, at most `max` pixels on its long side.
    fn screenshot(&mut self, _node: NodeId, _max: u32) -> Result<Vec<u8>, String> {
        Err("this workspace cannot render images".into())
    }
    /// Cooks `frames` frames and returns each node's mean cook time (ms).
    fn bench(&mut self, _frames: u32) -> Result<Vec<(NodeId, f64)>, String> {
        Err("this workspace cannot benchmark".into())
    }
    /// The project folder, when there is one (kernel files are written there).
    fn project_dir(&self) -> Option<&Path> {
        None
    }
    /// Called after the agent wrote project files (kernels): register the
    /// project's operators again.
    fn files_changed(&mut self) -> Result<(), String> {
        Ok(())
    }
    /// Runs Python in the project's interpreter (scope `python.exec`).
    fn python_exec(&mut self, _code: &str) -> Result<String, String> {
        Err("Python is not available in this workspace".into())
    }
}

/// A workspace with no engine: a network and a registry. Edits apply;
/// cooking reports nothing. For tests and dry runs.
#[derive(Debug, Clone)]
pub struct NetworkWorkspace {
    pub network: Network,
    pub registry: Registry,
    /// Applied batches, by label (one entry per undo step).
    pub history: Vec<String>,
}

impl NetworkWorkspace {
    pub fn new(registry: Registry) -> Self {
        Self {
            network: Network::default(),
            registry,
            history: Vec::new(),
        }
    }
}

impl Workspace for NetworkWorkspace {
    fn network(&self) -> &Network {
        &self.network
    }

    fn registry(&self) -> &Registry {
        &self.registry
    }

    fn apply(&mut self, label: &str, edits: Vec<Edit>) -> Result<(), String> {
        let mut next = self.network.clone();
        for e in edits {
            next.apply(&self.registry, e).map_err(|e| e.to_string())?;
        }
        self.network = next;
        self.history.push(label.to_owned());
        Ok(())
    }

    fn cook_errors(&mut self) -> Vec<(NodeId, String)> {
        Vec::new()
    }
}
