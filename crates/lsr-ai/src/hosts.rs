//! The LAN and cloud AI Hosts a project names, their tokens, and applying a
//! project's `[ai]` settings to the process (FR-A2, FR-A7, FR-A8).
//!
//! A project lists hosts by name and address (`[[ai.hosts]]` in
//! `project.toml`). Their tokens are secrets, so they never go in a project:
//! they live per user in `hosts.toml`, in the app's settings folder beside
//! `accepted.toml` ([`Store::hosts_path`](crate::models::Store::hosts_path)),
//! keyed by host name, readable by the user only (Unix mode 0600):
//!
//! ```toml
//! format = 1
//!
//! [[host]]
//! name = "lan:studio"
//! token = "…"
//! ```
//!
//! [`apply`] is called when a project opens and whenever its AI settings
//! change: it sets Sovereign mode and the spend cap, and makes the
//! supervisor's remote hosts match the project's (the `local` host stays).
//! Registering a host does not connect to it: the supervisor connects when a
//! node first wants it, so applying never waits on the network.

use std::collections::BTreeMap;
use std::path::Path;

use lsr_format::AiSettings;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

use crate::egress;
use crate::router;
use crate::supervisor::{HostKind, HostSpec, Supervisor};

/// Host tokens by host name, as stored per user.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostTokens {
    pub by_name: BTreeMap<String, String>,
}

impl HostTokens {
    /// Reads `path`; a missing file means no tokens.
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Writes `path` (to a temporary file readable by the user only, then
    /// renamed). Refused in a folder whose app data was removed this session
    /// ([`lsr_core::user_data`]).
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if lsr_core::user_data::was_removed(path) {
            return Err(lsr_core::user_data::removed_error(path).to_string());
        }
        let err = |e: std::io::Error| format!("{}: {e}", path.display());
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(err)?;
        }
        let tmp = path.with_extension("toml.tmp~");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        {
            use std::io::Write;
            let mut f = options.open(&tmp).map_err(err)?;
            f.write_all(self.to_string().as_bytes()).map_err(err)?;
        }
        std::fs::rename(&tmp, path).map_err(err)
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.by_name.get(name).map(String::as_str)
    }

    /// Stores `token` for `name`; an empty token removes it.
    pub fn set(&mut self, name: &str, token: &str) {
        if token.is_empty() {
            self.by_name.remove(name);
        } else {
            self.by_name.insert(name.to_owned(), token.to_owned());
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> Result<Self, String> {
        let doc: DocumentMut = text
            .parse()
            .map_err(|e: toml_edit::TomlError| e.to_string())?;
        let root = doc.as_table();
        if let Some((k, _)) = root.iter().find(|(k, _)| !["format", "host"].contains(k)) {
            return Err(format!("unknown key `{k}`"));
        }
        if root.get("format").and_then(Item::as_integer) != Some(1) {
            return Err("`format` must be 1".into());
        }
        let mut out = Self::default();
        let Some(item) = root.get("host") else {
            return Ok(out);
        };
        let list = item
            .as_array_of_tables()
            .ok_or("`host` must be [[host]] tables")?;
        for (i, t) in list.iter().enumerate() {
            if let Some((k, _)) = t.iter().find(|(k, _)| !["name", "token"].contains(k)) {
                return Err(format!("host {}: unknown key `{k}`", i + 1));
            }
            let get = |k: &str| {
                t.get(k)
                    .and_then(Item::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| format!("host {}: `{k}` must be a string", i + 1))
            };
            out.by_name.insert(get("name")?, get("token")?);
        }
        Ok(out)
    }
}

impl std::fmt::Display for HostTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut doc = DocumentMut::new();
        doc.insert("format", Item::Value(Value::from(1)));
        if let Some((mut key, _)) = doc.as_table_mut().get_key_value_mut("format") {
            key.leaf_decor_mut().set_prefix(
                "# AI Host tokens on this machine (Lasair). Secrets: per user,\n\
                 # never part of a project. See docs/ai-host-protocol.md.\n",
            );
        }
        if !self.by_name.is_empty() {
            let mut list = ArrayOfTables::new();
            for (name, token) in &self.by_name {
                let mut t = Table::new();
                t.insert("name", Item::Value(Value::from(name.as_str())));
                t.insert("token", Item::Value(Value::from(token.as_str())));
                list.push(t);
            }
            doc.insert("host", Item::ArrayOfTables(list));
        }
        f.write_str(&doc.to_string())
    }
}

/// The supervisor entry for a project host.
pub fn spec(name: &str, addr: &str, tokens: &HostTokens) -> HostSpec {
    HostSpec {
        name: name.to_owned(),
        kind: HostKind::Remote {
            addr: addr.to_owned(),
            token: tokens.get(name).map(str::to_owned),
        },
    }
}

/// Makes `sup`'s remote hosts the project's `hosts` (name → address):
/// adds or updates each, removes any other but `local`. Hosts that did not
/// change keep their connection.
pub fn apply_hosts(hosts: &BTreeMap<String, String>, tokens: &HostTokens, sup: &Supervisor) {
    for h in sup.snapshot() {
        if h.name != "local" && !hosts.contains_key(&h.name) {
            sup.remove(&h.name);
        }
    }
    for (name, addr) in hosts {
        sup.set(spec(name, addr, tokens));
    }
}

/// Applies a project's AI settings to the process: Sovereign mode, the
/// spend cap and the hosts. Call [`router::Spend::reset`] as well when a
/// project opens.
pub fn apply(ai: &AiSettings, tokens: &HostTokens, sup: &Supervisor) {
    egress::set_sovereign(ai.sovereign);
    router::spend().set_cap_usd(ai.spend_cap_usd);
    apply_hosts(&ai.hosts, tokens, sup);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::HostCommand;

    fn names(sup: &Supervisor) -> Vec<String> {
        sup.snapshot().into_iter().map(|h| h.name).collect()
    }

    fn hosts(list: &[(&str, &str)]) -> BTreeMap<String, String> {
        list.iter()
            .map(|(n, a)| ((*n).to_owned(), (*a).to_owned()))
            .collect()
    }

    #[test]
    fn project_hosts_replace_the_remote_hosts_and_keep_local() {
        let sup = Supervisor::new();
        sup.set(HostSpec {
            name: "local".into(),
            kind: HostKind::Local(HostCommand {
                program: "lsr-no-such-host".into(),
                args: vec![],
                env: vec![],
            }),
        });
        sup.set(spec("lan:old", "10.0.0.9:7420", &HostTokens::default()));
        let mut tokens = HostTokens::default();
        tokens.set("cloud:runpod", "s3cret");
        let project = hosts(&[
            ("lan:studio", "192.168.1.20:7420"),
            ("cloud:runpod", "pod.example.net:7420"),
        ]);
        apply_hosts(&project, &tokens, &sup);
        assert_eq!(names(&sup), ["cloud:runpod", "lan:studio", "local"]);
        // Registering does not connect: nothing wanted them yet.
        assert!(
            sup.snapshot()
                .iter()
                .all(|h| h.state == crate::supervisor::HostState::Idle)
        );
        assert_eq!(
            spec("cloud:runpod", "pod.example.net:7420", &tokens).kind,
            HostKind::Remote {
                addr: "pod.example.net:7420".into(),
                token: Some("s3cret".into())
            }
        );
        assert_eq!(
            spec("lan:studio", "x:1", &tokens).kind,
            HostKind::Remote {
                addr: "x:1".into(),
                token: None
            }
        );
        apply_hosts(&BTreeMap::new(), &tokens, &sup);
        assert_eq!(names(&sup), ["local"]);
    }

    // Touches the process-wide Sovereign flag and spend meter.
    #[test]
    fn applying_a_project_sets_sovereign_mode_and_the_spend_cap() {
        let _flags = egress::TEST_FLAGS.lock().unwrap_or_else(|e| e.into_inner());
        let sup = Supervisor::new();
        let ai = AiSettings {
            sovereign: false,
            spend_cap_usd: 2.5,
            hosts: hosts(&[("cloud:decart", "api.example.com:443")]),
            ..AiSettings::default()
        };
        apply(&ai, &HostTokens::default(), &sup);
        assert!(!egress::sovereign());
        assert!((router::spend().cap_usd() - 2.5).abs() < 1e-9);
        assert_eq!(names(&sup), ["cloud:decart"]);
        apply(&AiSettings::default(), &HostTokens::default(), &sup);
        assert!(egress::sovereign());
        assert_eq!(router::spend().cap_usd(), 0.0);
        assert!(names(&sup).is_empty());
    }

    #[test]
    fn tokens_round_trip_through_a_private_file() {
        let dir = std::env::temp_dir().join(format!("lsr-ai-hosts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("hosts.toml");
        assert_eq!(HostTokens::load(&path).unwrap(), HostTokens::default());
        let mut t = HostTokens::default();
        t.set("lan:studio", "abc");
        t.set("cloud:runpod", "def");
        t.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.find("cloud:runpod").unwrap() < text.find("lan:studio").unwrap(),
            "{text}"
        );
        assert_eq!(HostTokens::load(&path).unwrap(), t);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        t.set("lan:studio", "");
        assert_eq!(t.get("lan:studio"), None);
        assert!(HostTokens::from_str("format = 1\n[[host]]\nname = \"a\"\nkey = 1\n").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
