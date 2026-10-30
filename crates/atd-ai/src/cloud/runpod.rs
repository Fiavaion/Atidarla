//! A self-hosted Runpod pod running the AI Host container (A59, A60).
//!
//! [`RunpodClient`] speaks Runpod's REST API (`https://rest.runpod.io/v1`)
//! through the egress gate: in Sovereign mode it refuses before any packet.
//! [`RunpodHost`] provisions or resumes the pod for a `cloud:<name>` host,
//! waits for its public address, registers it with the [`Supervisor`] as a
//! remote host (frames over `inline`) and sets the spend meter's hourly rate
//! from the pod's price.
//!
//! **Stopping.** A running pod bills by the hour whether or not frames flow.
//! Every pod brought up in this process is recorded; [`RunpodHost::down`]
//! stops one and [`stop_all`] stops them all (the app calls it on exit;
//! `atidarla cloud runpod down` stops a pod from the command line). A pod is
//! never left running without the user having been told its hourly price.
//!
//! Blocking: run [`RunpodHost::up`] through [`spawn_up`] (a worker thread
//! with progress), never on the render thread (rule 1).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use ureq::Agent;

use crate::models::{DownloadOptions, gated_agent};
use crate::supervisor::{HostKind, HostSpec, Supervisor, random_token};

/// Runpod's REST API.
pub const RUNPOD_API: &str = "https://rest.runpod.io/v1";
/// The port the AI Host container listens on (`ai-host/Dockerfile`).
pub const HOST_PORT: u16 = 7420;

#[derive(Debug, thiserror::Error)]
pub enum RunpodError {
    #[error(
        "no Runpod API key: set RUNPOD_API_KEY, or `api_key` under [runpod] in {}",
        .0.display()
    )]
    NoKey(PathBuf),
    #[error("Sovereign mode blocks {host}: Runpod is on the internet")]
    Sovereign { host: String },
    #[error("Runpod request failed: {0}")]
    Transport(String),
    #[error("Runpod answered HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("unexpected Runpod answer: {0}")]
    Parse(String),
    #[error("pod {pod_id} was not reachable within {waited:?} (status {status})")]
    Timeout {
        pod_id: String,
        waited: Duration,
        status: String,
    },
    #[error(
        "pod {pod_id} exists but this machine does not know its host token; terminate it and bring it up again"
    )]
    NoToken { pod_id: String },
    #[error(
        "no pod exists yet and no container image was given: build and push ai-host/Dockerfile, then pass its image name"
    )]
    NoImage,
    #[error("cannot save the pod's host token: {0}")]
    Secrets(String),
}

/// A pod as Runpod reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Pod {
    pub id: String,
    pub name: String,
    /// `RUNNING`, `EXITED`, `TERMINATED`, …
    pub desired_status: String,
    /// Empty while the pod initialises.
    pub public_ip: Option<String>,
    /// Internal port → public port (random; changes when the pod restarts).
    pub port_mappings: BTreeMap<u16, u16>,
    pub cost_per_hr: Option<f64>,
}

impl Pod {
    fn from_json(v: &Value) -> Result<Self, RunpodError> {
        let id = v
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| RunpodError::Parse(format!("a pod without an id: {v}")))?;
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default();
        let port_mappings = v
            .get("portMappings")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, p)| {
                        let public = p.as_u64().or_else(|| p.as_str()?.parse().ok())?;
                        Some((k.parse().ok()?, u16::try_from(public).ok()?))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let cost_per_hr = v.get("costPerHr").and_then(|c| {
            c.as_f64()
                .or_else(|| c.as_str().and_then(|s| s.parse().ok()))
        });
        Ok(Self {
            id: id.to_owned(),
            name: s("name").to_owned(),
            desired_status: s("desiredStatus").to_owned(),
            public_ip: Some(s("publicIp").trim().to_owned()).filter(|ip| !ip.is_empty()),
            port_mappings,
            cost_per_hr,
        })
    }

    /// `ip:public_port` for an internal port, once both are known.
    pub fn endpoint(&self, port: u16) -> Option<String> {
        let ip = self.public_ip.as_deref()?;
        let public = self.port_mappings.get(&port)?;
        Some(if ip.contains(':') {
            format!("[{ip}]:{public}")
        } else {
            format!("{ip}:{public}")
        })
    }
}

/// What to create when no pod exists yet.
#[derive(Debug, Clone, PartialEq)]
pub struct PodSpec {
    /// The host's short name: the pod is `atd-<name>`, the host `cloud:<name>`.
    pub name: String,
    /// The AI Host container image (built from `ai-host/Dockerfile`).
    pub image: String,
    pub gpu_type: String,
    pub gpu_count: u32,
    /// `SECURE` or `COMMUNITY`.
    pub cloud_type: String,
    pub container_disk_gb: u32,
}

impl PodSpec {
    pub fn new(name: &str, image: &str) -> Self {
        Self {
            name: name.to_owned(),
            image: image.to_owned(),
            gpu_type: "NVIDIA GeForce RTX 4090".into(),
            gpu_count: 1,
            cloud_type: "SECURE".into(),
            container_disk_gb: 50,
        }
    }

    pub fn pod_name(&self) -> String {
        format!("atd-{}", self.name)
    }

    pub fn host_name(&self) -> String {
        format!("cloud:{}", self.name)
    }
}

/// Runpod's REST API, through the egress gate.
#[derive(Debug, Clone)]
pub struct RunpodClient {
    base: String,
    key: String,
    agent: Agent,
    refused: Arc<Mutex<Option<String>>>,
}

impl RunpodClient {
    /// `base` is [`RUNPOD_API`] (a local mock in tests).
    pub fn new(base: &str, key: &str) -> Self {
        let refused = Arc::new(Mutex::new(None));
        let agent = gated_agent(
            &DownloadOptions {
                allow_internet_once: false,
                base_url: base.to_owned(),
            },
            refused.clone(),
        );
        Self {
            base: base.trim_end_matches('/').to_owned(),
            key: key.to_owned(),
            agent,
            refused,
        }
    }

    /// The key from `RUNPOD_API_KEY`, else `runpod.api_key` in `secrets`.
    pub fn key(secrets: &std::path::Path) -> Result<String, RunpodError> {
        std::env::var("RUNPOD_API_KEY")
            .ok()
            .map(|k| k.trim().to_owned())
            .filter(|k| !k.is_empty())
            .or_else(|| super::read_secret(secrets, &["runpod", "api_key"]))
            .ok_or_else(|| RunpodError::NoKey(secrets.to_owned()))
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, RunpodError> {
        let url = format!("{}{path}", self.base);
        let auth = format!("Bearer {}", self.key);
        let result = match (method, body) {
            ("GET", _) => self.agent.get(&url).header("Authorization", &auth).call(),
            ("DELETE", _) => self
                .agent
                .delete(&url)
                .header("Authorization", &auth)
                .call(),
            (_, Some(b)) => self
                .agent
                .post(&url)
                .header("Authorization", &auth)
                .header("Content-Type", "application/json")
                .send(b.to_string()),
            (_, None) => self
                .agent
                .post(&url)
                .header("Authorization", &auth)
                .send_empty(),
        };
        let resp = match result {
            Ok(r) => r,
            Err(e) => {
                if let Some(host) = self.refused.lock().ok().and_then(|mut r| r.take()) {
                    return Err(RunpodError::Sovereign { host });
                }
                return Err(RunpodError::Transport(e.to_string()));
            }
        };
        let status = resp.status().as_u16();
        let text = resp
            .into_body()
            .read_to_string()
            .map_err(|e| RunpodError::Transport(e.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(RunpodError::Http {
                status,
                body: text.chars().take(400).collect(),
            });
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| RunpodError::Parse(e.to_string()))
    }

    /// `POST /pods`.
    pub fn create(&self, spec: &PodSpec, token: &str) -> Result<Pod, RunpodError> {
        let body = json!({
            "name": spec.pod_name(),
            "imageName": spec.image,
            "gpuTypeIds": [spec.gpu_type],
            "gpuCount": spec.gpu_count,
            "cloudType": spec.cloud_type,
            "ports": [format!("{HOST_PORT}/tcp")],
            "env": { "ATD_AI_HOST_TOKEN": token },
            "containerDiskInGb": spec.container_disk_gb,
        });
        Pod::from_json(&self.request("POST", "/pods", Some(&body))?)
    }

    /// `GET /pods/{id}`.
    pub fn get(&self, id: &str) -> Result<Pod, RunpodError> {
        Pod::from_json(&self.request("GET", &format!("/pods/{id}"), None)?)
    }

    /// `GET /pods?name=…`, filtered by exact name. The list shape (a bare
    /// array) is assumed from the API reference.
    pub fn find(&self, name: &str) -> Result<Option<Pod>, RunpodError> {
        let v = self.request("GET", &format!("/pods?name={name}"), None)?;
        let list = v
            .as_array()
            .or_else(|| v.get("pods").and_then(Value::as_array))
            .ok_or_else(|| RunpodError::Parse(format!("pod list: {v}")))?;
        for p in list {
            let pod = Pod::from_json(p)?;
            if pod.name == name && pod.desired_status != "TERMINATED" {
                return Ok(Some(pod));
            }
        }
        Ok(None)
    }

    /// `POST /pods/{id}/start`.
    pub fn start(&self, id: &str) -> Result<(), RunpodError> {
        self.request("POST", &format!("/pods/{id}/start"), None)
            .map(drop)
    }

    /// `POST /pods/{id}/stop`: billing for the GPU stops; the disk is kept.
    pub fn stop(&self, id: &str) -> Result<(), RunpodError> {
        self.request("POST", &format!("/pods/{id}/stop"), None)
            .map(drop)
    }

    /// `DELETE /pods/{id}` (unconfirmed against the live API): deletes the
    /// pod and its disk.
    pub fn terminate(&self, id: &str) -> Result<(), RunpodError> {
        self.request("DELETE", &format!("/pods/{id}"), None)
            .map(drop)
    }
}

/// Where bringing a pod up has got to.
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Looking,
    Creating,
    Resuming { pod_id: String },
    Waiting { pod_id: String, status: String },
    Ready(Ready),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ready {
    pub pod_id: String,
    /// `ip:port` the supervisor connects to.
    pub addr: String,
    pub usd_per_hour: f64,
}

/// Pods brought up by this process: (client, pod id, host name).
fn running() -> &'static Mutex<Vec<(RunpodClient, String, String)>> {
    static RUNNING: Mutex<Vec<(RunpodClient, String, String)>> = Mutex::new(Vec::new());
    &RUNNING
}

/// Stops every pod this process brought up and removes their hosts (on
/// app exit). Returns the pods it could not stop, with why.
pub fn stop_all(supervisor: &Supervisor) -> Vec<(String, RunpodError)> {
    let pods = std::mem::take(&mut *running().lock().expect("pods"));
    let mut failed = Vec::new();
    for (client, pod_id, host) in pods {
        supervisor.remove(&host);
        if let Err(e) = client.stop(&pod_id) {
            log::error!("could not stop Runpod pod {pod_id}: {e}; stop it in the Runpod console");
            failed.push((pod_id, e));
        }
    }
    failed
}

/// One `cloud:<name>` host on a Runpod pod.
#[derive(Debug)]
pub struct RunpodHost {
    pub client: RunpodClient,
    pub spec: PodSpec,
    /// Where the pod's host token is kept (the per-user secrets file).
    pub secrets: PathBuf,
    pub poll: Duration,
    pub timeout: Duration,
}

impl RunpodHost {
    pub fn new(client: RunpodClient, spec: PodSpec, secrets: PathBuf) -> Self {
        Self {
            client,
            spec,
            secrets,
            poll: Duration::from_secs(5),
            timeout: Duration::from_secs(20 * 60),
        }
    }

    fn token_keys(&self) -> [&str; 3] {
        ["runpod", "tokens", &self.spec.name]
    }

    /// The pod, if one exists.
    pub fn status(&self) -> Result<Option<Pod>, RunpodError> {
        self.client.find(&self.spec.pod_name())
    }

    /// Creates or resumes the pod, waits until its host port is mapped,
    /// registers `cloud:<name>` with `supervisor` and sets its spend rate.
    /// Blocks for minutes: call it from a worker thread ([`spawn_up`]).
    pub fn up(
        &self,
        supervisor: &Supervisor,
        progress: &dyn Fn(Phase),
    ) -> Result<Ready, RunpodError> {
        progress(Phase::Looking);
        let keys = self.token_keys();
        let known_token = super::read_secret(&self.secrets, &keys);
        let (pod_id, token) = match self.status()? {
            Some(pod) => {
                let token = known_token.ok_or_else(|| RunpodError::NoToken {
                    pod_id: pod.id.clone(),
                })?;
                if pod.desired_status != "RUNNING" {
                    progress(Phase::Resuming {
                        pod_id: pod.id.clone(),
                    });
                    self.client.start(&pod.id)?;
                }
                (pod.id, token)
            }
            None => {
                if self.spec.image.trim().is_empty() {
                    return Err(RunpodError::NoImage);
                }
                progress(Phase::Creating);
                let token = random_token();
                super::write_secret(&self.secrets, &keys, &token)
                    .map_err(|e| RunpodError::Secrets(e.to_string()))?;
                (self.client.create(&self.spec, &token)?.id, token)
            }
        };
        // Recorded before waiting, so an exit while it starts still stops it.
        let host = self.spec.host_name();
        {
            let mut r = running().lock().expect("pods");
            if !r.iter().any(|(_, id, _)| *id == pod_id) {
                r.push((self.client.clone(), pod_id.clone(), host.clone()));
            }
        }
        let started = Instant::now();
        let pod = loop {
            let pod = self.client.get(&pod_id)?;
            if let Some(addr) = pod.endpoint(HOST_PORT) {
                break (pod, addr);
            }
            if started.elapsed() > self.timeout {
                return Err(RunpodError::Timeout {
                    pod_id,
                    waited: self.timeout,
                    status: pod.desired_status,
                });
            }
            progress(Phase::Waiting {
                pod_id: pod_id.clone(),
                status: pod.desired_status.clone(),
            });
            std::thread::sleep(self.poll);
        };
        let (pod, addr) = pod;
        let usd_per_hour = pod.cost_per_hr.unwrap_or(0.0);
        crate::router::spend().set_rate(&host, usd_per_hour);
        supervisor.set(HostSpec {
            name: host,
            kind: HostKind::Remote {
                addr: addr.clone(),
                token: Some(token),
            },
        });
        let ready = Ready {
            pod_id,
            addr,
            usd_per_hour,
        };
        log::warn!(
            "Runpod pod {} is running at ${:.2}/h until it is stopped",
            ready.pod_id,
            ready.usd_per_hour
        );
        progress(Phase::Ready(ready.clone()));
        Ok(ready)
    }

    /// Removes the host and stops the pod (GPU billing stops; the disk is
    /// kept for a fast resume). Returns the stopped pod's id.
    pub fn down(&self, supervisor: &Supervisor) -> Result<Option<String>, RunpodError> {
        supervisor.remove(&self.spec.host_name());
        let Some(pod) = self.status()? else {
            return Ok(None);
        };
        self.client.stop(&pod.id)?;
        running()
            .lock()
            .expect("pods")
            .retain(|(_, id, _)| *id != pod.id);
        Ok(Some(pod.id))
    }

    /// Stops and deletes the pod and forgets its token.
    pub fn terminate(&self, supervisor: &Supervisor) -> Result<Option<String>, RunpodError> {
        supervisor.remove(&self.spec.host_name());
        let Some(pod) = self.status()? else {
            return Ok(None);
        };
        self.client.terminate(&pod.id)?;
        running()
            .lock()
            .expect("pods")
            .retain(|(_, id, _)| *id != pod.id);
        Ok(Some(pod.id))
    }
}

/// [`RunpodHost::up`] on a worker thread; `phase` shows its progress.
#[derive(Debug)]
pub struct UpTask {
    pub phase: Arc<Mutex<Phase>>,
    pub handle: JoinHandle<Result<Ready, RunpodError>>,
}

pub fn spawn_up(host: Arc<RunpodHost>, supervisor: &'static Supervisor) -> UpTask {
    let phase = Arc::new(Mutex::new(Phase::Looking));
    let p = phase.clone();
    let handle = std::thread::Builder::new()
        .name("atd-runpod-up".into())
        .spawn(move || {
            let set = |ph: Phase| *p.lock().expect("phase") = ph;
            let r = host.up(supervisor, &set);
            if let Err(e) = &r {
                set(Phase::Failed(e.to_string()));
            }
            r
        })
        .expect("spawn runpod thread");
    UpTask { phase, handle }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    /// A Runpod stand-in on 127.0.0.1: answers from `respond(method, path,
    /// body)` and records every request.
    struct Mock {
        base: String,
        log: Arc<Mutex<Vec<(String, String, String)>>>,
    }

    fn mock(respond: impl Fn(&str, &str, &str) -> (u16, String) + Send + Sync + 'static) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let log = Arc::new(Mutex::new(Vec::new()));
        let l = log.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut r = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if r.read_line(&mut line).is_err() {
                    continue;
                }
                let mut parts = line.split_whitespace();
                let method = parts.next().unwrap_or_default().to_owned();
                let path = parts
                    .next()
                    .unwrap_or_default()
                    .trim_start_matches("/v1")
                    .to_owned();
                let mut len = 0;
                let mut auth = String::new();
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    let h = h.trim_end();
                    if h.is_empty() {
                        break;
                    }
                    let (k, v) = h.split_once(':').unwrap();
                    match k.to_ascii_lowercase().as_str() {
                        "content-length" => len = v.trim().parse().unwrap(),
                        "authorization" => auth = v.trim().to_owned(),
                        _ => {}
                    }
                }
                let mut body = vec![0; len];
                r.read_exact(&mut body).unwrap();
                let body = String::from_utf8(body).unwrap();
                let (status, reply) = if auth == "Bearer test-key" {
                    respond(&method, &path, &body)
                } else {
                    (401, "{}".into())
                };
                l.lock().unwrap().push((method, path, body));
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
            }
        });
        Mock { base, log }
    }

    fn temp_secrets(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "atd-runpod-{tag}-{}/secrets.toml",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn a_new_pod_is_created_waited_for_registered_and_stopped() {
        // GET /pods/p1 answers: initialising, ip only, then ready.
        let polls = Arc::new(Mutex::new(0));
        let p = polls.clone();
        let created = Arc::new(Mutex::new(false));
        let c = created.clone();
        let m = mock(move |method, path, _| match (method, path) {
            ("GET", p0) if p0.starts_with("/pods?") => {
                if *c.lock().unwrap() {
                    (
                        200,
                        r#"[{"id":"p1","name":"atd-studio","desiredStatus":"RUNNING"}]"#.into(),
                    )
                } else {
                    (200, "[]".into())
                }
            }
            ("POST", "/pods") => {
                *c.lock().unwrap() = true;
                (
                    201,
                    r#"{"id":"p1","name":"atd-studio","desiredStatus":"RUNNING","publicIp":"","portMappings":null,"costPerHr":0.69}"#.into(),
                )
            }
            ("GET", "/pods/p1") => {
                let mut n = p.lock().unwrap();
                *n += 1;
                let body = match *n {
                    1 => r#"{"id":"p1","desiredStatus":"RUNNING","publicIp":"","portMappings":{}}"#,
                    2 => {
                        r#"{"id":"p1","desiredStatus":"RUNNING","publicIp":"127.0.0.1","portMappings":{}}"#
                    }
                    _ => {
                        r#"{"id":"p1","desiredStatus":"RUNNING","publicIp":"127.0.0.1","portMappings":{"7420":13007,"22":13008},"costPerHr":0.69}"#
                    }
                };
                (200, body.into())
            }
            ("POST", "/pods/p1/stop") => (200, r#"{"id":"p1","desiredStatus":"EXITED"}"#.into()),
            _ => (404, "{}".into()),
        });
        let secrets = temp_secrets("up");
        let mut host = RunpodHost::new(
            RunpodClient::new(&m.base, "test-key"),
            PodSpec::new("studio", "ghcr.io/example/atd-ai-host:1"),
            secrets.clone(),
        );
        host.poll = Duration::from_millis(5);
        let sup = Supervisor::new();
        let phases = Mutex::new(Vec::new());
        let ready = host
            .up(&sup, &|ph| phases.lock().unwrap().push(ph))
            .unwrap();
        assert_eq!(ready.addr, "127.0.0.1:13007");
        assert_eq!(ready.pod_id, "p1");
        let phases = phases.into_inner().unwrap();
        assert_eq!(phases[..2], [Phase::Looking, Phase::Creating]);
        assert!(matches!(phases[2], Phase::Waiting { .. }));
        assert!(matches!(phases.last(), Some(Phase::Ready(_))));
        // The create body carries the image, GPU, port and the saved token.
        let token = crate::cloud::read_secret(&secrets, &["runpod", "tokens", "studio"]).unwrap();
        let create = m.log.lock().unwrap()[1].2.clone();
        let body: Value = serde_json::from_str(&create).unwrap();
        assert_eq!(body["name"], "atd-studio");
        assert_eq!(body["imageName"], "ghcr.io/example/atd-ai-host:1");
        assert_eq!(body["gpuTypeIds"][0], "NVIDIA GeForce RTX 4090");
        assert_eq!(body["ports"][0], "7420/tcp");
        assert_eq!(body["env"]["ATD_AI_HOST_TOKEN"], token.as_str());
        // The supervisor knows the host at the public mapping.
        let snap = sup.host("cloud:studio").expect("registered");
        assert_eq!(snap.name, "cloud:studio");
        assert!((crate::router::spend().rate("cloud:studio").unwrap() - 0.69).abs() < 1e-9);
        // Down removes the host and stops the pod.
        assert_eq!(host.down(&sup).unwrap().as_deref(), Some("p1"));
        assert!(sup.host("cloud:studio").is_none());
        assert!(
            m.log
                .lock()
                .unwrap()
                .iter()
                .any(|(meth, path, _)| meth == "POST" && path == "/pods/p1/stop")
        );
        let _ = std::fs::remove_file(&secrets);
    }

    #[test]
    fn a_stopped_pod_is_resumed_and_stop_all_stops_it() {
        let m = mock(|method, path, _| {
            match (method, path) {
            ("GET", p) if p.starts_with("/pods?") => (
                200,
                r#"[{"id":"old","name":"atd-other","desiredStatus":"EXITED"},{"id":"p2","name":"atd-gpu","desiredStatus":"EXITED"}]"#.into(),
            ),
            ("POST", "/pods/p2/start") | ("POST", "/pods/p2/stop") => (200, "".into()),
            ("GET", "/pods/p2") => (
                200,
                r#"{"id":"p2","desiredStatus":"RUNNING","publicIp":"127.0.0.1","portMappings":{"7420":"14001"},"costPerHr":"0.34"}"#.into(),
            ),
            _ => (404, "{}".into()),
        }
        });
        let secrets = temp_secrets("resume");
        crate::cloud::write_secret(&secrets, &["runpod", "tokens", "gpu"], "known").unwrap();
        let host = RunpodHost::new(
            RunpodClient::new(&m.base, "test-key"),
            PodSpec::new("gpu", "img"),
            secrets.clone(),
        );
        let sup = Supervisor::new();
        let ready = host.up(&sup, &|_| {}).unwrap();
        assert_eq!(ready.addr, "127.0.0.1:14001");
        assert!((ready.usd_per_hour - 0.34).abs() < 1e-9);
        assert!(stop_all(&sup).is_empty());
        assert!(sup.host("cloud:gpu").is_none());
        let log = m.log.lock().unwrap();
        let calls: Vec<String> = log.iter().map(|(m, p, _)| format!("{m} {p}")).collect();
        assert!(
            calls.contains(&"POST /pods/p2/start".to_owned()),
            "{calls:?}"
        );
        assert!(
            calls.contains(&"POST /pods/p2/stop".to_owned()),
            "{calls:?}"
        );
        assert!(!calls.iter().any(|c| c == "POST /pods"), "{calls:?}");
        let _ = std::fs::remove_file(&secrets);
    }

    #[test]
    fn errors_are_typed() {
        let m = mock(|_, _, _| (500, "boom".into()));
        let c = RunpodClient::new(&m.base, "test-key");
        assert!(matches!(
            c.get("x"),
            Err(RunpodError::Http { status: 500, .. })
        ));
        let c = RunpodClient::new(&m.base, "wrong");
        assert!(matches!(
            c.get("x"),
            Err(RunpodError::Http { status: 401, .. })
        ));
    }

    #[test]
    fn sovereign_mode_refuses_runpod_before_any_packet() {
        let _flags = crate::egress::TEST_FLAGS
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::egress::set_sovereign(true);
        let before = crate::egress::refused();
        let c = RunpodClient::new("http://203.0.113.9/v1", "test-key");
        let err = c.get("p1").unwrap_err();
        assert!(matches!(err, RunpodError::Sovereign { .. }), "{err}");
        assert!(crate::egress::refused() > before);
    }
}
