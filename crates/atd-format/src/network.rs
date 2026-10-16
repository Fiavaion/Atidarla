//! One network as a TOML file (`networks/<id>.toml`, PRD §5.5).
//!
//! ```toml
//! format = 1
//! next_id = 4
//!
//! [[node]]
//! id = 0
//! name = "lfo1"
//! type = "sig.lfo"
//! pos = [0.0, 0.0]
//!
//! [[node]]
//! id = 1
//! name = "level1"
//! type = "img.level"
//! pos = [240.0, 0.0]
//! inputs.in = { node = 2, output = "out" }
//! params.brightness = { value = 1.0, bind = { node = 0, channel = "lfo" } }
//! params.gamma = 1.2
//! ```
//!
//! - Nodes are sorted by ID, and inputs and parameters by name, so the same
//!   network always gives the same bytes and a change touches only the
//!   lines it changes (CLAUDE.md rule 8).
//! - A node lists its own inputs (`inputs.<socket>`): each input holds at
//!   most one wire, so a rewire is a one-line diff on the node it affects.
//! - Only parameters that differ from the default are written. A constant
//!   is a bare value; a bound or expression parameter is an inline table.
//! - Wires and bindings refer to stable IDs, never to names, so renaming a
//!   node changes one line.
//! - Text with line breaks (shader code, Text lines) is written as a
//!   `'''` block, so it reads and diffs line by line.
//!
//! Reading accepts any equivalent TOML (standard tables, other key orders,
//! a missing `next_id` or `pos`) and rejects unknown keys, so a typo in a
//! hand-edited file is an error rather than silently ignored.

use std::collections::BTreeMap;

use atd_core::graph::Network;
use serde_json::{Map, Value as Json, json};
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, TableLike, Value};

use crate::{FORMAT, FormatError};

const HEADER: &str =
    "# Atidarla network. Node IDs are stable: inputs and bindings refer to them.\n";

/// The network as TOML text. The same network always gives the same text.
pub fn network_to_string(net: &Network) -> Result<String, FormatError> {
    let repr = serde_json::to_value(net).map_err(|e| invalid(e.to_string()))?;
    let next_id = repr["next_id"].as_u64().unwrap_or(0);

    // Inputs by destination node, sorted by socket.
    let mut inputs: BTreeMap<u64, BTreeMap<String, (u64, String)>> = BTreeMap::new();
    for wire in repr["wires"].as_array().into_iter().flatten() {
        let to = &wire["to"];
        let from = &wire["from"];
        inputs
            .entry(to["node"].as_u64().unwrap_or(0))
            .or_default()
            .insert(
                text(&to["socket"]).to_owned(),
                (
                    from["node"].as_u64().unwrap_or(0),
                    text(&from["socket"]).to_owned(),
                ),
            );
    }

    let mut doc = DocumentMut::new();
    let mut format = Item::Value(Value::from(FORMAT));
    if let Some(v) = format.as_value_mut() {
        v.decor_mut().set_prefix(" ");
    }
    doc.insert("format", format);
    if let Some((mut key, _)) = doc.as_table_mut().get_key_value_mut("format") {
        key.leaf_decor_mut().set_prefix(HEADER);
    }
    doc.insert("next_id", Item::Value(int(next_id)?));

    let mut nodes = ArrayOfTables::new();
    for node in repr["nodes"].as_array().into_iter().flatten() {
        let id = node["id"].as_u64().unwrap_or(0);
        let mut t = Table::new();
        t.insert("id", Item::Value(int(id)?));
        t.insert("name", Item::Value(string(text(&node["name"]))));
        t.insert("type", Item::Value(string(text(&node["type_id"]))));
        t.insert("pos", Item::Value(Value::Array(pos(&node["pos"]))));
        if let Some(sockets) = inputs.get(&id) {
            let mut it = Table::new();
            it.set_dotted(true);
            for (socket, (from, output)) in sockets {
                let mut wire = InlineTable::new();
                wire.insert("node", int(*from)?);
                wire.insert("output", string(output));
                it.insert(socket, Item::Value(Value::InlineTable(wire)));
            }
            t.insert("inputs", Item::Table(it));
        }
        if let Some(params) = node["params"].as_object().filter(|p| !p.is_empty()) {
            let mut pt = Table::new();
            pt.set_dotted(true);
            for (name, param) in params {
                let v = param_to_toml(param)
                    .map_err(|e| invalid(format!("node {id}, parameter `{name}`: {e}")))?;
                pt.insert(name, Item::Value(v));
            }
            t.insert("params", Item::Table(pt));
        }
        nodes.push(t);
    }
    if !nodes.is_empty() {
        doc.insert("node", Item::ArrayOfTables(nodes));
    }

    let mut comments = ArrayOfTables::new();
    for c in repr["comments"].as_array().into_iter().flatten() {
        let mut t = Table::new();
        t.insert("id", Item::Value(int(c["id"].as_u64().unwrap_or(0))?));
        t.insert("pos", Item::Value(Value::Array(pos(&c["pos"]))));
        t.insert("size", Item::Value(Value::Array(pos(&c["size"]))));
        t.insert("text", Item::Value(string(text(&c["text"]))));
        comments.push(t);
    }
    if !comments.is_empty() {
        doc.insert("comment", Item::ArrayOfTables(comments));
    }
    Ok(doc.to_string())
}

/// Parses a network file. Structural checks only (IDs, one wire per input,
/// wires between existing nodes); check it against a registry with
/// [`Network::validated`] before use.
pub fn network_from_str(source: &str) -> Result<Network, FormatError> {
    let doc: DocumentMut = source
        .parse()
        .map_err(|e: toml_edit::TomlError| invalid(e.to_string().trim_end().to_owned()))?;
    let root = doc.as_table();
    only_keys(root, &["format", "next_id", "node", "comment"], "the file")?;
    check_format(root)?;

    let mut nodes = Vec::new();
    let mut wires = Vec::new();
    let mut max_id = None;
    for (i, node) in tables(root.get("node"), "node")?.into_iter().enumerate() {
        let id = required_u64(node, "id", &format!("node #{}", i + 1))?;
        let at = || format!("node {id}");
        only_keys(
            node,
            &["id", "name", "type", "pos", "inputs", "params"],
            &at(),
        )?;
        max_id = max_id.max(Some(id));
        let mut params = Map::new();
        if let Some(item) = node.get("params") {
            for (name, item) in table_like(item, &format!("{}: params", at()))?.iter() {
                let p = param_from_toml(item)
                    .map_err(|e| invalid(format!("{}, parameter `{name}`: {e}", at())))?;
                params.insert(name.to_owned(), p);
            }
        }
        if let Some(item) = node.get("inputs") {
            for (socket, item) in table_like(item, &format!("{}: inputs", at()))?.iter() {
                let at = format!("{}, input `{socket}`", at());
                let wire = table_like(item, &at)?;
                only_keys(wire, &["node", "output"], &at)?;
                wires.push(json!({
                    "from": {
                        "node": required_u64(wire, "node", &at)?,
                        "socket": required_str(wire, "output", &at)?,
                    },
                    "to": { "node": id, "socket": socket },
                }));
            }
        }
        nodes.push(json!({
            "id": id,
            "type_id": required_str(node, "type", &at())?,
            "name": required_str(node, "name", &at())?,
            "pos": match node.get("pos") {
                Some(item) => pair(item, &format!("{}: pos", at()))?,
                None => json!([0.0, 0.0]),
            },
            "params": params,
        }));
    }

    let mut comments = Vec::new();
    for (i, c) in tables(root.get("comment"), "comment")?
        .into_iter()
        .enumerate()
    {
        let at = format!("comment #{}", i + 1);
        only_keys(c, &["id", "pos", "size", "text"], &at)?;
        let id = required_u64(c, "id", &at)?;
        max_id = max_id.max(Some(id));
        comments.push(json!({
            "id": id,
            "text": c.get("text").map_or(Ok(""), |_| required_str(c, "text", &at))?,
            "pos": match c.get("pos") {
                Some(item) => pair(item, &format!("{at}: pos"))?,
                None => json!([0.0, 0.0]),
            },
            "size": match c.get("size") {
                Some(item) => pair(item, &format!("{at}: size"))?,
                None => json!([200.0, 100.0]),
            },
        }));
    }

    // A hand-written file may leave out next_id: the next free ID.
    let next_id = match root.get("next_id") {
        Some(_) => required_u64(root, "next_id", "the file")?,
        None => max_id.map_or(0, |m| m + 1),
    };
    let repr = json!({
        "next_id": next_id,
        "nodes": nodes,
        "wires": wires,
        "comments": comments,
    });
    serde_json::from_value(repr).map_err(|e| invalid(e.to_string()))
}

pub(crate) fn check_format(root: &Table) -> Result<(), FormatError> {
    match root.get("format") {
        None => Err(invalid("missing `format`".into())),
        Some(item) => match item.as_integer() {
            Some(v) if v == FORMAT => Ok(()),
            Some(v) if v > FORMAT => Err(invalid(format!(
                "format {v} was written by a newer Atidarla (this one reads format {FORMAT})"
            ))),
            _ => Err(invalid(format!("`format` must be {FORMAT}"))),
        },
    }
}

fn invalid(message: String) -> FormatError {
    FormatError::Invalid(message)
}

fn text(v: &Json) -> &str {
    v.as_str().unwrap_or_default()
}

fn int(v: u64) -> Result<Value, FormatError> {
    i64::try_from(v)
        .map(Value::from)
        .map_err(|_| invalid(format!("{v} is too large")))
}

/// A canvas position. Positions are `f32`; written via their shortest
/// decimal form, so `0.1` stays `0.1` rather than `0.10000000149011612`.
fn pos(v: &Json) -> Array {
    let mut a = Array::new();
    for x in v.as_array().into_iter().flatten() {
        let f = x.as_f64().unwrap_or(0.0) as f32;
        a.push(format!("{f:?}").parse::<f64>().unwrap_or(0.0));
    }
    a
}

/// A string value: a `'''` block when it has line breaks (if that
/// represents it exactly), otherwise a normal string.
pub(crate) fn string(s: &str) -> Value {
    if s.contains('\n')
        && let Ok(v) = format!("'''\n{s}'''").parse::<Value>()
        && v.as_str() == Some(s)
    {
        return v;
    }
    Value::from(s)
}

fn param_to_toml(param: &Json) -> Result<Value, String> {
    let value = &param["value"];
    let expr = param.get("expr").and_then(Json::as_str);
    let bind = param.get("bind").filter(|b| !b.is_null());
    if expr.is_none() && bind.is_none() && !value.is_object() {
        return json_to_toml(value);
    }
    let mut t = InlineTable::new();
    t.insert("value", json_to_toml(value)?);
    if let Some(expr) = expr {
        t.insert("expr", string(expr));
    }
    if let Some(bind) = bind {
        let mut b = InlineTable::new();
        b.insert(
            "node",
            Value::from(bind["node"].as_u64().unwrap_or(0) as i64),
        );
        if let Some(c) = bind.get("channel").and_then(Json::as_str) {
            b.insert("channel", string(c));
        }
        t.insert("bind", Value::InlineTable(b));
    }
    Ok(Value::InlineTable(t))
}

fn param_from_toml(item: &Item) -> Result<Json, String> {
    let Some(t) = item.as_table_like() else {
        let v = item.as_value().ok_or("not a value")?;
        return Ok(json!({ "value": toml_to_json(v)? }));
    };
    for (k, _) in t.iter() {
        if !["value", "expr", "bind"].contains(&k) {
            return Err(format!(
                "unknown key `{k}` (a table needs `value`, and may have `expr` and `bind`)"
            ));
        }
    }
    let value = t
        .get("value")
        .and_then(Item::as_value)
        .ok_or("missing `value`")?;
    let mut out = Map::new();
    out.insert("value".into(), toml_to_json(value)?);
    if let Some(e) = t.get("expr") {
        out.insert(
            "expr".into(),
            Json::from(e.as_str().ok_or("`expr` must be a string")?),
        );
    }
    if let Some(b) = t.get("bind") {
        let b = b
            .as_table_like()
            .ok_or("`bind` must be a table: { node = <id>, channel = \"name\" }")?;
        let node = b
            .get("node")
            .and_then(Item::as_integer)
            .filter(|n| *n >= 0)
            .ok_or("`bind` needs `node = <id>`")?;
        let mut bind = Map::new();
        bind.insert("node".into(), Json::from(node));
        if let Some(c) = b.get("channel") {
            bind.insert(
                "channel".into(),
                Json::from(c.as_str().ok_or("`channel` must be a string")?),
            );
        }
        out.insert("bind".into(), Json::Object(bind));
    }
    Ok(Json::Object(out))
}

fn json_to_toml(v: &Json) -> Result<Value, String> {
    Ok(match v {
        Json::Null => return Err("null cannot be written".into()),
        Json::Bool(b) => Value::from(*b),
        Json::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => Value::from(i),
            (None, Some(f)) if n.is_f64() => Value::from(f),
            _ => return Err(format!("{n} is out of range")),
        },
        Json::String(s) => string(s),
        Json::Array(items) => {
            let mut a = Array::new();
            for item in items {
                a.push(json_to_toml(item)?);
            }
            Value::Array(a)
        }
        Json::Object(map) => {
            let mut t = InlineTable::new();
            for (k, item) in map {
                t.insert(k, json_to_toml(item)?);
            }
            Value::InlineTable(t)
        }
    })
}

fn toml_to_json(v: &Value) -> Result<Json, String> {
    Ok(match v {
        Value::String(s) => Json::from(s.value().as_str()),
        Value::Integer(i) => Json::from(*i.value()),
        Value::Float(f) => {
            let f = *f.value();
            Json::from(
                serde_json::Number::from_f64(f).ok_or(format!("{f} is not a finite number"))?,
            )
        }
        Value::Boolean(b) => Json::from(*b.value()),
        Value::Datetime(_) => return Err("dates are not parameter values".into()),
        Value::Array(a) => Json::Array(a.iter().map(toml_to_json).collect::<Result<_, _>>()?),
        Value::InlineTable(t) => Json::Object(
            t.iter()
                .map(|(k, v)| Ok((k.to_owned(), toml_to_json(v)?)))
                .collect::<Result<_, String>>()?,
        ),
    })
}

/// `[[name]]` tables, or an array of inline tables.
fn tables<'a>(item: Option<&'a Item>, name: &str) -> Result<Vec<&'a dyn TableLike>, FormatError> {
    match item {
        None => Ok(Vec::new()),
        Some(Item::ArrayOfTables(a)) => Ok(a.iter().map(|t| t as &dyn TableLike).collect()),
        Some(Item::Value(Value::Array(a))) => a
            .iter()
            .map(|v| {
                v.as_inline_table()
                    .map(|t| t as &dyn TableLike)
                    .ok_or_else(|| invalid(format!("`{name}` entries must be tables")))
            })
            .collect(),
        Some(_) => Err(invalid(format!(
            "`{name}` must be written as [[{name}]] tables"
        ))),
    }
}

fn table_like<'a>(item: &'a Item, at: &str) -> Result<&'a dyn TableLike, FormatError> {
    item.as_table_like()
        .ok_or_else(|| invalid(format!("{at}: expected a table")))
}

pub(crate) fn only_keys(t: &dyn TableLike, keys: &[&str], at: &str) -> Result<(), FormatError> {
    for (k, _) in t.iter() {
        if !keys.contains(&k) {
            return Err(invalid(format!(
                "{at}: unknown key `{k}` (expected one of {})",
                keys.join(", ")
            )));
        }
    }
    Ok(())
}

fn required_u64(t: &dyn TableLike, key: &str, at: &str) -> Result<u64, FormatError> {
    t.get(key)
        .and_then(Item::as_integer)
        .and_then(|v| u64::try_from(v).ok())
        .ok_or_else(|| invalid(format!("{at}: `{key}` must be a whole number ≥ 0")))
}

pub(crate) fn required_str<'a>(
    t: &'a dyn TableLike,
    key: &str,
    at: &str,
) -> Result<&'a str, FormatError> {
    t.get(key)
        .and_then(Item::as_str)
        .ok_or_else(|| invalid(format!("{at}: `{key}` must be a string")))
}

fn pair(item: &Item, at: &str) -> Result<Json, FormatError> {
    let bad = || invalid(format!("{at}: expected two numbers, like [0.0, 0.0]"));
    let a = item.as_array().ok_or_else(bad)?;
    let nums: Vec<f64> = a
        .iter()
        .map(|v| {
            v.as_float()
                .or_else(|| v.as_integer().map(|i| i as f64))
                .filter(|f| f.is_finite())
        })
        .collect::<Option<_>>()
        .ok_or_else(bad)?;
    if nums.len() != 2 {
        return Err(bad());
    }
    Ok(json!(nums))
}
