//! The agent's typed tools (FR-G1) and the session that runs them.
//!
//! Every tool has a JSON Schema for its arguments and returns JSON. Graph
//! changes are not applied as they are made: they are staged on a preview
//! copy of the network (FR-G2), reviewed with `review_changes`, and applied
//! by `accept_changes` as one undo step — by a person in the app, or by the
//! agent itself only when the session has the `accept` scope (headless
//! `serve --auto-accept`). Under Show Lock (FR-G3) graph edits are refused
//! and only whitelisted parameters move, within their ranges, at once.
//! There is no arbitrary code execution: `python_exec` exists only when
//! the session has the `python.exec` scope (CLAUDE.md rule 9).

use std::collections::BTreeSet;
use std::fmt::Write as _;

use atd_core::Registry;
use atd_core::graph::{Edit, Endpoint, Network, Node, NodeId, Param, Wire};
use atd_core::pack::{self, PackDef, ParamTarget, Version};
use atd_core::registry::{OpFamily, OperatorSchema, ParamKind};
use serde_json::{Map, Value, json};

use crate::cues::{self, CueContext, CueQueue, Move, Quantum};
use crate::lock::{Refusal, Scope, ShowLock};
use crate::workspace::Workspace;

/// A tool as offered to a model: name, description, argument schema.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: Value,
    /// Scope needed to see and call it (`None`: always).
    pub scope: Option<Scope>,
}

/// A tool's answer: JSON, an image, or an error message for the model.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    Json(Value),
    Png(Vec<u8>, String),
    Error(String),
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

/// Every tool there is.
pub fn specs() -> Vec<ToolSpec> {
    let node = json!({"type": "string", "description": "A node's name (or its id, e.g. `n12`)."});
    vec![
        ToolSpec {
            name: "list_ops",
            description: "Lists the operator types (type id, label, one-line summary), optionally of one family: img (images), sig (channels: audio, control, MIDI), pnt (points), msh (meshes), mat (materials), cmp (scene, Packs), dat, ai.",
            schema: obj(
                json!({"family": {"type": "string", "enum": ["img", "sig", "pnt", "msh", "mat", "cmp", "dat", "ai"]}}),
                &[],
            ),
            scope: None,
        },
        ToolSpec {
            name: "get_schema",
            description: "The full schema of an operator type: parameters (kind, default, range, menu options, help), inputs and outputs with their wire types (IMG, SIG, GEO, MAT, CMP, DAT), capability flags. Read it before creating or wiring a node of that type.",
            schema: obj(json!({"type": {"type": "string"}}), &["type"]),
            scope: None,
        },
        ToolSpec {
            name: "get_network",
            description: "The current network as the agent sees it (including staged, not yet accepted changes): each node's name, id, type, parameters that differ from their defaults (with expressions and channel bindings), and what is wired into each input; plus node errors from the last cook.",
            schema: obj(json!({}), &[]),
            scope: None,
        },
        ToolSpec {
            name: "create_node",
            description: "Stages a new node. Returns its name and id. Parameters not given keep their defaults. Wire it with `connect`.",
            schema: obj(
                json!({
                    "type": {"type": "string", "description": "Operator type id, e.g. `img.noise`."},
                    "name": {"type": "string", "description": "Optional name; a unique one is made from the type if omitted."},
                    "params": {"type": "object", "description": "Parameter values by name."},
                    "position": {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2}
                }),
                &["type"],
            ),
            scope: Some(Scope::Edit),
        },
        ToolSpec {
            name: "build_network",
            description: "Stages a whole network in one call: nodes (type, name, parameter values) and the wires between them. A node whose name already exists with the same type is updated, not duplicated, so call it again with the same names to fix errors. A text value for a number parameter is taken as an expression (e.g. `0.5 + 0.5 * sin(time)`). Returns the nodes made and every error; then call accept_changes.",
            schema: obj(
                json!({
                    "nodes": {"type": "array", "items": {"type": "object", "properties": {
                        "name": {"type": "string"},
                        "type": {"type": "string", "description": "Operator type id, e.g. `img.noise`."},
                        "params": {"type": "object", "description": "Parameter values (or expressions) by name."}
                    }, "required": ["name", "type"]}},
                    "wires": {"type": "array", "items": {"type": "object", "properties": {
                        "from": {"type": "string"}, "output": {"type": "string", "default": "out"},
                        "to": {"type": "string"}, "input": {"type": "string"}
                    }, "required": ["from", "to", "input"]}}
                }),
                &["nodes"],
            ),
            scope: Some(Scope::Edit),
        },
        ToolSpec {
            name: "delete_node",
            description: "Stages removing a node and its wires.",
            schema: obj(json!({"node": node.clone()}), &["node"]),
            scope: Some(Scope::Edit),
        },
        ToolSpec {
            name: "connect",
            description: "Stages a wire from an output of one node to an input of another (replacing what was wired there). Wire types must match (IMG to IMG, SIG to SIG, GEO to GEO …).",
            schema: obj(
                json!({
                    "from": node.clone(), "output": {"type": "string", "default": "out"},
                    "to": node.clone(), "input": {"type": "string"}
                }),
                &["from", "to", "input"],
            ),
            scope: Some(Scope::Edit),
        },
        ToolSpec {
            name: "disconnect",
            description: "Stages removing the wire into an input.",
            schema: obj(
                json!({"to": node.clone(), "input": {"type": "string"}}),
                &["to", "input"],
            ),
            scope: Some(Scope::Edit),
        },
        ToolSpec {
            name: "set_param",
            description: "Sets a parameter: a value, an expression (e.g. `0.5 + 0.5 * sin(time)`, or `op('lfo1')['lfo']`), or a binding to a SIG node's channel. Without Show Lock this is staged; under Show Lock only whitelisted parameters within their ranges move, at once.",
            schema: obj(
                json!({
                    "node": node.clone(), "param": {"type": "string"},
                    "value": {"description": "The value (number, bool, string, or an array for vectors and colours)."},
                    "expression": {"type": "string"},
                    "bind": {"type": "object", "properties": {"node": {"type": "string"}, "channel": {"type": "string"}}, "required": ["node", "channel"]}
                }),
                &["node", "param"],
            ),
            scope: None,
        },
        ToolSpec {
            name: "create_pack",
            description: "Stages turning existing nodes (or nodes you just created) into a new Pack: a reusable unit with a semantic version, help, promoted parameters and sockets. The nodes are replaced by one instance (`cmp.pack_<name>`). Wires from outside the selection become the Pack's inputs, wires to outside its outputs (with none, the last node nothing inside reads becomes `out`). Accepted with the other staged changes as one undo step.",
            schema: obj(
                json!({
                    "name": {"type": "string", "pattern": "^[a-z][a-z0-9_]*$", "maxLength": 48, "description": "The Pack's name; its operator type is `cmp.pack_<name>`."},
                    "nodes": {"type": "array", "items": node.clone(), "minItems": 1, "description": "The nodes that go inside the Pack."},
                    "promote": {"type": "array", "description": "Inner parameters the instance shows as its own; their current values become the defaults.", "items": {
                        "type": "object",
                        "properties": {
                            "node": node.clone(),
                            "param": {"type": "string"},
                            "name": {"type": "string", "pattern": "^[a-z_][a-z0-9_]*$", "description": "The promoted parameter's name (default: the inner one's)."},
                            "label": {"type": "string"},
                            "help": {"type": "string"},
                            "min": {"type": "number", "description": "May only narrow the inner parameter's range."},
                            "max": {"type": "number"}
                        },
                        "required": ["node", "param"],
                        "additionalProperties": false
                    }},
                    "version": {"type": "string", "pattern": "^[0-9]+\\.[0-9]+\\.[0-9]+$", "default": "0.1.0"},
                    "label": {"type": "string"},
                    "summary": {"type": "string", "description": "One sentence for the Palette."},
                    "help": {"type": "string", "description": "The help page (Markdown)."},
                    "instance": {"type": "string", "description": "The instance's name (default: the Pack's name and a number)."}
                }),
                &["name", "nodes"],
            ),
            scope: Some(Scope::Edit),
        },
        ToolSpec {
            name: "review_changes",
            description: "The staged changes as a list (nodes added/removed, wires, parameter changes old → new). Nothing is applied until accepted.",
            schema: obj(json!({}), &[]),
            scope: None,
        },
        ToolSpec {
            name: "accept_changes",
            description: "Applies every staged change as one undo step and cooks the network; returns node errors so you can fix them. Only available when this session may accept on its own; otherwise a person accepts in the app.",
            schema: obj(
                json!({"label": {"type": "string", "description": "What the changes do, for the undo history."}}),
                &[],
            ),
            scope: Some(Scope::Accept),
        },
        ToolSpec {
            name: "discard_changes",
            description: "Drops every staged change.",
            schema: obj(json!({}), &[]),
            scope: None,
        },
        ToolSpec {
            name: "get_errors",
            description: "Cooks the network once and lists node errors (empty when everything cooks).",
            schema: obj(json!({}), &[]),
            scope: None,
        },
        ToolSpec {
            name: "screenshot_viewer",
            description: "A PNG of a node's image output, to see what the patch looks like.",
            schema: obj(
                json!({"node": node.clone(), "max_size": {"type": "integer", "minimum": 64, "maximum": 2048, "default": 512}}),
                &["node"],
            ),
            scope: None,
        },
        ToolSpec {
            name: "run_bench",
            description: "Cooks the network for a number of frames and reports each node's mean cook time in milliseconds against its budget.",
            schema: obj(
                json!({"frames": {"type": "integer", "minimum": 1, "maximum": 600, "default": 60}}),
                &[],
            ),
            scope: None,
        },
        ToolSpec {
            name: "write_kernel",
            description: "Writes a Kernel DSL file into the project's `kernels/` folder (`<name>.atk` for points, `<name>.pixels.atk` for pixels); it becomes operator `kernel.<name>`. Returns compile errors, if any. See the Kernel DSL reference.",
            schema: obj(
                json!({
                    "name": {"type": "string", "pattern": "^[a-z][a-z0-9_]*$"},
                    "target": {"type": "string", "enum": ["points", "pixels"]},
                    "source": {"type": "string"}
                }),
                &["name", "target", "source"],
            ),
            scope: Some(Scope::Files),
        },
        ToolSpec {
            name: "stage_list",
            description: "The Stage decks: layers, their clips by slot, what each layer plays, the crossfader.",
            schema: obj(json!({}), &[]),
            scope: None,
        },
        ToolSpec {
            name: "stage_launch",
            description: "Launches the clip in a slot of a Stage layer (or stops the layer with slot -1); the layer's transition and quantise settings apply.",
            schema: obj(
                json!({"layer": node.clone(), "slot": {"type": "integer", "minimum": -1, "maximum": 15}}),
                &["layer", "slot"],
            ),
            scope: Some(Scope::Stage),
        },
        ToolSpec {
            name: "stage_crossfader",
            description: "Moves a deck's crossfader (0 = side A, 1 = side B).",
            schema: obj(
                json!({"mixer": node, "value": {"type": "number", "minimum": 0, "maximum": 1}}),
                &["mixer", "value"],
            ),
            scope: Some(Scope::Stage),
        },
        ToolSpec {
            name: "cue",
            description: "An in-show cue: words such as `darker, slower on the next drop` or `set brightness to 0.4 now` become moves of whitelisted parameters (Show Lock `allow` entries), within their ranges, fired on the clock: `now` on the next beat, by default on the next bar, `next drop` on the next 16-beat boundary, `next phrase` on the next 32. Returns the planned moves and queues them; never edits the graph.",
            schema: obj(
                json!({
                    "text": {"type": "string", "description": "The cue, as the performer said it."},
                    "beat": {"type": "number", "description": "The current beat position (default: the show's clock)."},
                    "quantum": {"type": "string", "enum": ["beat", "bar", "drop", "phrase"], "description": "Overrides the timing the words ask for."},
                    "queue": {"type": "boolean", "default": true, "description": "False: only plan, queue nothing."}
                }),
                &["text"],
            ),
            scope: Some(Scope::Stage),
        },
        ToolSpec {
            name: "python_exec",
            description: "Runs Python in the project's interpreter and returns what it printed. Only in sessions the user granted `python.exec`.",
            schema: obj(json!({"code": {"type": "string"}}), &["code"]),
            scope: Some(Scope::PythonExec),
        },
    ]
}

/// One agent session over a workspace.
#[derive(Debug, Clone)]
pub struct Agent {
    pub scopes: BTreeSet<Scope>,
    pub lock: ShowLock,
    /// The network with the staged changes applied, while any are staged.
    preview: Option<Network>,
    /// The registry with the staged Packs' operator types, while any are staged.
    preview_registry: Option<Registry>,
    /// New Packs the staged edits use (added when they are accepted).
    staged_packs: Vec<PackDef>,
    staged: Vec<Edit>,
    /// What each staged edit is, for review.
    notes: Vec<String>,
    /// Cue moves waiting for their beat (FR-G5); [`Self::fire_cues`] applies them.
    pub cues: CueQueue,
}

impl Agent {
    pub fn new(scopes: BTreeSet<Scope>, lock: ShowLock) -> Self {
        Self {
            scopes,
            lock,
            preview: None,
            preview_registry: None,
            staged_packs: Vec::new(),
            staged: Vec::new(),
            notes: Vec::new(),
            cues: CueQueue::default(),
        }
    }

    /// The tools this session may call.
    pub fn tools(&self) -> Vec<ToolSpec> {
        specs()
            .into_iter()
            .filter(|t| t.scope.as_ref().is_none_or(|s| self.scopes.contains(s)))
            .collect()
    }

    pub fn staged(&self) -> &[Edit] {
        &self.staged
    }

    /// New Packs the staged changes use (`create_pack`).
    pub fn staged_packs(&self) -> &[PackDef] {
        &self.staged_packs
    }

    /// The staged changes, taken (for a person accepting them in the app).
    /// Staged Packs are dropped; use [`Self::take_staged_with_packs`] when
    /// any may be staged.
    pub fn take_staged(&mut self) -> (Vec<Edit>, Vec<String>) {
        let (_, edits, notes) = self.take_staged_with_packs();
        (edits, notes)
    }

    /// The staged changes and the new Packs they use, taken: apply them with
    /// [`Workspace::apply_with_packs`] as one undo step.
    pub fn take_staged_with_packs(&mut self) -> (Vec<PackDef>, Vec<Edit>, Vec<String>) {
        self.preview = None;
        self.preview_registry = None;
        (
            std::mem::take(&mut self.staged_packs),
            std::mem::take(&mut self.staged),
            std::mem::take(&mut self.notes),
        )
    }

    pub(crate) fn view<'a>(&'a self, ws: &'a dyn Workspace) -> &'a Network {
        self.preview.as_ref().unwrap_or_else(|| ws.network())
    }

    /// The registry as the agent sees it (staged Packs included).
    fn reg<'a>(&'a self, ws: &'a dyn Workspace) -> &'a Registry {
        self.preview_registry
            .as_ref()
            .unwrap_or_else(|| ws.registry())
    }

    /// Runs a tool. Errors are answers for the model, not failures.
    pub fn call(&mut self, ws: &mut dyn Workspace, name: &str, args: &Value) -> Output {
        let Some(spec) = specs().into_iter().find(|t| t.name == name) else {
            return Output::Error(format!("unknown tool `{name}`"));
        };
        if let Some(scope) = &spec.scope
            && !self.scopes.contains(scope)
        {
            return Output::Error(Refusal::Scope(scope.name()).to_string());
        }
        let empty = Map::new();
        let a = args.as_object().unwrap_or(&empty);
        match self.run(ws, name, a) {
            Ok(o) => o,
            Err(e) => Output::Error(e),
        }
    }

    fn run(
        &mut self,
        ws: &mut dyn Workspace,
        name: &str,
        a: &Map<String, Value>,
    ) -> Result<Output, String> {
        let s = |k: &str| a.get(k).and_then(Value::as_str);
        let need = |k: &str| s(k).ok_or_else(|| format!("missing argument `{k}`"));
        match name {
            "list_ops" => {
                let family = s("family");
                let ops: Vec<Value> = self
                    .reg(ws)
                    .iter()
                    .filter(|o| family.is_none_or(|f| family_name(o.family) == f))
                    .map(|o| json!({"type": o.type_id, "label": o.label, "summary": o.summary}))
                    .collect();
                Ok(Output::Json(json!(ops)))
            }
            "get_schema" => {
                let t = need("type")?;
                let op = self
                    .reg(ws)
                    .get(t)
                    .ok_or_else(|| unknown_type(ws.registry(), t))?;
                Ok(Output::Json(schema_json(op)))
            }
            "get_network" => {
                let errors = ws.cook_errors();
                Ok(Output::Json(network_json(self.view(ws), ws, &errors)))
            }
            "create_node" => {
                self.lock.check_graph_edit().map_err(|e| e.to_string())?;
                let t = need("type")?;
                let op = self
                    .reg(ws)
                    .get(t)
                    .ok_or_else(|| unknown_type(ws.registry(), t))?
                    .clone();
                let mut net = self.view(ws).clone();
                let id = net.alloc_id();
                let base = op.type_id.split('.').nth(1).unwrap_or("node").to_owned();
                let name = match s("name") {
                    Some(n) if net.node_by_name(n).is_some() => {
                        return Err(format!("a node named `{n}` exists; pick another name"));
                    }
                    Some(n) => n.to_owned(),
                    None => net.unique_name(&base),
                };
                let mut node = Node::new(id, op.type_id.clone(), name.clone());
                if let Some(p) = a.get("position").and_then(Value::as_array)
                    && let (Some(x), Some(y)) = (
                        p.first().and_then(Value::as_f64),
                        p.get(1).and_then(Value::as_f64),
                    )
                {
                    node.pos = [x as f32, y as f32];
                } else {
                    node.pos = [
                        220.0 * (net.len() % 6) as f32,
                        160.0 * (net.len() / 6) as f32,
                    ];
                }
                let mut edits = vec![Edit::AddNode { node }];
                // Unknown parameters are left out and reported; the node is
                // still made (a model can set the right ones next).
                let mut ignored = Vec::new();
                if let Some(params) = a.get("params").and_then(Value::as_object) {
                    for (p, v) in params {
                        if !op.params.iter().any(|s| &s.name == p) {
                            ignored.push(p.clone());
                            continue;
                        }
                        edits.push(Edit::SetParam {
                            node: id,
                            param: p.clone(),
                            value: Some(Param::constant(v.clone())),
                        });
                    }
                }
                self.stage(ws, net, edits, format!("add {name} ({t})"))?;
                let mut out = json!({"name": name, "id": id.to_string(), "staged": true});
                if !ignored.is_empty() {
                    out["ignored_params"] = json!(ignored);
                    out["params_it_has"] = json!(
                        op.params
                            .iter()
                            .map(|s| s.name.as_str())
                            .collect::<Vec<_>>()
                    );
                }
                Ok(Output::Json(out))
            }
            "build_network" => self.build_network(ws, a),
            "delete_node" => {
                self.lock.check_graph_edit().map_err(|e| e.to_string())?;
                let net = self.view(ws).clone();
                let id = find(&net, need("node")?)?;
                let name = net.node(id).map(|n| n.name.clone()).unwrap_or_default();
                self.stage(
                    ws,
                    net,
                    vec![Edit::RemoveNode { id }],
                    format!("remove {name}"),
                )?;
                Ok(Output::Json(json!({"removed": name, "staged": true})))
            }
            "connect" => {
                self.lock.check_graph_edit().map_err(|e| e.to_string())?;
                let net = self.view(ws).clone();
                let from = find(&net, need("from")?)?;
                let to = find(&net, need("to")?)?;
                let output = s("output").unwrap_or("out");
                let input = need("input")?;
                let wire = Wire::new(Endpoint::new(from, output), Endpoint::new(to, input));
                let note = format!(
                    "wire {}.{output} → {}.{input}",
                    net.node(from).map_or("?", |n| n.name.as_str()),
                    net.node(to).map_or("?", |n| n.name.as_str())
                );
                self.stage(ws, net, vec![Edit::Connect { wire }], note)?;
                Ok(Output::Json(json!({"connected": true, "staged": true})))
            }
            "disconnect" => {
                self.lock.check_graph_edit().map_err(|e| e.to_string())?;
                let net = self.view(ws).clone();
                let to = find(&net, need("to")?)?;
                let input = need("input")?;
                let (_, src) = net
                    .inputs_of(to)
                    .find(|(n, _)| *n == input)
                    .ok_or_else(|| format!("nothing is wired into `{input}`"))?;
                let wire = Wire::new(src.clone(), Endpoint::new(to, input));
                self.stage(
                    ws,
                    net,
                    vec![Edit::Disconnect { wire }],
                    format!("unwire {input}"),
                )?;
                Ok(Output::Json(json!({"disconnected": true, "staged": true})))
            }
            "set_param" => self.set_param(ws, a),
            "create_pack" => self.create_pack(ws, a),
            "review_changes" => Ok(Output::Json(json!({
                "staged": self.notes,
                "count": self.staged.len(),
            }))),
            "accept_changes" => {
                let label = s("label").unwrap_or("Agent changes").to_owned();
                let errors = self.accept(ws, &label)?;
                Ok(Output::Json(
                    json!({"applied": true, "errors": errors_json(ws, &errors)}),
                ))
            }
            "discard_changes" => {
                let n = self.staged.len();
                self.take_staged();
                Ok(Output::Json(json!({"discarded": n})))
            }
            "get_errors" => {
                let errors = ws.cook_errors();
                Ok(Output::Json(errors_json(ws, &errors)))
            }
            "screenshot_viewer" => {
                let id = find(ws.network(), need("node")?)?;
                let max = a
                    .get("max_size")
                    .and_then(Value::as_u64)
                    .unwrap_or(512)
                    .clamp(64, 2048) as u32;
                let png = ws.screenshot(id, max)?;
                Ok(Output::Png(png, format!("image of {}", need("node")?)))
            }
            "run_bench" => {
                let frames = a
                    .get("frames")
                    .and_then(Value::as_u64)
                    .unwrap_or(60)
                    .clamp(1, 600) as u32;
                let times = ws.bench(frames)?;
                let net = ws.network();
                let rows: Vec<Value> = times
                    .iter()
                    .filter_map(|(id, ms)| {
                        let n = net.node(*id)?;
                        let budget = ws.registry().get(&n.type_id).map(|o| f64::from(o.cook_budget_us) / 1e3);
                        Some(json!({"node": n.name, "type": n.type_id, "mean_ms": ms, "budget_ms": budget}))
                    })
                    .collect();
                Ok(Output::Json(json!({"frames": frames, "nodes": rows})))
            }
            "write_kernel" => {
                self.lock.check_graph_edit().map_err(|e| e.to_string())?;
                let kname = need("name")?;
                if !kname
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                    || !kname.starts_with(|c: char| c.is_ascii_lowercase())
                {
                    return Err("a kernel name is lower-case letters, digits and `_`".into());
                }
                let pixels = need("target")? == "pixels";
                let source = need("source")?;
                let target = if pixels {
                    atd_kernel::Target::Pixels
                } else {
                    atd_kernel::Target::Points
                };
                if let Err(errs) =
                    atd_kernel::compile(source, &atd_kernel::CompileOptions::new(target))
                {
                    let list: Vec<String> = errs.iter().map(ToString::to_string).collect();
                    return Ok(Output::Json(json!({"written": false, "errors": list})));
                }
                let dir = ws
                    .project_dir()
                    .ok_or("there is no project folder to write kernels into")?
                    .join("kernels");
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let file = dir.join(if pixels {
                    format!("{kname}.pixels.atk")
                } else {
                    format!("{kname}.atk")
                });
                std::fs::write(&file, source).map_err(|e| e.to_string())?;
                ws.files_changed()?;
                Ok(Output::Json(
                    json!({"written": true, "operator": format!("kernel.{kname}")}),
                ))
            }
            "stage_list" => Ok(Output::Json(stage_json(ws))),
            "stage_launch" => {
                self.lock.check_stage().map_err(|e| e.to_string())?;
                let layer = find(ws.network(), need("layer")?)?;
                let slot = a
                    .get("slot")
                    .and_then(Value::as_i64)
                    .ok_or("missing argument `slot`")?;
                let target = usize::try_from(slot).ok();
                ws.apply("Agent: launch", vec![atd_stage::set_target(layer, target)])?;
                Ok(Output::Json(json!({"launched": slot})))
            }
            "stage_crossfader" => {
                self.lock.check_stage().map_err(|e| e.to_string())?;
                let mixer = find(ws.network(), need("mixer")?)?;
                let v = a
                    .get("value")
                    .and_then(Value::as_f64)
                    .ok_or("missing argument `value`")?;
                ws.apply(
                    "Agent: crossfader",
                    vec![Edit::SetParam {
                        node: mixer,
                        param: "crossfader".into(),
                        value: Some(Param::constant(json!(v.clamp(0.0, 1.0)))),
                    }],
                )?;
                Ok(Output::Json(json!({"crossfader": v.clamp(0.0, 1.0)})))
            }
            "cue" => {
                let text = need("text")?;
                let now = a
                    .get("beat")
                    .and_then(Value::as_f64)
                    .or_else(|| ws.beat())
                    .ok_or("this workspace has no clock; pass `beat`")?;
                let quantum = match s("quantum") {
                    Some(q) => Some(Quantum::parse(q).ok_or_else(|| format!("no quantum `{q}`"))?),
                    None => None,
                };
                // Cues move the live show, not the staged preview.
                let plan = CueContext::capture(ws.network(), ws.registry(), &self.lock)
                    .plan(text, now, quantum);
                let queue = a.get("queue").and_then(Value::as_bool).unwrap_or(true);
                if queue {
                    self.cues.push(plan.moves.iter().cloned());
                }
                let mut out = serde_json::to_value(&plan).map_err(|e| e.to_string())?;
                out["queued"] = json!(queue && !plan.moves.is_empty());
                Ok(Output::Json(out))
            }
            "python_exec" => {
                let out = ws.python_exec(need("code")?)?;
                Ok(Output::Json(json!({"output": out})))
            }
            other => Err(format!("unknown tool `{other}`")),
        }
    }

    fn set_param(
        &mut self,
        ws: &mut dyn Workspace,
        a: &Map<String, Value>,
    ) -> Result<Output, String> {
        let net = self.view(ws).clone();
        let node_ref = a
            .get("node")
            .and_then(Value::as_str)
            .ok_or("missing argument `node`")?;
        let id = find(&net, node_ref)?;
        let node = net.node(id).expect("found");
        let param = a
            .get("param")
            .and_then(Value::as_str)
            .ok_or("missing argument `param`")?;
        let op = self
            .reg(ws)
            .get(&node.type_id)
            .ok_or("unknown operator type")?;
        let schema = op.params.iter().find(|p| p.name == param).ok_or_else(|| {
            format!(
                "`{}` has no parameter `{param}`; it has: {}",
                node.type_id,
                op.params
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        let current = node.params.get(param).cloned();
        let mut p = current.unwrap_or_else(|| Param::constant(schema.default.clone()));
        if let Some(v) = a.get("value") {
            p.value = v.clone();
            // Models often quote numbers ("0.5"); a quoted number, toggle or
            // list is taken as itself for parameters that are not text.
            if let Some(text) = v.as_str()
                && !matches!(
                    schema.kind,
                    ParamKind::String | ParamKind::Menu { .. } | ParamKind::Path
                )
                && let Ok(parsed) = serde_json::from_str::<Value>(text.trim())
                && !parsed.is_string()
            {
                p.value = parsed;
            }
        }
        if let Some(e) = a.get("expression").and_then(Value::as_str) {
            p.expr = (!e.trim().is_empty()).then(|| e.to_owned());
        }
        if let Some(b) = a.get("bind").and_then(Value::as_object) {
            let src = find(
                &net,
                b.get("node")
                    .and_then(Value::as_str)
                    .ok_or("bind needs `node`")?,
            )?;
            let ch = b
                .get("channel")
                .and_then(Value::as_str)
                .ok_or("bind needs `channel`")?;
            p = Param::bound(p.value.clone(), src, ch);
        }
        let edit = Edit::SetParam {
            node: id,
            param: param.to_owned(),
            value: Some(p.clone()),
        };
        let note = format!("{}.{param} = {}", node.name, describe(&p));
        if self.lock.engaged {
            // Show time: only whitelisted moves, in range, applied at once.
            if p.expr.is_some() || p.bind.is_some() {
                return Err(Refusal::GraphEdit.to_string());
            }
            let pack = node.type_id.strip_prefix(atd_core::pack::TYPE_PREFIX);
            self.lock
                .check_param(&node.name, pack, param, p.value.as_f64())
                .map_err(|e| e.to_string())?;
            ws.apply(&format!("Agent: {note}"), vec![edit])?;
            return Ok(Output::Json(json!({"applied": true})));
        }
        if !self.scopes.contains(&Scope::Edit) {
            return Err(Refusal::Scope("edit").to_string());
        }
        self.stage(ws, net, vec![edit], note)?;
        Ok(Output::Json(json!({"staged": true})))
    }

    /// `create_pack`: Make Pack (`atd_core::pack::make_pack`) on the preview,
    /// with the interface the agent asked for, staged like any other edit.
    fn create_pack(
        &mut self,
        ws: &mut dyn Workspace,
        a: &Map<String, Value>,
    ) -> Result<Output, String> {
        self.lock.check_graph_edit().map_err(|e| e.to_string())?;
        let str_arg = |k: &str| a.get(k).and_then(Value::as_str);
        let name = str_arg("name").ok_or("missing argument `name`")?;
        if !pack::valid_pack_name(name) {
            return Err(format!(
                "`{name}` is not a Pack name: lower-case letters, digits and `_`, starting with a letter, at most {} long",
                pack::MAX_NAME_LEN
            ));
        }
        let type_id = pack::type_id(name);
        if self.reg(ws).get(&type_id).is_some() {
            return Err(format!(
                "an operator `{type_id}` exists; pick another Pack name"
            ));
        }
        let mut net = self.view(ws).clone();
        let mut selection = BTreeSet::new();
        for r in a
            .get("nodes")
            .and_then(Value::as_array)
            .ok_or("missing argument `nodes`")?
        {
            let r = r.as_str().ok_or("`nodes` lists node names")?;
            selection.insert(find(&net, r)?);
        }
        let promote_args: Vec<&Map<String, Value>> = a
            .get("promote")
            .and_then(Value::as_array)
            .map(|v| v.iter().filter_map(Value::as_object).collect())
            .unwrap_or_default();
        let mut promote: Vec<ParamTarget> = Vec::new();
        for p in &promote_args {
            let node = p
                .get("node")
                .and_then(Value::as_str)
                .ok_or("each `promote` entry needs `node`")?;
            let param = p
                .get("param")
                .and_then(Value::as_str)
                .ok_or("each `promote` entry needs `param`")?;
            let id = find(&net, node)?;
            if !selection.contains(&id) {
                return Err(format!("`{node}` is not one of `nodes`"));
            }
            let t = ParamTarget::new(id, param);
            if promote.contains(&t) {
                return Err(format!("{node}.{param} is promoted twice"));
            }
            promote.push(t);
        }
        let instance = net.alloc_id();
        let made = pack::make_pack(&net, self.reg(ws), &selection, name, &promote, instance)
            .map_err(|e| e.to_string())?;
        let mut def = made.def;
        let mut edits = made.edits;
        if let Some(v) = str_arg("version") {
            def.version = v
                .parse::<Version>()
                .map_err(|e| format!("`version`: {e}"))?;
        }
        for (field, key) in [
            (&mut def.label, "label"),
            (&mut def.summary, "summary"),
            (&mut def.help, "help"),
        ] {
            if let Some(v) = str_arg(key) {
                v.clone_into(field);
            }
        }
        // `make_pack` numbers inner nodes in selection order and puts the
        // chosen promotions first, in the order asked for.
        let inner =
            |id: NodeId| NodeId(selection.iter().position(|s| *s == id).unwrap_or(0) as u64);
        for (p, t) in promote_args.iter().zip(&promote) {
            let target = ParamTarget::new(inner(t.node), t.param.clone());
            let promoted = def
                .params
                .iter_mut()
                .find(|q| q.targets.contains(&target))
                .ok_or("a promoted parameter went missing")?;
            let text = |k: &str| p.get(k).and_then(Value::as_str);
            if let Some(n) = text("name") {
                n.clone_into(&mut promoted.name);
            }
            if let Some(l) = text("label") {
                l.clone_into(&mut promoted.label);
            }
            if let Some(h) = text("help") {
                h.clone_into(&mut promoted.help);
            }
            promoted.min = p.get("min").and_then(Value::as_f64).or(promoted.min);
            promoted.max = p.get("max").and_then(Value::as_f64).or(promoted.max);
        }
        let mut instance_name = String::new();
        for e in &mut edits {
            if let Edit::AddNode { node } = e
                && node.id == instance
            {
                if let Some(n) = str_arg("instance") {
                    if net
                        .node_by_name(n)
                        .is_some_and(|o| !selection.contains(&o.id))
                    {
                        return Err(format!("a node named `{n}` exists; pick another name"));
                    }
                    n.clone_into(&mut node.name);
                }
                instance_name.clone_from(&node.name);
            }
        }
        let schema = def.schema(self.reg(ws)).map_err(|e| e.to_string())?;
        let mut reg = self.reg(ws).clone();
        reg.register(schema).map_err(|e| e.to_string())?;
        let previous = self.preview_registry.replace(reg);
        let note = format!(
            "make Pack {name} {} from {} node(s) → {instance_name}",
            def.version,
            selection.len()
        );
        if let Err(e) = self.stage(ws, net, edits, note) {
            self.preview_registry = previous;
            return Err(e);
        }
        let out = json!({
            "pack": name,
            "type": type_id,
            "version": def.version.to_string(),
            "instance": instance_name,
            "params": def.params.iter().map(|p| &p.name).collect::<Vec<_>>(),
            "inputs": def.inputs.iter().map(|i| &i.name).collect::<Vec<_>>(),
            "outputs": def.outputs.iter().map(|o| &o.name).collect::<Vec<_>>(),
            "warnings": made.warnings,
            "staged": true,
        });
        self.staged_packs.push(def);
        Ok(Output::Json(out))
    }

    /// Applies `edits` to the preview (checking them against the registry)
    /// and records them.
    fn stage(
        &mut self,
        ws: &dyn Workspace,
        mut net: Network,
        edits: Vec<Edit>,
        note: String,
    ) -> Result<(), String> {
        let reg = self.reg(ws);
        for e in &edits {
            net.apply(reg, e.clone()).map_err(|e| e.to_string())?;
        }
        self.preview = Some(net);
        self.staged.extend(edits);
        self.notes.push(note);
        Ok(())
    }

    /// Applies the cue moves due at `beat`, each one undo step, checked
    /// against the show and the lock as they are now. A host calls this as
    /// its clock advances.
    pub fn fire_cues(
        &mut self,
        ws: &mut dyn Workspace,
        beat: f64,
    ) -> Vec<(Move, Result<f64, String>)> {
        self.cues
            .due(beat)
            .into_iter()
            .map(|m| {
                let r = cues::apply_move(ws, &self.lock, &m);
                (m, r)
            })
            .collect()
    }

    /// Applies the staged changes as one undo step and cooks.
    pub fn accept(
        &mut self,
        ws: &mut dyn Workspace,
        label: &str,
    ) -> Result<Vec<(NodeId, String)>, String> {
        self.lock.check_graph_edit().map_err(|e| e.to_string())?;
        if self.staged.is_empty() {
            return Ok(ws.cook_errors());
        }
        let (packs, edits, _) = self.take_staged_with_packs();
        if packs.is_empty() {
            ws.apply(label, edits)?;
        } else {
            ws.apply_with_packs(label, packs, edits)?;
        }
        Ok(ws.cook_errors())
    }
}

impl Agent {
    /// `build_network`: the nodes, then their parameters, then the wires,
    /// each through the single-step tools so the same checks apply; one
    /// failure does not stop the rest.
    fn build_network(
        &mut self,
        ws: &mut dyn Workspace,
        a: &Map<String, Value>,
    ) -> Result<Output, String> {
        self.lock.check_graph_edit().map_err(|e| e.to_string())?;
        let mut made = Vec::new();
        let mut errors = Vec::new();
        let args = |v: Value| v.as_object().cloned().unwrap_or_default();
        for n in a
            .get("nodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let (Some(t), Some(name)) = (n["type"].as_str(), n["name"].as_str()) else {
                errors.push(format!("node {n}: needs `type` and `name`"));
                continue;
            };
            let existing = self.view(ws).node_by_name(name).map(|x| x.type_id.clone());
            match existing {
                Some(e) if e == t => {}
                Some(e) => {
                    errors.push(format!("node `{name}` exists as `{e}`; use another name"));
                    continue;
                }
                None => {
                    if let Err(e) =
                        self.run(ws, "create_node", &args(json!({"type": t, "name": name})))
                    {
                        errors.push(format!("node `{name}`: {e}"));
                        continue;
                    }
                }
            }
            made.push(name.to_owned());
            let kinds: Vec<(String, bool)> = self.reg(ws).get(t).map_or(Vec::new(), |op| {
                op.params
                    .iter()
                    .map(|p| {
                        let text = matches!(
                            p.kind,
                            ParamKind::String | ParamKind::Menu { .. } | ParamKind::Path
                        );
                        (p.name.clone(), text)
                    })
                    .collect()
            });
            for (p, v) in n["params"].as_object().into_iter().flatten() {
                let text = kinds.iter().find(|k| &k.0 == p).is_some_and(|k| k.1);
                let expression = v
                    .as_str()
                    .filter(|e| !text && serde_json::from_str::<Value>(e.trim()).is_err());
                // A bare word ("noise1", "on") for a number is a mistake, not
                // an expression worth evaluating.
                if let Some(word) = expression.filter(|e| {
                    let e = e.trim();
                    !matches!(e, "time" | "frame")
                        && e.chars()
                            .next()
                            .is_some_and(|c| c.is_alphabetic() || c == '_')
                        && e.chars().all(|c| c.is_alphanumeric() || c == '_')
                }) {
                    errors.push(format!(
                        "{name}.{p}: `{word}` is not a value for this parameter; give a number, true/false, or an expression such as `0.5 + 0.5 * sin(time)`"
                    ));
                    continue;
                }
                let call = match expression {
                    Some(e) => json!({"node": name, "param": p, "expression": e}),
                    None => json!({"node": name, "param": p, "value": v}),
                };
                if let Err(e) = self.run(ws, "set_param", &args(call)) {
                    errors.push(format!("{name}.{p}: {e}"));
                }
            }
        }
        let mut wired = 0;
        for w in a
            .get("wires")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let call = json!({
                "from": w["from"], "to": w["to"], "input": w["input"],
                "output": w.get("output").cloned().unwrap_or(json!("out")),
            });
            match self.run(ws, "connect", &args(call)) {
                Ok(_) => wired += 1,
                Err(e) => errors.push(format!(
                    "wire {}.{} → {}.{}: {e}",
                    w["from"].as_str().unwrap_or("?"),
                    w["output"].as_str().unwrap_or("out"),
                    w["to"].as_str().unwrap_or("?"),
                    w["input"].as_str().unwrap_or("?")
                )),
            }
        }
        let next = if errors.is_empty() {
            "call accept_changes"
        } else {
            "fix these errors (call build_network again with the same names), then call accept_changes"
        };
        Ok(Output::Json(json!({
            "nodes": made, "wires": wired, "errors": errors, "staged": true, "next": next,
        })))
    }
}

/// Finds a node by name, or by id written `n12` / `12`.
fn find(net: &Network, r: &str) -> Result<NodeId, String> {
    if let Some(n) = net.node_by_name(r) {
        return Ok(n.id);
    }
    let digits = r.strip_prefix('n').unwrap_or(r);
    if let Ok(i) = digits.parse::<u64>()
        && net.node(NodeId(i)).is_some()
    {
        return Ok(NodeId(i));
    }
    let names: Vec<&str> = net.nodes().map(|n| n.name.as_str()).collect();
    let hint = if r.contains('.') {
        " (that is an operator type, not a node: create a node of that type with build_network or create_node, then wire it by the node's name)"
    } else {
        ""
    };
    Err(if names.is_empty() {
        format!("no node named `{r}`: the network is empty; create it first{hint}")
    } else if !hint.is_empty() {
        format!("no node named `{r}`{hint}; nodes are: {}", names.join(", "))
    } else {
        format!("no node named `{r}`; nodes are: {}", names.join(", "))
    })
}

/// The error for an operator type that does not exist, with the types that
/// share its last part (`ai.noise` → `img.noise`) so a model can correct
/// itself.
fn unknown_type(registry: &Registry, t: &str) -> String {
    let tail = t
        .rsplit(['.', '_'])
        .next()
        .unwrap_or(t)
        .to_ascii_lowercase();
    let near: Vec<&str> = registry
        .iter()
        .map(|o| o.type_id.as_str())
        .filter(|id| !tail.is_empty() && id.to_ascii_lowercase().contains(&tail))
        .take(8)
        .collect();
    if near.is_empty() {
        format!("no operator type `{t}`; see list_ops")
    } else {
        format!(
            "no operator type `{t}`; did you mean {}?",
            near.iter()
                .map(|n| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn family_name(f: OpFamily) -> &'static str {
    f.prefix()
}

fn describe(p: &Param) -> String {
    match (&p.expr, &p.bind) {
        (Some(e), _) => format!("`{e}`"),
        (_, Some(b)) => format!("bound to {}['{}']", b.node, b.channel),
        _ => p.value.to_string(),
    }
}

/// An operator's schema for a model.
pub fn schema_json(op: &OperatorSchema) -> Value {
    let params: Vec<Value> = op
        .params
        .iter()
        .map(|p| {
            let mut v = serde_json::to_value(p).unwrap_or(Value::Null);
            if let Some(o) = v.as_object_mut() {
                o.retain(|k, _| {
                    matches!(
                        k.as_str(),
                        "name" | "kind" | "default" | "help" | "multiline"
                    )
                });
            }
            v
        })
        .collect();
    let sockets = |s: &[atd_core::registry::SocketSchema]| -> Vec<Value> {
        s.iter()
            .map(|s| json!({"name": s.name, "wire": format!("{:?}", s.wire).to_uppercase(), "optional": s.optional}))
            .collect()
    };
    json!({
        "type": op.type_id,
        "label": op.label,
        "summary": op.summary,
        "params": params,
        "inputs": sockets(&op.inputs),
        "outputs": sockets(&op.outputs),
        "capabilities": op.capabilities.iter().map(|c| c.flag.to_string()).collect::<Vec<_>>(),
        "cook_budget_us": op.cook_budget_us,
    })
}

fn network_json(net: &Network, ws: &dyn Workspace, errors: &[(NodeId, String)]) -> Value {
    let nodes: Vec<Value> = net
        .nodes()
        .map(|n| {
            let params: Map<String, Value> = n
                .params
                .iter()
                .map(|(k, p)| (k.clone(), json!(describe(p))))
                .collect();
            let inputs: Map<String, Value> = net
                .inputs_of(n.id)
                .map(|(i, src)| {
                    let from = net.node(src.node).map_or("?".to_owned(), |s| s.name.clone());
                    (i.to_owned(), json!(format!("{from}.{}", src.socket)))
                })
                .collect();
            let mut v = json!({"name": n.name, "id": n.id.to_string(), "type": n.type_id, "params": params, "inputs": inputs});
            if let Some((_, e)) = errors.iter().find(|(id, _)| *id == n.id) {
                v["error"] = json!(e);
            }
            if let Some(s) = ws.status(n.id) {
                v["status"] = json!(s);
            }
            v
        })
        .collect();
    json!({"nodes": nodes})
}

fn errors_json(ws: &dyn Workspace, errors: &[(NodeId, String)]) -> Value {
    let net = ws.network();
    json!(
        errors
            .iter()
            .map(|(id, e)| json!({"node": net.node(*id).map_or(id.to_string(), |n| n.name.clone()), "error": e}))
            .collect::<Vec<_>>()
    )
}

fn stage_json(ws: &dyn Workspace) -> Value {
    let stage = atd_stage::Stage::read(ws.network(), ws.registry());
    let decks: Vec<Value> = stage
        .decks
        .iter()
        .map(|d| {
            let layers: Vec<Value> = d
                .layers
                .iter()
                .map(|l| {
                    let clips: Vec<Value> = l
                        .clips
                        .iter()
                        .map(|c| json!({"slot": c.slot, "clip": c.label}))
                        .collect();
                    json!({"layer": l.name, "playing": l.target, "clips": clips})
                })
                .collect();
            json!({"mixer": d.name, "crossfader": d.crossfader, "layers": layers})
        })
        .collect();
    json!({"decks": decks})
}

/// A short text form of a tool's output (for logs and plain transports).
pub fn output_text(o: &Output) -> String {
    match o {
        Output::Json(v) => serde_json::to_string_pretty(v).unwrap_or_default(),
        Output::Png(p, what) => {
            let mut s = String::new();
            let _ = write!(s, "[{what}: PNG, {} bytes]", p.len());
            s
        }
        Output::Error(e) => format!("error: {e}"),
    }
}
