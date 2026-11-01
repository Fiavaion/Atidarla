//! Cloud targets (A59): a self-hosted Runpod pod running our AI Host
//! container ([`runpod`]) and Decart's hosted real-time video model
//! ([`register_decart`]). See `docs/cloud.md`.
//!
//! Both are `cloud:<name>` hosts in the [`Supervisor`], so the router keeps
//! them out of Sovereign mode and charges them to the project's spend meter.
//! Keys come from the environment or a per-user secrets file in the app's
//! settings folder ([`secrets_path`]), never from a project.

pub mod runpod;

use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, Table, value};

use crate::client::HostCommand;
use crate::models::Store;
use crate::supervisor::{HostKind, HostSpec, Supervisor, default_local_command};

/// The supervisor's name for the Decart host.
pub const DECART_HOST: &str = "cloud:decart";

/// Decart Lucy 2.6 realtime, standard tier ($0.02 per billed second).
pub const DECART_USD_PER_HOUR: f64 = 72.0;

/// The per-user secrets file: `secrets.toml` in the app's settings folder
/// (`~/Library/Caches/Lasair/secrets.toml` on macOS), never beside a model
/// folder chosen with `LSR_MODELS_DIR` ([`crate::models::settings_dir`]).
pub fn secrets_path() -> PathBuf {
    Store::default().secrets_path()
}

/// Reads `section.key` (dotted path) from a secrets file.
pub fn read_secret(path: &Path, keys: &[&str]) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let doc: DocumentMut = text.parse().ok()?;
    let mut item: &Item = doc.as_item();
    for k in keys {
        item = item.get(k)?;
    }
    item.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Writes `section.key` (dotted path) into a secrets file, creating it
/// readable by this user only. Refused in a folder whose app data was
/// removed this session ([`lsr_core::user_data`]).
pub fn write_secret(path: &Path, keys: &[&str], secret: &str) -> std::io::Result<()> {
    if lsr_core::user_data::was_removed(path) {
        return Err(lsr_core::user_data::removed_error(path));
    }
    let (last, tables) = keys.split_last().expect("at least one key");
    let mut doc: DocumentMut = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| t.parse().ok())
        .unwrap_or_default();
    let mut table: &mut Table = doc.as_table_mut();
    for k in tables {
        let entry = table.entry(k).or_insert_with(|| {
            let mut t = Table::new();
            t.set_implicit(true);
            Item::Table(t)
        });
        table = entry
            .as_table_mut()
            .ok_or_else(|| std::io::Error::other(format!("{k} is not a table")))?;
    }
    table.insert(last, value(secret));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, doc.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Registers `cloud:decart`: a local AI Host process whose `video.v2v`
/// adapter streams to Decart with the user's `DECART_API_KEY` (inherited
/// from this process's environment). The supervisor runs it like `local`,
/// but it is a cloud target: the router skips it in Sovereign mode, the
/// supervisor refuses to start it then, and it is charged per billed second
/// (the host reports `cost_s` with each frame) at `rate_usd_per_hour`.
pub fn register_decart(supervisor: &Supervisor, rate_usd_per_hour: f64) {
    register_decart_with(supervisor, default_local_command(), rate_usd_per_hour);
}

/// [`register_decart`] with an explicit host command (tests, packaging).
pub fn register_decart_with(supervisor: &Supervisor, command: HostCommand, rate_usd_per_hour: f64) {
    crate::router::spend().set_rate(DECART_HOST, rate_usd_per_hour);
    supervisor.set(HostSpec {
        name: DECART_HOST.into(),
        kind: HostKind::Local(command),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // Touches the process-wide Sovereign flag.
    #[test]
    fn decart_is_a_cloud_target_that_sovereign_mode_refuses() {
        use crate::router::{Need, Route, Skip, choose};
        use crate::supervisor::HostState;
        let _flags = crate::egress::TEST_FLAGS
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::egress::set_sovereign(true);
        let sup = Supervisor::new();
        // A program that does not exist: Sovereign mode must refuse before
        // trying to start it, so the error names Sovereign mode.
        let cmd = HostCommand {
            program: "/nonexistent/lsr-ai-host".into(),
            args: vec![],
            env: vec![],
        };
        register_decart_with(&sup, cmd, DECART_USD_PER_HOUR);
        assert_eq!(
            crate::router::spend().rate(DECART_HOST),
            Some(DECART_USD_PER_HOUR)
        );
        sup.want(DECART_HOST);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let state = loop {
            let s = sup.host(DECART_HOST).unwrap().state;
            if matches!(s, HostState::Failed(_)) || std::time::Instant::now() > deadline {
                break s;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        match state {
            HostState::Failed(why) => assert!(why.contains("Sovereign"), "{why}"),
            other => panic!("expected a Sovereign refusal, got {other:?}"),
        }
        assert_eq!(sup.host(DECART_HOST).unwrap().pid, None);
        // The router skips it too.
        let route = Route::parse("cloud:decart").unwrap();
        let need = Need {
            adapter: "video.v2v",
            realtime_vendor: None,
        };
        let why = choose(&route, need, &sup.snapshot(), &[]).unwrap_err();
        assert_eq!(why[0].1, Skip::Sovereign);
    }

    #[test]
    fn secrets_round_trip_without_clobbering_other_keys() {
        let dir = std::env::temp_dir().join(format!("lsr-secrets-{}", std::process::id()));
        let path = dir.join("secrets.toml");
        let _ = std::fs::remove_file(&path);
        assert_eq!(read_secret(&path, &["runpod", "api_key"]), None);
        write_secret(&path, &["runpod", "api_key"], "rp-key").unwrap();
        write_secret(&path, &["runpod", "tokens", "studio"], "tok").unwrap();
        assert_eq!(
            read_secret(&path, &["runpod", "api_key"]).as_deref(),
            Some("rp-key")
        );
        assert_eq!(
            read_secret(&path, &["runpod", "tokens", "studio"]).as_deref(),
            Some("tok")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
