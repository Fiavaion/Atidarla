//! Show Lock (FR-G3) and the agent's permission scopes.
//!
//! During a performance an agent may only move whitelisted parameters (or
//! the promoted parameters of whitelisted Packs) within their declared
//! ranges; every graph edit is refused. Settings live in the project's
//! `agent.toml` (text, sorted), so a show carries its own lock:
//!
//! ```toml
//! [show_lock]
//! engaged = true
//! stage = true            # launches and the crossfader are allowed
//!
//! [[show_lock.allow]]
//! node = "level1"
//! param = "brightness"
//! min = 0.2
//! max = 1.0
//!
//! [[show_lock.allow]]
//! pack = "colour-grade"   # any promoted parameter of its instances
//! ```

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// What an agent session may do. Read tools are always allowed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Stage graph changes for review.
    Edit,
    /// Accept staged changes without a person (headless `serve --auto-accept`).
    Accept,
    /// Launch clips, move the crossfader.
    Stage,
    /// Write kernel files into the project.
    Files,
    /// Run Python (`python.exec`): off unless the user grants it per session.
    #[serde(rename = "python.exec")]
    PythonExec,
}

impl Scope {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "edit" => Self::Edit,
            "accept" => Self::Accept,
            "stage" => Self::Stage,
            "files" => Self::Files,
            "python.exec" => Self::PythonExec,
            _ => return None,
        })
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Edit => "edit",
            Self::Accept => "accept",
            Self::Stage => "stage",
            Self::Files => "files",
            Self::PythonExec => "python.exec",
        }
    }

    /// The scopes a session has unless told otherwise: everything but
    /// accepting on its own and running Python.
    pub fn defaults() -> BTreeSet<Scope> {
        [Self::Edit, Self::Stage, Self::Files].into_iter().collect()
    }
}

/// One whitelisted control.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Allow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShowLock {
    #[serde(default)]
    pub engaged: bool,
    /// Stage launches and the crossfader are allowed while engaged.
    #[serde(default)]
    pub stage: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<Allow>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentFile {
    #[serde(default)]
    show_lock: ShowLock,
}

/// Why the lock refused something.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Refusal {
    #[error("Show Lock is engaged: graph edits are refused during the show")]
    GraphEdit,
    #[error("Show Lock is engaged: {node}.{param} is not on the show's whitelist")]
    NotAllowed { node: String, param: String },
    #[error(
        "Show Lock is engaged: {node}.{param} may move between {min} and {max}, not to {value}"
    )]
    OutOfRange {
        node: String,
        param: String,
        value: f64,
        min: f64,
        max: f64,
    },
    #[error("Show Lock is engaged: the Stage is not on the show's whitelist")]
    Stage,
    #[error("this session does not have the `{0}` scope")]
    Scope(&'static str),
}

impl ShowLock {
    /// Reads `agent.toml` in a project folder (absent: no lock).
    pub fn load(project: &Path) -> Result<Self, String> {
        let path = project.join("agent.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => toml_edit::de::from_str::<AgentFile>(&text)
                .map(|f| f.show_lock)
                .map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// The canonical text of `agent.toml`.
    pub fn to_toml(&self) -> String {
        let mut s = String::from("# Atidarla agent settings (Show Lock, FR-G3).\n[show_lock]\n");
        let _ = writeln!(s, "engaged = {}", self.engaged);
        let _ = writeln!(s, "stage = {}", self.stage);
        let mut allow = self.allow.clone();
        allow.sort_by(|a, b| (&a.node, &a.param, &a.pack).cmp(&(&b.node, &b.param, &b.pack)));
        for a in &allow {
            s.push_str("\n[[show_lock.allow]]\n");
            for (k, v) in [("node", &a.node), ("param", &a.param), ("pack", &a.pack)] {
                if let Some(v) = v {
                    let _ = writeln!(s, "{k} = {}", toml_edit::Value::from(v.as_str()));
                }
            }
            for (k, v) in [("min", a.min), ("max", a.max)] {
                if let Some(v) = v {
                    let _ = writeln!(s, "{k} = {}", toml_edit::Value::from(v));
                }
            }
        }
        s
    }

    /// The whitelist entry covering `node.param`, engaged or not (`pack`:
    /// the Pack `node` is an instance of). In-show cues move only these.
    pub fn rule(&self, node: &str, pack: Option<&str>, param: &str) -> Option<&Allow> {
        self.allow.iter().find(|a| {
            let by_node =
                a.node.as_deref() == Some(node) && a.param.as_deref().is_none_or(|p| p == param);
            let by_pack = a.pack.is_some() && a.pack.as_deref() == pack;
            by_node || by_pack
        })
    }

    /// Whether moving `node.param` to `value` (numeric, when it is) is
    /// allowed. `pack` is the Pack `node` is an instance of, if any.
    pub fn check_param(
        &self,
        node: &str,
        pack: Option<&str>,
        param: &str,
        value: Option<f64>,
    ) -> Result<(), Refusal> {
        if !self.engaged {
            return Ok(());
        }
        let Some(rule) = self.rule(node, pack, param) else {
            return Err(Refusal::NotAllowed {
                node: node.to_owned(),
                param: param.to_owned(),
            });
        };
        if let Some(v) = value {
            let (min, max) = (rule.min.unwrap_or(f64::MIN), rule.max.unwrap_or(f64::MAX));
            if !(min..=max).contains(&v) || !v.is_finite() {
                return Err(Refusal::OutOfRange {
                    node: node.to_owned(),
                    param: param.to_owned(),
                    value: v,
                    min,
                    max,
                });
            }
        }
        Ok(())
    }

    pub fn check_graph_edit(&self) -> Result<(), Refusal> {
        if self.engaged {
            Err(Refusal::GraphEdit)
        } else {
            Ok(())
        }
    }

    pub fn check_stage(&self) -> Result<(), Refusal> {
        if self.engaged && !self.stage {
            Err(Refusal::Stage)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock() -> ShowLock {
        ShowLock {
            engaged: true,
            stage: false,
            allow: vec![
                Allow {
                    node: Some("level1".into()),
                    param: Some("brightness".into()),
                    pack: None,
                    min: Some(0.2),
                    max: Some(1.0),
                },
                Allow {
                    node: None,
                    param: None,
                    pack: Some("colour-grade".into()),
                    min: None,
                    max: None,
                },
            ],
        }
    }

    #[test]
    fn the_whitelist_and_ranges_decide() {
        let l = lock();
        assert!(
            l.check_param("level1", None, "brightness", Some(0.5))
                .is_ok()
        );
        assert!(matches!(
            l.check_param("level1", None, "brightness", Some(1.5)),
            Err(Refusal::OutOfRange { .. })
        ));
        assert!(matches!(
            l.check_param("level1", None, "contrast", Some(1.0)),
            Err(Refusal::NotAllowed { .. })
        ));
        assert!(
            l.check_param("grade2", Some("colour-grade"), "warmth", Some(9.0))
                .is_ok()
        );
        assert_eq!(l.check_graph_edit(), Err(Refusal::GraphEdit));
        assert_eq!(l.check_stage(), Err(Refusal::Stage));
        let off = ShowLock::default();
        assert!(off.check_param("x", None, "y", Some(1e9)).is_ok());
        assert!(off.check_graph_edit().is_ok());
    }

    #[test]
    fn agent_toml_round_trips_in_canonical_form() {
        let dir = std::env::temp_dir().join(format!("atd-agent-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut l = lock();
        l.allow.reverse();
        std::fs::write(dir.join("agent.toml"), l.to_toml()).unwrap();
        let back = ShowLock::load(&dir).unwrap();
        assert_eq!(back.to_toml(), l.to_toml());
        assert_eq!(back.allow.len(), 2);
        assert!(back.engaged);
        std::fs::write(dir.join("agent.toml"), "[show_lock]\nbogus = 1\n").unwrap();
        assert!(ShowLock::load(&dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(ShowLock::load(&dir).unwrap(), ShowLock::default());
    }
}
