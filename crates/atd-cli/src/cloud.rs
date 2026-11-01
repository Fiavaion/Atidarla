//! `atidarla cloud runpod up|status|down` (A59, docs/cloud.md).
//!
//! These commands exist to reach Runpod, so they turn Sovereign mode off for
//! this process (it is a per-project setting; no project is open here).

use std::path::PathBuf;

use anyhow::{Result, anyhow};
use atd_ai::cloud::runpod::{HOST_PORT, Phase, PodSpec, RUNPOD_API, RunpodClient, RunpodHost};
use atd_ai::cloud::secrets_path;
use atd_ai::supervisor::Supervisor;
use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub enum CloudCommand {
    /// A self-hosted Runpod pod running the AI Host container.
    Runpod {
        #[command(subcommand)]
        action: RunpodCommand,
    },
}

#[derive(Debug, Args)]
pub struct PodArgs {
    /// The host's name: the pod is `atd-<name>`, the AI host `cloud:<name>`.
    #[arg(long)]
    pub name: String,
    /// Runpod API base URL.
    #[arg(long, default_value = RUNPOD_API, hide = true)]
    pub api: String,
    /// Secrets file holding `[runpod] api_key` and the pods' host tokens
    /// (default: secrets.toml in the app's settings folder). RUNPOD_API_KEY wins.
    #[arg(long)]
    pub secrets: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum RunpodCommand {
    /// Create the pod (or resume a stopped one) and wait until its AI Host
    /// port is reachable. The pod then bills by the hour until `down`.
    Up {
        #[command(flatten)]
        pod: PodArgs,
        /// GPU type ID, as Runpod names it.
        #[arg(long, default_value = "NVIDIA GeForce RTX 4090")]
        gpu: String,
        /// Container image built from ai-host/Dockerfile (needed to create).
        #[arg(long)]
        image: Option<String>,
        /// SECURE or COMMUNITY.
        #[arg(long, default_value = "SECURE")]
        cloud_type: String,
        /// Container disk in GB.
        #[arg(long, default_value_t = 50)]
        disk_gb: u32,
    },
    /// Show the pod's state, address and price.
    Status {
        #[command(flatten)]
        pod: PodArgs,
    },
    /// Stop the pod (GPU billing stops; the disk is kept for a fast `up`).
    Down {
        #[command(flatten)]
        pod: PodArgs,
        /// Delete the pod and its disk instead of stopping it.
        #[arg(long)]
        terminate: bool,
    },
}

fn host(pod: &PodArgs, spec: PodSpec) -> Result<RunpodHost> {
    let secrets = pod.secrets.clone().unwrap_or_else(secrets_path);
    let key = RunpodClient::key(&secrets)?;
    Ok(RunpodHost::new(
        RunpodClient::new(&pod.api, &key),
        spec,
        secrets,
    ))
}

pub fn run(cmd: CloudCommand) -> Result<()> {
    atd_ai::egress::set_sovereign(false);
    let CloudCommand::Runpod { action } = cmd;
    let supervisor = Supervisor::new();
    match action {
        RunpodCommand::Up {
            pod,
            gpu,
            image,
            cloud_type,
            disk_gb,
        } => {
            let mut spec = PodSpec::new(&pod.name, image.as_deref().unwrap_or_default());
            spec.gpu_type = gpu;
            spec.cloud_type = cloud_type;
            spec.container_disk_gb = disk_gb;
            let h = host(&pod, spec)?;
            let ready = h.up(&supervisor, &|phase| match phase {
                Phase::Looking => eprintln!("looking for pod atd-{}…", pod.name),
                Phase::Creating => eprintln!("creating the pod…"),
                Phase::Resuming { pod_id } => eprintln!("resuming pod {pod_id}…"),
                Phase::Waiting { pod_id, status } => {
                    eprintln!("waiting for pod {pod_id} ({status}) to map port {HOST_PORT}…");
                }
                Phase::Ready(_) | Phase::Failed(_) => {}
            })?;
            println!("cloud:{} ready at {}", pod.name, ready.addr);
            println!(
                "pod {} is RUNNING at ${:.2}/h until you stop it: atidarla cloud runpod down --name {}",
                ready.pod_id, ready.usd_per_hour, pod.name
            );
        }
        RunpodCommand::Status { pod } => {
            let h = host(&pod, PodSpec::new(&pod.name, ""))?;
            match h.status()? {
                None => println!("no pod named atd-{}", pod.name),
                Some(p) => {
                    let addr = p.endpoint(HOST_PORT).unwrap_or_else(|| "-".into());
                    let price = p
                        .cost_per_hr
                        .map_or_else(|| "?".into(), |c| format!("${c:.2}/h"));
                    println!("{} {} {addr} {price}", p.id, p.desired_status);
                }
            }
        }
        RunpodCommand::Down { pod, terminate } => {
            let h = host(&pod, PodSpec::new(&pod.name, ""))?;
            let id = if terminate {
                h.terminate(&supervisor)?
            } else {
                h.down(&supervisor)?
            };
            match id {
                Some(id) if terminate => println!("terminated pod {id}"),
                Some(id) => {
                    println!("stopped pod {id} (its disk is kept; `down --terminate` deletes it)")
                }
                None => return Err(anyhow!("no pod named atd-{}", pod.name)),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct T {
        #[command(subcommand)]
        cmd: CloudCommand,
    }

    #[test]
    fn the_commands_parse() {
        let t = T::parse_from([
            "t",
            "runpod",
            "up",
            "--name",
            "studio",
            "--image",
            "ghcr.io/x/atd:1",
            "--gpu",
            "NVIDIA RTX A6000",
        ]);
        let CloudCommand::Runpod {
            action: RunpodCommand::Up {
                pod, gpu, image, ..
            },
        } = t.cmd
        else {
            panic!("up");
        };
        assert_eq!(pod.name, "studio");
        assert_eq!(pod.api, RUNPOD_API);
        assert_eq!(gpu, "NVIDIA RTX A6000");
        assert_eq!(image.as_deref(), Some("ghcr.io/x/atd:1"));
        let t = T::parse_from(["t", "runpod", "down", "--name", "studio", "--terminate"]);
        assert!(matches!(
            t.cmd,
            CloudCommand::Runpod {
                action: RunpodCommand::Down {
                    terminate: true,
                    ..
                }
            }
        ));
        assert!(T::try_parse_from(["t", "runpod", "status"]).is_err());
    }
}
