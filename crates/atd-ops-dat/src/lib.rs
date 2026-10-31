//! DAT operator family: tables, text, JSON and scripting data, PRD §6.
//!
//! DAT values ([`atd_core::dat::Dat`]) are shared text or tables of
//! strings; the engine cooks a DAT node only when an input value, a
//! parameter or the time it depends on changed, and an operator keeps its
//! old value when the new one is equal (`Dat::replace`), so nothing
//! downstream cooks for nothing. Operators that touch files, sockets or
//! devices do so on worker threads and poll them from the cook (CLAUDE.md
//! rule 1); a steady frame allocates nothing (rule 2).
//!
//! Every operator ships complete (rule 7): schema with a cook-time budget,
//! a generated docs page (`atidarla docs`), example projects
//! (`examples/dat-*.atidarla`) and unit tests.
//!
//! | Operator | Gives |
//! |---|---|
//! | `dat.table` | a table typed in as delimited text |
//! | `dat.text` | text typed in |
//! | `dat.json` | JSON (typed or from an input) as a table or pretty text |
//! | `dat.convert` | text ⇄ table ⇄ JSON |
//! | `dat.select` | rows and columns of a table, by range, name or match |
//! | `dat.prompt_template` | text with `{name}` filled from a table, text and channels |
//! | `dat.file_in` | a file's text or table, reloaded when it is saved |
//! | `dat.file_out` | its input, written to a file |
//! | `dat.web_client` | an HTTP(S) response body, through the egress gate |
//! | `dat.web_server` | requests received; serves its input on loopback |
//! | `dat.websocket` | messages received; sends its input |
//! | `dat.serial` | lines from a serial port; writes its input |

pub mod delim;
mod file;
mod json;
mod net;
mod select;
mod serial;
mod serial_port;
mod table;
mod template;
mod web;
mod websocket;

use atd_core::registry::{OpFamily, OperatorSchema, ParamSchema, Registry, SocketSchema, WireType};
use atd_gpu::engine::{DatOp, OpLibrary};

/// A DAT input socket.
pub(crate) fn dat_in(name: &str, optional: bool) -> SocketSchema {
    SocketSchema {
        name: name.into(),
        wire: WireType::Dat,
        optional,
        feedback: false,
    }
}

/// An optional SIG input socket.
pub(crate) fn sig_in(name: &str) -> SocketSchema {
    SocketSchema {
        name: name.into(),
        wire: WireType::Sig,
        optional: true,
        feedback: false,
    }
}

/// The DAT `out` socket every operator here has.
pub(crate) fn dat_out() -> SocketSchema {
    SocketSchema {
        name: "out".into(),
        wire: WireType::Dat,
        optional: false,
        feedback: false,
    }
}

/// A DAT operator schema (not time-dependent, not a sink).
pub(crate) fn op_schema(
    name: &str,
    label: &str,
    summary: &str,
    params: Vec<ParamSchema>,
    inputs: Vec<SocketSchema>,
    cook_budget_us: u32,
) -> OperatorSchema {
    OperatorSchema {
        type_id: format!("dat.{name}"),
        family: OpFamily::Dat,
        label: label.into(),
        summary: summary.into(),
        version: 1,
        params,
        inputs,
        outputs: vec![dat_out()],
        capabilities: vec![],
        cook_budget_us,
        time_dependent: false,
        sink: false,
    }
}

type Factory = fn() -> Box<dyn DatOp>;

/// Every operator: its schema and its factory.
fn ops() -> Vec<(OperatorSchema, Factory)> {
    vec![
        (table::table_schema(), table::create_table),
        (table::text_schema(), table::create_text),
        (json::schema(), json::create),
        (table::convert_schema(), table::create_convert),
        (select::schema(), select::create),
        (template::schema(), template::create),
        (file::file_in_schema(), file::create_file_in),
        (file::file_out_schema(), file::create_file_out),
        (web::client_schema(), web::create_client),
        (web::server_schema(), web::create_server),
        (websocket::schema(), websocket::create),
        (serial::schema(), serial::create),
    ]
}

/// Every schema this crate registers.
pub fn schemas() -> Vec<OperatorSchema> {
    ops().into_iter().map(|(s, _)| s).collect()
}

/// Registers this family's operator schemas.
pub fn register(registry: &mut Registry) {
    for schema in schemas() {
        registry
            .register(schema)
            .expect("DAT operator schemas are valid and unique");
    }
}

/// Registers this family's implementations.
pub fn library(library: &mut OpLibrary) {
    for (schema, factory) in ops() {
        library.add_dat(schema.type_id, factory);
    }
}

/// Cooking one operator outside the engine, for the unit tests.
#[cfg(test)]
pub(crate) mod testing {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use atd_core::dat::Dat;
    use atd_core::graph::{Edit, Network, Node, NodeId, Param};
    use atd_core::registry::{OperatorSchema, Registry};
    use atd_gpu::engine::{Channels, DatCtx, DatInputs, DatOp, Params, SigInputs};
    use serde_json::Value;

    pub struct Harness {
        pub net: Network,
        pub registry: Registry,
        pub node: NodeId,
        pub sig: HashMap<NodeId, Channels>,
        pub dat: HashMap<NodeId, Dat>,
        pub dat_sources: Vec<Option<NodeId>>,
        pub sig_sources: Vec<Option<NodeId>>,
        pub out: Dat,
        pub dir: Option<PathBuf>,
        pub frame: u64,
    }

    impl Harness {
        pub fn new(schema: OperatorSchema) -> Self {
            let inputs = schema.inputs.len();
            let type_id = schema.type_id.clone();
            let mut registry = Registry::default();
            registry.register(schema).unwrap();
            let mut net = Network::default();
            let node = net.alloc_id();
            net.apply(
                &registry,
                Edit::AddNode {
                    node: Node::new(node, type_id, "op"),
                },
            )
            .unwrap();
            Self {
                net,
                registry,
                node,
                sig: HashMap::new(),
                dat: HashMap::new(),
                dat_sources: vec![None; inputs],
                sig_sources: vec![None; inputs],
                out: Dat::default(),
                dir: None,
                frame: 0,
            }
        }

        pub fn set(&mut self, name: &str, value: Value) -> &mut Self {
            self.net
                .apply(
                    &self.registry,
                    Edit::SetParam {
                        node: self.node,
                        param: name.into(),
                        value: Some(Param::constant(value)),
                    },
                )
                .unwrap();
            self
        }

        /// Puts `value` on DAT input `i` (`None` disconnects it).
        pub fn input(&mut self, i: usize, value: Option<Dat>) -> &mut Self {
            let id = NodeId(1000 + i as u64);
            match value {
                Some(v) => {
                    self.dat.insert(id, v);
                    self.dat_sources[i] = Some(id);
                }
                None => self.dat_sources[i] = None,
            }
            self
        }

        /// Puts `channels` on SIG input `i`.
        pub fn sig_input(&mut self, i: usize, channels: Channels) -> &mut Self {
            let id = NodeId(2000 + i as u64);
            self.sig.insert(id, channels);
            self.sig_sources[i] = Some(id);
            self
        }

        /// Cooks once; `Ok(true)` when the operator asked to cook again.
        pub fn cook(&mut self, op: &mut dyn DatOp) -> Result<bool, String> {
            let params = Params::new(&self.net, &self.registry, &self.sig, self.node)
                .with_project_dir(self.dir.as_deref());
            let mut ctx = DatCtx {
                inputs: DatInputs::new(&self.dat_sources, &self.dat),
                sig_inputs: SigInputs::new(&self.sig_sources, &self.sig),
                output: &mut self.out,
                params,
                inputs_changed: true,
                time: self.frame as f64 / 60.0,
                dt: 1.0 / 60.0,
                frame: self.frame,
                cook_again: false,
            };
            let r = op.cook(&mut ctx);
            let again = ctx.cook_again;
            self.frame += 1;
            r.map(|()| again)
        }

        /// Cooks until the operator stops asking to (at most `frames`
        /// frames, a millisecond apart), for operators with workers.
        pub fn settle(&mut self, op: &mut dyn DatOp, frames: u32) -> Result<(), String> {
            for _ in 0..frames {
                if !self.cook(op)? {
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err("still cooking again".into())
        }

        pub fn text(&self) -> String {
            self.out.as_text().into_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_operator_registers_complete() {
        let mut registry = Registry::default();
        register(&mut registry);
        let mut library = OpLibrary::default();
        super::library(&mut library);
        assert_eq!(registry.iter().count(), ops().len());
        for op in registry.iter() {
            assert!(op.type_id.starts_with("dat."), "{}", op.type_id);
            assert!(library.contains(&op.type_id), "{}", op.type_id);
            assert!(op.cook_budget_us > 0, "{}", op.type_id);
            assert!(!op.summary.is_empty(), "{}", op.type_id);
            assert_eq!(op.outputs[0].wire, WireType::Dat, "{}", op.type_id);
            for p in &op.params {
                assert!(!p.help.is_empty(), "{}.{} has no help", op.type_id, p.name);
            }
        }
    }
}
