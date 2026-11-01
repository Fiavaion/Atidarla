//! Model manager (FR-A12, PRD A61; see `docs/models.md`).
//!
//! - A **catalogue** embedded in the crate ([`catalogue()`]): each model's
//!   licence (SPDX-style ID, class, URL), Hugging Face source pinned to a
//!   commit, and every file with its size and SHA-256.
//! - A per-user **cache** ([`models_root`]): one folder per model ID, never
//!   inside a project or the repo (CLAUDE.md rule 4).
//! - An **acceptance store** ([`Acceptances`], `accepted.toml` beside the
//!   cache): a model downloads only after its licence was accepted.
//! - **Status** ([`status`], a size check) and **verification** ([`verify`],
//!   full SHA-256, on a worker thread).
//! - A **downloader** ([`start_download`]): resumable `.part` files, checked
//!   before they are renamed, on its own thread, through the egress gate.
//! - **Project helpers** ([`missing`], [`allowed`]) over the `[[models]]`
//!   manifest and `[ai] commercial_safe_only` in `project.toml`.
//!
//! Nothing here runs on the render thread: disk and network work happens in
//! [`Job`]s, which the UI polls.

mod accept;
mod catalogue;
mod download;
mod paths;
mod status;

use std::collections::BTreeMap;
use std::path::PathBuf;

pub use accept::{Acceptance, Acceptances, today};
pub use catalogue::{Catalogue, Licence, Model, ModelFile, Source, catalogue, parse_catalogue};
pub use download::agent as gated_agent;
pub use download::{DownloadError, DownloadOptions, HUGGING_FACE, download, gate, start_download};
pub use lsr_format::{LicenceClass, ModelPin};
pub use paths::{Store, models_root, settings_dir};
pub use status::{FileCheck, FileState, Job, Progress, ProgressSnapshot, Status, VerifyReport};
pub use status::{start_verify, status, verify};

/// The folder of model `id` in the per-user cache (`models_root()/id`).
pub fn model_dir(id: &str) -> PathBuf {
    models_root().join(id)
}

/// The licence class of catalogue model `id`; `None` when the catalogue
/// has no such model (treat it as not allowed in a commercial-safe-only
/// project).
pub fn licence_class(id: &str) -> Option<LicenceClass> {
    catalogue().get(id).map(|m| m.licence.class)
}

/// What a project would record in `[[models]]` for `model`.
pub fn pin(model: &Model) -> ModelPin {
    ModelPin {
        licence_class: model.licence.class,
        sha256: model.main_file().sha256.clone(),
    }
}

/// Why a project's model cannot be used as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Missing {
    /// The catalogue has no model with this ID.
    Unknown,
    /// The catalogue's main file differs from the one the project records
    /// (a different revision of the model).
    OtherVersion { catalogue_sha256: String },
    /// Not (fully) in the model cache: `status` is not [`Status::Ready`].
    NotReady(Status),
}

/// The project's models that are not ready to use, by ID. Checks sizes
/// only (it reads file metadata, so call it off the render thread).
pub fn missing(
    store: &Store,
    cat: &Catalogue,
    models: &BTreeMap<String, ModelPin>,
) -> BTreeMap<String, Missing> {
    let mut out = BTreeMap::new();
    for (id, want) in models {
        let Some(model) = cat.get(id) else {
            out.insert(id.clone(), Missing::Unknown);
            continue;
        };
        let main = &model.main_file().sha256;
        if *main != want.sha256 {
            out.insert(
                id.clone(),
                Missing::OtherVersion {
                    catalogue_sha256: main.clone(),
                },
            );
            continue;
        }
        let s = status(store, model);
        if s != Status::Ready {
            out.insert(id.clone(), Missing::NotReady(s));
        }
    }
    out
}

/// Whether a project may use a model of this class.
pub fn allowed(commercial_safe_only: bool, class: LicenceClass) -> bool {
    !commercial_safe_only || class.is_commercial_safe()
}

/// The catalogue models a project may use: all of them, or only the
/// commercial-safe ones when the project says so.
pub fn visible(cat: &Catalogue, commercial_safe_only: bool) -> impl Iterator<Item = &Model> {
    cat.iter()
        .filter(move |m| allowed(commercial_safe_only, m.licence.class))
}

/// Models in the project manifest that a commercial-safe-only project must
/// not use, by ID (they stay listed, so turning the flag off restores them,
/// but nodes refuse to load them). The catalogue's class wins over the one
/// the project file records.
pub fn blocked<'a>(
    cat: &Catalogue,
    commercial_safe_only: bool,
    models: &'a BTreeMap<String, ModelPin>,
) -> Vec<&'a str> {
    models
        .iter()
        .filter(|(id, p)| {
            let class = cat.get(id).map_or(p.licence_class, |m| m.licence.class);
            !allowed(commercial_safe_only, class)
        })
        .map(|(id, _)| id.as_str())
        .collect()
}

#[cfg(test)]
mod tests;
