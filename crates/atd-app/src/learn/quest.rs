//! The quest file format (docs/learn.md) and the shipped quests.

use toml_edit::{DocumentMut, Item};

use super::check::Check;

/// The shipped quests, in order. A test keeps this equal to the files in
/// `learn/quests/`.
pub const SHIPPED: [(&str, &str); 10] = [
    (
        "01-first-patch",
        include_str!("../../../../learn/quests/01-first-patch.toml"),
    ),
    (
        "02-audio-reactive",
        include_str!("../../../../learn/quests/02-audio-reactive.toml"),
    ),
    (
        "03-first-clip-deck",
        include_str!("../../../../learn/quests/03-first-clip-deck.toml"),
    ),
    (
        "04-kernel-dsl",
        include_str!("../../../../learn/quests/04-kernel-dsl.toml"),
    ),
    (
        "05-particles",
        include_str!("../../../../learn/quests/05-particles.toml"),
    ),
    (
        "06-ai-clip",
        include_str!("../../../../learn/quests/06-ai-clip.toml"),
    ),
    (
        "07-mapping",
        include_str!("../../../../learn/quests/07-mapping.toml"),
    ),
    (
        "08-dmx",
        include_str!("../../../../learn/quests/08-dmx.toml"),
    ),
    (
        "09-packs",
        include_str!("../../../../learn/quests/09-packs.toml"),
    ),
    (
        "10-comfyui",
        include_str!("../../../../learn/quests/10-comfyui.toml"),
    ),
];

/// One step: what to do, and the check that says it is done.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub title: String,
    /// Markdown (a small subset, see `markdown`).
    pub text: String,
    pub hint: Option<String>,
    pub check: Check,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Quest {
    /// The file stem, `01-first-patch`: the key progress is stored under.
    pub id: String,
    pub title: String,
    pub summary: String,
    pub minutes: u32,
    /// Ids (without the number) of quests worth doing first.
    pub prerequisites: Vec<String>,
    /// An example project (relative to the Atidarla folder) the quest
    /// starts from, instead of an empty project.
    pub start: Option<String>,
    pub steps: Vec<Step>,
}

impl Quest {
    /// The id without its number: `first-patch`.
    pub fn slug(&self) -> &str {
        self.id.split_once('-').map_or(&self.id, |(_, s)| s)
    }

    /// Reads a quest file. `id` is the file stem.
    pub fn parse(id: &str, source: &str) -> Result<Self, String> {
        let doc: DocumentMut = source.parse().map_err(|e| format!("{id}: {e}"))?;
        let at = |msg: String| format!("{id}: {msg}");
        let known = [
            "title",
            "summary",
            "minutes",
            "prerequisites",
            "start",
            "step",
        ];
        if let Some((k, _)) = doc.iter().find(|(k, _)| !known.contains(k)) {
            return Err(at(format!("unknown key `{k}`")));
        }
        let string = |key: &str| {
            doc.get(key)
                .and_then(Item::as_str)
                .map(str::to_owned)
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| at(format!("needs a non-empty string `{key}`")))
        };
        let minutes = doc
            .get("minutes")
            .and_then(Item::as_integer)
            .filter(|m| (1..=240).contains(m))
            .ok_or_else(|| at("`minutes` must be a whole number from 1 to 240".into()))?
            as u32;
        let prerequisites = match doc.get("prerequisites") {
            None => Vec::new(),
            Some(item) => item
                .as_array()
                .and_then(|a| a.iter().map(|v| v.as_str().map(str::to_owned)).collect())
                .ok_or_else(|| at("`prerequisites` must be a list of quest names".into()))?,
        };
        let start = match doc.get("start") {
            None => None,
            Some(item) => Some(
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| at("`start` must be a string".into()))?,
            ),
        };
        let steps_item = doc
            .get("step")
            .and_then(Item::as_array_of_tables)
            .ok_or_else(|| at("needs at least one [[step]]".into()))?;
        let mut steps = Vec::new();
        for (i, t) in steps_item.iter().enumerate() {
            let n = i + 1;
            let step_at = |msg: String| at(format!("step {n}: {msg}"));
            if let Some((k, _)) = t
                .iter()
                .find(|(k, _)| !["title", "text", "hint", "check"].contains(k))
            {
                return Err(step_at(format!("unknown key `{k}`")));
            }
            let get = |key: &str| {
                t.get(key)
                    .and_then(Item::as_str)
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
            };
            let title = get("title").ok_or_else(|| step_at("needs a `title`".into()))?;
            let text = get("text").ok_or_else(|| step_at("needs `text`".into()))?;
            let hint = match t.get("hint") {
                None => None,
                Some(_) => Some(get("hint").ok_or_else(|| step_at("`hint` is empty".into()))?),
            };
            let table = t
                .get("check")
                .and_then(Item::as_table_like)
                .ok_or_else(|| step_at("needs a `check` table".into()))?;
            let check = Check::parse(table).map_err(step_at)?;
            steps.push(Step {
                title,
                text,
                hint,
                check,
            });
        }
        if steps.is_empty() {
            return Err(at("needs at least one [[step]]".into()));
        }
        Ok(Quest {
            id: id.to_owned(),
            title: string("title")?,
            summary: string("summary")?,
            minutes,
            prerequisites,
            start,
            steps,
        })
    }

    /// Every operator and parameter the quest names must exist in
    /// `registry`. A prefix (`comfy.*`) must match at least one operator
    /// there, so the caller registers the project operators it names.
    #[cfg(test)]
    pub fn check_against(&self, registry: &atd_core::registry::Registry) -> Vec<String> {
        use super::check::{ParamOp, TypePat};
        use atd_core::registry::{OperatorSchema, ParamKind};
        let mut problems = Vec::new();
        for (i, step) in self.steps.iter().enumerate() {
            let mut bad = |msg: String| problems.push(format!("{} step {}: {msg}", self.id, i + 1));
            let schemas = |pat: &TypePat, bad: &mut dyn FnMut(String)| -> Vec<&OperatorSchema> {
                let found: Vec<_> = registry
                    .iter()
                    .filter(|s| pat.matches(&s.type_id))
                    .collect();
                if found.is_empty() {
                    bad(format!("no operator `{}`", pat.as_str()));
                }
                found
            };
            match &step.check {
                Check::NodeExists { ty } | Check::CooksWithoutError { ty } => {
                    schemas(ty, &mut bad);
                }
                Check::Wired { from, to, input } => {
                    if schemas(from, &mut bad).iter().all(|s| s.outputs.is_empty()) {
                        bad(format!("`{}` has no output", from.as_str()));
                    }
                    let to_schemas = schemas(to, &mut bad);
                    if let Some(input) = input
                        && !to_schemas.is_empty()
                        && !to_schemas
                            .iter()
                            .any(|s| s.inputs.iter().any(|x| x.name == *input))
                    {
                        bad(format!("`{}` has no input `{input}`", to.as_str()));
                    }
                }
                Check::Param { ty, name, op } => {
                    let found = schemas(ty, &mut bad);
                    if found.is_empty() {
                        continue;
                    }
                    let Some(p) = found
                        .iter()
                        .find_map(|s| s.params.iter().find(|p| p.name == *name))
                    else {
                        bad(format!("`{}` has no parameter `{name}`", ty.as_str()));
                        continue;
                    };
                    let numeric = matches!(p.kind, ParamKind::Float { .. } | ParamKind::Int { .. });
                    match op {
                        ParamOp::Gt(_) | ParamOp::Lt(_) if !numeric => {
                            bad(format!("`{name}` is not a number, gt/lt cannot compare it"));
                        }
                        ParamOp::Eq(v) => {
                            if let Err(e) = p.validate(v) {
                                bad(format!("`{name}` cannot be {v}: {e}"));
                            }
                        }
                        ParamOp::Contains(_) if !matches!(p.kind, ParamKind::String) => {
                            bad(format!("`{name}` is not text"));
                        }
                        _ => {}
                    }
                }
                Check::MappingExists { .. }
                | Check::StageLayerPlaying
                | Check::KernelCompiles
                | Check::PackInstance { .. }
                | Check::FileExists { .. } => {}
            }
        }
        problems
    }
}

/// Parses the shipped quests. A quest that fails to parse is reported by
/// the tests; at run time it is logged and left out, so one bad file cannot
/// take the panel down.
pub fn shipped() -> Vec<Quest> {
    let quests = parse_shipped();
    for problem in check_set(&quests) {
        log::error!("learn: {problem}");
    }
    quests
}

fn parse_shipped() -> Vec<Quest> {
    SHIPPED
        .iter()
        .filter_map(|(id, source)| match Quest::parse(id, source) {
            Ok(q) => Some(q),
            Err(e) => {
                log::error!("learn: {e}");
                None
            }
        })
        .collect()
}

/// Checks a set of quests as a whole: unique ids, prerequisites that name
/// an earlier quest.
pub fn check_set(quests: &[Quest]) -> Vec<String> {
    let mut problems = Vec::new();
    for (i, q) in quests.iter().enumerate() {
        if quests[..i]
            .iter()
            .any(|p| p.id == q.id || p.slug() == q.slug())
        {
            problems.push(format!("{}: id used twice", q.id));
        }
        for pre in &q.prerequisites {
            if !quests[..i].iter().any(|p| p.slug() == pre) {
                problems.push(format!(
                    "{}: prerequisite `{pre}` is not an earlier quest",
                    q.id
                ));
            }
        }
    }
    problems
}
