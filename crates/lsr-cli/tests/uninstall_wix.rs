//! The Windows installers' uninstall actions (PRD A70 c, docs/install.md
//! § Uninstalling): when `lasair storage uninstall` asks about the model
//! files, and that its failure never fails the uninstall. Reads
//! `packaging/windows/lasair.wxs` and `bundle.wxs`, so it runs on every
//! platform (WiX itself builds only on Windows).

use std::collections::BTreeMap;
use std::path::Path;

fn wxs(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging/windows")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The attributes of every `<tag .../>` or `<tag ...>` element, XML entities
/// decoded. Comments are skipped.
fn elements(xml: &str, tag: &str) -> Vec<BTreeMap<String, String>> {
    let mut text = String::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<!--") {
        text.push_str(&rest[..start]);
        rest = rest[start..]
            .split_once("-->")
            .map_or("", |(_, after)| after);
    }
    text.push_str(rest);

    let open = format!("<{tag}");
    let mut out = vec![];
    let mut rest = text.as_str();
    while let Some(start) = rest.find(&open) {
        rest = &rest[start + open.len()..];
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        let end = rest.find('>').expect("element closes");
        let mut attrs = BTreeMap::new();
        let mut body = rest[..end].trim_end_matches('/');
        while let Some((name, after)) = body.split_once("=\"") {
            let (value, after) = after.split_once('"').expect("attribute closes");
            let value = value
                .replace("&quot;", "\"")
                .replace("&gt;", ">")
                .replace("&lt;", "<")
                .replace("&amp;", "&");
            attrs.insert(name.trim().to_owned(), value);
            body = after;
        }
        out.push(attrs);
        rest = &rest[end..];
    }
    out
}

fn by_id<'a>(
    list: &'a [BTreeMap<String, String>],
    key: &str,
    id: &str,
) -> &'a BTreeMap<String, String> {
    list.iter()
        .find(|e| e.get(key).map(String::as_str) == Some(id))
        .unwrap_or_else(|| panic!("no element with {key}=\"{id}\""))
}

/// Who is watching: the bundle's full UI, or the bare MSI's full UI.
const FULL_UI: &str =
    "(LSRBUNDLEUILEVEL AND LSRBUNDLEUILEVEL >= 4) OR (NOT LSRBUNDLEUILEVEL AND UILevel = 5)";
const UNINSTALL: &str = "REMOVE=\"ALL\" AND NOT UPGRADINGPRODUCTCODE";

#[test]
fn the_msi_asks_only_at_full_ui_and_never_fails_the_uninstall() {
    let msi = wxs("lasair.wxs");
    let custom = elements(&msi, "Custom");
    let ask = by_id(&custom, "Action", "RemoveUserDataAsk");
    let keep = by_id(&custom, "Action", "RemoveUserDataKeep");
    assert_eq!(ask["Condition"], format!("{UNINSTALL} AND ({FULL_UI})"));
    assert_eq!(
        keep["Condition"],
        format!("{UNINSTALL} AND NOT ({FULL_UI})")
    );
    for c in [ask, keep] {
        assert_eq!(c["Before"], "RemoveFiles", "the exe must still be there");
    }

    let actions = elements(&msi, "CustomAction");
    for id in ["RemoveUserDataAsk", "RemoveUserDataKeep"] {
        let a = by_id(&actions, "Id", id);
        assert_eq!(a["Return"], "ignore", "{id}");
        assert_eq!(a["Execute"], "deferred", "{id}");
        assert_eq!(a["Impersonate"], "yes", "{id}: runs as the user");
    }

    let commands = elements(&msi, "SetProperty");
    assert!(by_id(&commands, "Id", "RemoveUserDataAsk")["Value"].ends_with("--models ask"));
    assert!(by_id(&commands, "Id", "RemoveUserDataKeep")["Value"].ends_with("--models keep"));

    let props = elements(&msi, "Property");
    assert_eq!(by_id(&props, "Id", "LSRBUNDLEUILEVEL")["Secure"], "yes");
}

#[test]
fn the_bundle_passes_its_ui_level() {
    let bundle = wxs("bundle.wxs");
    let props = elements(&bundle, "MsiProperty");
    assert_eq!(
        by_id(&props, "Name", "LSRBUNDLEUILEVEL")["Value"],
        "[WixBundleUILevel]"
    );
}
