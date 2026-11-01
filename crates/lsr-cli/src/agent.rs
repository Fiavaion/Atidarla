//! `lasair agent` (FR-G4, M5 AC1): the assistant from the command line.
//! `ask` builds one request into a project; `eval` runs the network-
//! generation eval (`docs/milestones/m5/eval-prompts.toml`) on fresh
//! projects and writes a Markdown report.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use lsr_agent::assistant::{self, Anthropic, Llm, Ollama};
use lsr_agent::lock::Scope;
use lsr_agent::{Agent, ShowLock, Workspace};
use serde::Deserialize;

use crate::serve::ServeWorkspace;

#[derive(Debug, clap::Subcommand)]
pub enum AgentCommand {
    /// Build a request into a project (changes are accepted and saved).
    Ask {
        project: PathBuf,
        request: String,
        #[command(flatten)]
        model: ModelArgs,
        /// Run the project's Python (ops/*.py operators, Script nodes,
        /// Python expressions); see docs/python-ops.md.
        #[arg(long)]
        allow_python_ops: bool,
    },
    /// Run the network-generation eval and write a report.
    Eval {
        #[arg(long, default_value = "docs/milestones/m5/eval-prompts.toml")]
        prompts: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
        /// Only prompts whose id contains this.
        #[arg(long)]
        only: Option<String>,
        #[command(flatten)]
        model: ModelArgs,
        /// Evaluate the Python expressions the model writes (they are code
        /// with your user's privileges); without it they keep their stored
        /// values and count as node errors. See docs/python-ops.md.
        #[arg(long)]
        allow_python_ops: bool,
    },
}

#[derive(Debug, clap::Args)]
pub struct ModelArgs {
    /// `ollama` (default, local) or `anthropic` (opt-in; ANTHROPIC_API_KEY,
    /// refused in Sovereign mode unless --allow-internet).
    #[arg(long, default_value = "ollama")]
    pub provider: String,
    #[arg(long, default_value = "qwen2.5-coder:7b")]
    pub model: String,
    /// Model turns per request.
    #[arg(long, default_value_t = 40)]
    pub max_steps: u32,
    /// Turn Sovereign mode off for this run (cloud providers).
    #[arg(long)]
    pub allow_internet: bool,
}

fn llm(m: &ModelArgs) -> Result<Box<dyn Llm>> {
    lsr_ai::egress::set_sovereign(!m.allow_internet);
    Ok(match m.provider.as_str() {
        "ollama" => Box::new(Ollama::new(&m.model)),
        "anthropic" => Box::new(Anthropic {
            key: std::env::var("ANTHROPIC_API_KEY").context("ANTHROPIC_API_KEY is not set")?,
            model: m.model.clone(),
        }),
        other => bail!("unknown provider {other:?}"),
    })
}

fn scopes() -> BTreeSet<Scope> {
    Scope::defaults()
        .into_iter()
        .chain([Scope::Accept])
        .collect()
}

#[derive(Debug, Deserialize)]
struct Prompts {
    prompt: Vec<Prompt>,
}

#[derive(Debug, Deserialize)]
struct Prompt {
    id: String,
    text: String,
    #[serde(default)]
    require: Vec<String>,
    #[serde(default = "one")]
    min_nodes: usize,
    /// Something must move: a parameter with an expression or a channel
    /// binding, or a SIG node wired into another node.
    #[serde(default)]
    animated: bool,
}

fn one() -> usize {
    1
}

pub fn run(cmd: AgentCommand) -> Result<()> {
    match cmd {
        AgentCommand::Ask {
            project,
            request,
            model,
            allow_python_ops,
        } => {
            let mut llm = llm(&model)?;
            let mut ws = ServeWorkspace::open(&project, true, allow_python_ops)?;
            let lock = ShowLock::load(&project).map_err(|e| anyhow!(e))?;
            let mut agent = Agent::new(scopes(), lock);
            let run =
                assistant::build(llm.as_mut(), &mut agent, &mut ws, &request, model.max_steps);
            println!(
                "{} steps, {} tool calls ({} refused), {} nodes, errors: {:?}",
                run.steps,
                run.tool_calls,
                run.tool_errors,
                ws.network().len(),
                run.errors
            );
            if let Some(e) = run.llm_error {
                bail!(e);
            }
            Ok(())
        }
        AgentCommand::Eval {
            prompts,
            out,
            only,
            model,
            allow_python_ops,
        } => eval(
            &prompts,
            out.as_deref(),
            only.as_deref(),
            &model,
            allow_python_ops,
        ),
    }
}

fn eval(
    path: &Path,
    out: Option<&Path>,
    only: Option<&str>,
    m: &ModelArgs,
    allow_python: bool,
) -> Result<()> {
    let text = std::fs::read_to_string(path).with_context(|| path.display().to_string())?;
    let prompts: Prompts = toml_edit::de::from_str(&text)?;
    let mut llm = llm(m)?;
    let tmp = std::env::temp_dir().join(format!("lsr-eval-{}", std::process::id()));
    let mut rows = String::new();
    let (mut valid, mut total) = (0, 0);
    let start = Instant::now();
    for p in prompts
        .prompt
        .iter()
        .filter(|p| only.is_none_or(|o| p.id.contains(o)))
    {
        total += 1;
        let dir = tmp.join(format!("{}.lasair", p.id));
        let _ = std::fs::remove_dir_all(&dir);
        let t = Instant::now();
        let mut ws = ServeWorkspace::open(&dir, false, allow_python)?;
        let mut agent = Agent::new(scopes(), ShowLock::default());
        let run = assistant::build(llm.as_mut(), &mut agent, &mut ws, &p.text, m.max_steps);
        // Judge the network the model left, cooked afresh.
        let errors = ws.cook_errors();
        let net = ws.network();
        let types: Vec<&str> = net.nodes().map(|n| n.type_id.as_str()).collect();
        let missing: Vec<&str> = p
            .require
            .iter()
            .filter(|r| !types.iter().any(|t| t.starts_with(r.as_str())))
            .map(String::as_str)
            .collect();
        let mut why = Vec::new();
        if net.len() < p.min_nodes {
            why.push(format!("{} nodes < {}", net.len(), p.min_nodes));
        }
        if !missing.is_empty() {
            why.push(format!("missing {}", missing.join(", ")));
        }
        if !errors.is_empty() {
            why.push(format!("{} node error(s): {}", errors.len(), errors[0].1));
        }
        // The motion must reach a node that is not itself a channel node
        // (an LFO with an expression that drives nothing does not count).
        let sig = |id| {
            net.node(id)
                .is_some_and(|n: &lsr_core::graph::Node| n.type_id.starts_with("sig."))
        };
        let moves = net.nodes().filter(|n| !sig(n.id)).any(|n| {
            n.params
                .values()
                .any(|p| p.expr.is_some() || p.bind.is_some())
        }) || net.wires().any(|w| sig(w.from.node) && !sig(w.to.node));
        if p.animated && !moves {
            why.push("nothing is animated".into());
        }
        if let Some(e) = &run.llm_error {
            why.push(format!("model error: {e}"));
        }
        let ok = why.is_empty();
        valid += usize::from(ok);
        let mut kinds: Vec<&str> = types.clone();
        kinds.sort_unstable();
        kinds.dedup();
        let _ = writeln!(
            rows,
            "| {} | {} | {} | {} | {}/{} | {:.0} s | {} |",
            p.id,
            if ok { "valid" } else { "**invalid**" },
            net.len(),
            kinds.join(", "),
            run.tool_errors,
            run.tool_calls,
            t.elapsed().as_secs_f64(),
            why.join("; ").replace('|', "/")
        );
        eprintln!(
            "{:>20}: {}",
            p.id,
            if ok {
                "valid".to_owned()
            } else {
                why.join("; ")
            }
        );
    }
    let _ = std::fs::remove_dir_all(&tmp);
    let pct = 100.0 * valid as f64 / total.max(1) as f64;
    let report = format!(
        "# Network-generation eval\n\n**Model:** `{}` · **Prompts:** `{}` · **Valid:** {valid}/{total} (**{pct:.0}%**) · **Time:** {:.0} s · **Max turns:** {}\n\nValid = at least `min_nodes` nodes, every required type present, every node cooks without an error (judged on a fresh cook after the assistant stops), and for prompts marked animated something outside the channel nodes moves (an expression or a channel binding on it, or a SIG node wired into it).\n\n| Prompt | Result | Nodes | Types | Refused calls / calls | Time | Why |\n|---|---|---|---|---|---|---|\n{rows}",
        llm.name(),
        path.display(),
        start.elapsed().as_secs_f64(),
        m.max_steps
    );
    match out {
        Some(o) => std::fs::write(o, &report)?,
        None => print!("{report}"),
    }
    println!("{valid}/{total} valid ({pct:.0}%)");
    Ok(())
}
