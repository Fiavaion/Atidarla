//! Windows: Share Out and Share In over Spout ([`atd_interop::spout`]).
//!
//! # Threads (CLAUDE.md rule 1)
//! Each node starts a worker thread on its first cook. The worker owns the
//! node's [`SpoutSender`] or [`SpoutReceiver`] and makes every call that
//! can wait: creating them (a D3D11 device, up to 67 ms per shared-memory
//! mutex), renaming, dropping, polling the sender list and info maps, and
//! the D3D11 copies (which may wait up to 67 ms for another process's
//! access mutex). The cook never calls Spout: it records GPU passes into
//! the frame's encoder and hands over with `try_send`/`try_recv` on two
//! bounded channels, so it never blocks.
//!
//! # Hand-off
//! Cook and worker take turns with one shared texture per node; whoever
//! holds it may touch it, and it changes hands only where the GPU order is
//! already right:
//! - **Share Out**: the cook draws its input into the staging texture
//!   (8-bit BGRA), and once the frame is submitted ([`GpuOp::after_submit`])
//!   sends it to the worker, which copies it into the Spout texture (the
//!   copy runs on the GPU after the frame, see `SpoutSender::send_wgpu`)
//!   and sends it back. If the cook wants to publish while the worker
//!   still has the texture, it skips that frame and publishes the newest
//!   one when the texture returns (latest frame wins).
//! - **Share In**: after each submitted frame the cook gives the landing
//!   texture to the worker, which copies the sender's newest frame into it
//!   (ordered before any later frame on the GPU) and gives it back. The
//!   cook then copies it into a texture of its own, so frames keep showing
//!   while the worker has the landing texture.
//!
//! # Allocation (rule 2)
//! Textures are created on first use and when the size changes; channel
//! messages are moved, not boxed; strings are built only when the state
//! (published name, sender, error) changes. Steady-state frames allocate
//! nothing here beyond what wgpu does while recording passes.

use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::time::{Duration, Instant};

use atd_gpu::engine::{CookCtx, GpuOp, Params};
use atd_gpu::fullscreen::FullscreenPass;
use atd_interop::InteropError;
use atd_interop::dx12::SharedTexture;
use atd_interop::spout::{Received, SpoutReceiver, SpoutSender};

/// What Share Out publishes: the Spout SDK's default format.
const SHARE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

/// How long a worker waits before trying again to create a sender or
/// receiver that failed, so a bad name does not cost a D3D11 device a frame.
const RETRY: Duration = Duration::from_secs(1);

/// A full-screen copy; sizes match except for the frame a sender resizes.
const COPY: &str = "return textureSample(in0, samp, uv);";

fn describe(e: &InteropError) -> String {
    match e {
        InteropError::Platform(msg) => msg.clone(),
        other => other.to_string(),
    }
}

/// Why sharing cannot run on `device`, if it cannot: Spout needs D3D12.
fn backend_error(device: &wgpu::Device) -> Option<String> {
    let backend = device.adapter_info().backend;
    (backend != wgpu::Backend::Dx12).then(|| {
        format!(
            "Spout sharing needs the DX12 graphics backend, and this device runs on {backend:?}: start Atidarla with the environment variable WGPU_BACKEND=dx12"
        )
    })
}

/// A node that cannot run on this device; reports why every cook.
#[derive(Debug)]
struct Failed(String);

impl GpuOp for Failed {
    fn cook(&mut self, _ctx: &mut CookCtx<'_>) -> Result<(), String> {
        Err(self.0.clone())
    }
}

/// Waits until the GPU has run everything queued so far, including the
/// D3D11 copies (the queue waits on their fence), so that a worker's Spout
/// objects (its D3D11 device, the shared fence the queue waits on, shared
/// textures) are released only once the GPU is done with them. Blocks,
/// which is why only workers call it, as they exit.
fn settle(device: &wgpu::Device, queue: &wgpu::Queue) {
    queue.submit([]);
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
}

/// The cook's ends of a worker's channels.
type Link<M, R> = (SyncSender<M>, Receiver<R>);

/// Starts a worker thread running `f` with its ends of two bounded
/// channels. One message is in flight at a time plus parameter changes,
/// so the capacities are never reached in practice; the worker exits when
/// the node (holding the other ends) is dropped.
fn spawn<M: Send + 'static, R: Send + 'static>(
    name: &str,
    f: impl FnOnce(Receiver<M>, SyncSender<R>) + Send + 'static,
) -> Result<Link<M, R>, String> {
    let (tx, worker_rx) = sync_channel(4);
    let (worker_tx, rx) = sync_channel(2);
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || f(worker_rx, worker_tx))
        .map_err(|e| format!("cannot start the {name} thread: {e}"))?;
    Ok((tx, rx))
}

fn stopped() -> String {
    "the texture-sharing thread stopped".to_owned()
}

/// A texture the cook keeps a copy of the received image in.
fn held_texture(
    device: &wgpu::Device,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("atd-share-in"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

// ---------------------------------------------------------------- Share Out

#[derive(Debug)]
enum OutMsg {
    /// Publish under this name; `None` (disabled) removes the sender.
    Config(Option<String>),
    /// Copy this into the Spout texture, then send it back.
    Frame(SharedTexture),
}

/// A change in what the worker publishes, reported with the next reply.
#[derive(Debug)]
enum OutState {
    Unchanged,
    Published(String),
    Failed(String),
}

#[derive(Debug)]
struct OutReply {
    staging: SharedTexture,
    state: OutState,
}

pub(super) fn create_out(device: &wgpu::Device, format: wgpu::TextureFormat) -> Box<dyn GpuOp> {
    if let Some(e) = backend_error(device) {
        return Box::new(Failed(e));
    }
    Box::new(ShareOut {
        pass: FullscreenPass::new(device, "img.share_out", format, 1, "", COPY),
        pass8: FullscreenPass::new(device, "img.share_out.bgra8", SHARE_FORMAT, 1, "", COPY),
        link: None,
        staging: None,
        away: false,
        handoff: false,
        pending: false,
        sent: None,
        last: None,
        status: String::new(),
        error: None,
    })
}

#[derive(Debug)]
struct ShareOut {
    /// Input to output (pass-through), in the engine's format.
    pass: FullscreenPass,
    /// Input to the staging texture, in [`SHARE_FORMAT`].
    pass8: FullscreenPass,
    link: Option<Link<OutMsg, OutReply>>,
    /// The texture handed to the worker; `None` while it has it (`away`)
    /// or before the first publish.
    staging: Option<SharedTexture>,
    away: bool,
    /// Staging was drawn this frame: hand it over after the submit.
    handoff: bool,
    /// A frame to publish is waiting for the staging texture.
    pending: bool,
    /// The configuration the worker has: `Some(None)` is disabled.
    sent: Option<Option<String>>,
    /// The input texture and size last published: the engine gives a
    /// node's result a new texture every time it cooks, so the same
    /// texture means the same picture.
    last: Option<(Option<wgpu::Texture>, u32, u32)>,
    status: String,
    error: Option<String>,
}

impl ShareOut {
    fn take_replies(&mut self, rx: &Receiver<OutReply>) -> Result<(), String> {
        loop {
            match rx.try_recv() {
                Ok(reply) => {
                    self.away = false;
                    self.staging = Some(reply.staging);
                    // A reply for a sender since disabled changes nothing.
                    let enabled = matches!(self.sent, Some(Some(_)));
                    match reply.state {
                        OutState::Unchanged => {}
                        OutState::Published(name) if enabled => {
                            self.error = None;
                            self.status = format!("Published as \"{name}\"");
                        }
                        OutState::Failed(e) if enabled => {
                            self.status.clear();
                            self.error = Some(e);
                        }
                        OutState::Published(_) | OutState::Failed(_) => {}
                    }
                }
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => return Err(stopped()),
            }
        }
    }

    /// Tells the worker about a new name or the enabled switch.
    fn configure(&mut self, tx: &SyncSender<OutMsg>, p: &Params<'_>) -> Result<bool, String> {
        let (enabled, name) = (p.bool("enabled"), p.str("name"));
        let current = match &self.sent {
            Some(Some(n)) => enabled && n == name,
            Some(None) => !enabled,
            None => false,
        };
        if current {
            return Ok(true);
        }
        let want = enabled.then(|| name.to_owned());
        match tx.try_send(OutMsg::Config(want.clone())) {
            Ok(()) => {
                self.sent = Some(want);
                self.last = None;
                self.error = None;
                self.status.clear();
                if !enabled {
                    self.status.push_str("Disabled: not published");
                }
                Ok(true)
            }
            Err(TrySendError::Full(_)) => Ok(false),
            Err(TrySendError::Disconnected(_)) => Err(stopped()),
        }
    }
}

impl GpuOp for ShareOut {
    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        if self.link.is_none() {
            let (device, queue) = (ctx.device.clone(), ctx.queue.clone());
            self.link = Some(spawn("atd-share-out", move |rx, tx| {
                OutWorker {
                    device,
                    queue,
                    name: None,
                    sender: None,
                    reported: None,
                    retry_at: None,
                }
                .run(&rx, &tx);
            })?);
        }
        let Some((tx, rx)) = self.link.take() else {
            unreachable!("started above");
        };
        let configured = self
            .take_replies(&rx)
            .and_then(|()| self.configure(&tx, &ctx.params));
        self.link = Some((tx, rx));
        let configured = configured?;
        if !configured {
            ctx.cook_again = true;
        }

        let inputs: &[Option<wgpu::Texture>] = ctx.inputs;
        let input = inputs.first().and_then(Option::as_ref).unwrap_or(ctx.black);
        self.pass.draw(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            &[input],
            &[],
            ctx.output,
        );

        if configured && matches!(self.sent, Some(Some(_))) {
            let (w, h) = (ctx.output.width(), ctx.output.height());
            let key = (inputs.first().cloned().flatten(), w, h);
            if self.pending || self.last.as_ref() != Some(&key) {
                if self.away {
                    self.pending = true;
                } else {
                    let fits = self
                        .staging
                        .as_ref()
                        .is_some_and(|s| (s.texture.width(), s.texture.height()) == (w, h));
                    if !fits {
                        self.staging = None;
                        self.staging = Some(
                            SharedTexture::create(ctx.device, w, h, SHARE_FORMAT, None)
                                .map_err(|e| describe(&e))?,
                        );
                    }
                    let staging = self.staging.as_ref().expect("created above");
                    self.pass8.draw(
                        ctx.device,
                        ctx.queue,
                        ctx.encoder,
                        &[input],
                        &[],
                        &staging.texture,
                    );
                    self.handoff = true;
                    self.pending = false;
                    self.last = Some(key);
                }
            }
        }
        // Cook again until the worker answers: to publish a frame that
        // waited, and to show its result (the published name or an error).
        if self.handoff || self.away || self.pending {
            ctx.cook_again = true;
        }
        match &self.error {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    fn after_submit(&mut self) {
        if !std::mem::take(&mut self.handoff) {
            return;
        }
        let (Some((tx, _)), Some(staging)) = (&self.link, self.staging.take()) else {
            return;
        };
        match tx.try_send(OutMsg::Frame(staging)) {
            Ok(()) => self.away = true,
            Err(TrySendError::Full(msg) | TrySendError::Disconnected(msg)) => {
                if let OutMsg::Frame(staging) = msg {
                    self.staging = Some(staging);
                }
                self.pending = true;
            }
        }
    }

    fn status(&self) -> Option<&str> {
        (!self.status.is_empty()).then_some(self.status.as_str())
    }
}

/// What one frame did on the worker.
#[derive(Debug)]
enum Outcome {
    /// Disabled, or waiting before retrying a failed sender.
    Idle,
    Sent,
    Failed(String),
}

/// Share Out's worker: owns the Spout sender.
struct OutWorker {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// `None`: disabled.
    name: Option<String>,
    sender: Option<SpoutSender>,
    /// The last state sent to the cook: the published name or an error.
    reported: Option<Result<String, String>>,
    retry_at: Option<Instant>,
}

impl OutWorker {
    fn run(mut self, rx: &Receiver<OutMsg>, tx: &SyncSender<OutReply>) {
        while let Ok(msg) = rx.recv() {
            match msg {
                OutMsg::Config(name) => {
                    if name != self.name {
                        // Dropping the sender removes its name first.
                        self.sender = None;
                        self.name = name;
                        self.reported = None;
                        self.retry_at = None;
                    }
                }
                OutMsg::Frame(staging) => {
                    let state = match self.publish(&staging) {
                        Outcome::Idle => OutState::Unchanged,
                        Outcome::Sent => {
                            let name = self.sender.as_ref().map_or("", |s| s.name());
                            if matches!(&self.reported, Some(Ok(n)) if n == name) {
                                OutState::Unchanged
                            } else {
                                self.reported = Some(Ok(name.to_owned()));
                                OutState::Published(name.to_owned())
                            }
                        }
                        Outcome::Failed(e) => {
                            if matches!(&self.reported, Some(Err(r)) if *r == e) {
                                OutState::Unchanged
                            } else {
                                self.reported = Some(Err(e.clone()));
                                OutState::Failed(e)
                            }
                        }
                    };
                    if tx.send(OutReply { staging, state }).is_err() {
                        break;
                    }
                }
            }
        }
        // The node is gone: dropping `self` removes the sender.
        settle(&self.device, &self.queue);
    }

    fn publish(&mut self, staging: &SharedTexture) -> Outcome {
        let Some(name) = &self.name else {
            return Outcome::Idle;
        };
        if self.sender.is_none() {
            if self.retry_at.is_some_and(|t| Instant::now() < t) {
                return Outcome::Idle;
            }
            let size = staging.texture.size();
            let format = staging.texture.format();
            match SpoutSender::new(&self.device, name, size.width, size.height, format) {
                Ok(sender) => {
                    self.sender = Some(sender);
                    self.retry_at = None;
                }
                Err(e) => {
                    self.retry_at = Some(Instant::now() + RETRY);
                    return Outcome::Failed(describe(&e));
                }
            }
        }
        let sender = self.sender.as_mut().expect("created above");
        // `Ok(false)`: a receiver held the texture for 67 ms; the next
        // frame goes through.
        match sender.send_wgpu(&self.queue, staging) {
            Ok(_) => Outcome::Sent,
            Err(e) => Outcome::Failed(describe(&e)),
        }
    }
}

// ----------------------------------------------------------------- Share In

#[derive(Debug)]
enum InMsg {
    /// Receive from this sender; `None` follows the active sender.
    Follow(Option<String>),
    /// The landing texture is the worker's: copy the newest frame into it.
    Poll,
}

/// A change in what the worker receives, reported with the next reply.
#[derive(Debug)]
enum InChange {
    None,
    Connected(String),
    Lost,
    Failed(String),
}

#[derive(Debug)]
struct InReply {
    /// A new landing texture (first poll, or the sender changed size).
    landing: Option<wgpu::Texture>,
    /// A new frame is in the landing texture.
    frame: bool,
    change: InChange,
}

pub(super) fn create_in(device: &wgpu::Device, format: wgpu::TextureFormat) -> Box<dyn GpuOp> {
    if let Some(e) = backend_error(device) {
        return Box::new(Failed(e));
    }
    Box::new(ShareIn {
        pass: FullscreenPass::new(device, "img.share_in", format, 1, "", COPY),
        link: None,
        away: false,
        sent: None,
        landing: None,
        held: None,
        fresh: false,
        sender: None,
        status: String::new(),
        error: None,
    })
}

#[derive(Debug)]
struct ShareIn {
    pass: FullscreenPass,
    link: Option<Link<InMsg, InReply>>,
    /// The worker has the landing texture (a poll is in flight).
    away: bool,
    /// The sender name the worker follows (empty: the active sender).
    sent: Option<String>,
    /// The worker's landing texture: the sender's size and format.
    landing: Option<wgpu::Texture>,
    /// The last received frame, in the engine's format.
    held: Option<wgpu::Texture>,
    /// `landing` has a frame `held` does not.
    fresh: bool,
    /// The sender received from, while there is one.
    sender: Option<String>,
    status: String,
    error: Option<String>,
}

impl ShareIn {
    fn take_replies(&mut self, rx: &Receiver<InReply>) -> Result<(), String> {
        let mut changed = false;
        loop {
            match rx.try_recv() {
                Ok(reply) => {
                    self.away = false;
                    if let Some(landing) = reply.landing {
                        self.landing = Some(landing);
                        changed = true;
                    }
                    self.fresh |= reply.frame;
                    match reply.change {
                        InChange::None => {}
                        InChange::Connected(name) => {
                            self.sender = Some(name);
                            self.error = None;
                            changed = true;
                        }
                        InChange::Lost => {
                            self.sender = None;
                            self.error = None;
                            changed = true;
                        }
                        InChange::Failed(e) => {
                            self.sender = None;
                            self.error = Some(e);
                            changed = true;
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Err(stopped()),
            }
        }
        if changed {
            self.update_status();
        }
        Ok(())
    }

    fn follow(&mut self, tx: &SyncSender<InMsg>, name: &str) -> Result<(), String> {
        if self.sent.as_deref() == Some(name) {
            return Ok(());
        }
        let wanted = (!name.is_empty()).then(|| name.to_owned());
        match tx.try_send(InMsg::Follow(wanted)) {
            Ok(()) => {
                self.sent = Some(name.to_owned());
                self.sender = None;
                self.error = None;
                self.update_status();
                Ok(())
            }
            // Share In cooks every frame: try again on the next.
            Err(TrySendError::Full(_)) => Ok(()),
            Err(TrySendError::Disconnected(_)) => Err(stopped()),
        }
    }

    fn update_status(&mut self) {
        self.status.clear();
        match (&self.sender, &self.landing) {
            (Some(name), Some(t)) => {
                self.status = format!("Receiving \"{name}\" ({}×{})", t.width(), t.height());
            }
            _ => match self.sent.as_deref() {
                Some("") => self.status.push_str("No active sender"),
                Some(name) => self.status = format!("Waiting for \"{name}\""),
                None => {}
            },
        }
    }
}

impl GpuOp for ShareIn {
    fn output_size(
        &self,
        _params: &Params<'_>,
        _input: Option<(u32, u32)>,
        _project: (u32, u32),
    ) -> Option<(u32, u32)> {
        self.sender.as_ref()?;
        self.landing.as_ref().map(|t| (t.width(), t.height()))
    }

    fn cook(&mut self, ctx: &mut CookCtx<'_>) -> Result<(), String> {
        if self.link.is_none() {
            let (device, queue) = (ctx.device.clone(), ctx.queue.clone());
            self.link = Some(spawn("atd-share-in", move |rx, tx| {
                InWorker {
                    device,
                    queue,
                    wanted: None,
                    receiver: None,
                    landing: None,
                    connected: false,
                    report: true,
                    failed: None,
                    retry_at: None,
                }
                .run(&rx, &tx);
            })?);
        }
        let Some((tx, rx)) = self.link.take() else {
            unreachable!("started above");
        };
        let result = self
            .take_replies(&rx)
            .and_then(|()| self.follow(&tx, ctx.params.str("name")));
        self.link = Some((tx, rx));
        result?;

        // Keep the new frame while the landing texture is ours.
        if !self.away
            && self.fresh
            && let Some(landing) = &self.landing
        {
            let (w, h) = (landing.width(), landing.height());
            let format = ctx.output.format();
            let fits = self
                .held
                .as_ref()
                .is_some_and(|t| (t.width(), t.height(), t.format()) == (w, h, format));
            if !fits {
                self.held = Some(held_texture(ctx.device, w, h, format));
            }
            let held = self.held.as_ref().expect("created above");
            self.pass
                .draw(ctx.device, ctx.queue, ctx.encoder, &[landing], &[], held);
            self.fresh = false;
        }
        let source = match (&self.sender, &self.held) {
            (Some(_), Some(held)) => held,
            _ => ctx.black,
        };
        self.pass.draw(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            &[source],
            &[],
            ctx.output,
        );
        match &self.error {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    fn after_submit(&mut self) {
        // This frame's reads of the landing texture are submitted: the
        // worker may write it now.
        if !self.away
            && let Some((tx, _)) = &self.link
            && tx.try_send(InMsg::Poll).is_ok()
        {
            self.away = true;
        }
    }

    fn status(&self) -> Option<&str> {
        (!self.status.is_empty()).then_some(self.status.as_str())
    }
}

/// Share In's worker: owns the Spout receiver and the landing texture.
struct InWorker {
    device: wgpu::Device,
    queue: wgpu::Queue,
    wanted: Option<String>,
    receiver: Option<SpoutReceiver>,
    landing: Option<SharedTexture>,
    connected: bool,
    /// Report the connection state on the next poll even if unchanged.
    report: bool,
    failed: Option<String>,
    retry_at: Option<Instant>,
}

impl InWorker {
    fn run(mut self, rx: &Receiver<InMsg>, tx: &SyncSender<InReply>) {
        while let Ok(msg) = rx.recv() {
            match msg {
                InMsg::Follow(name) => {
                    self.wanted = name;
                    self.receiver = None;
                    self.connected = false;
                    self.report = true;
                    self.failed = None;
                    self.retry_at = None;
                }
                InMsg::Poll => {
                    if tx.send(self.poll()).is_err() {
                        break;
                    }
                }
            }
        }
        settle(&self.device, &self.queue);
    }

    fn poll(&mut self) -> InReply {
        let mut reply = InReply {
            landing: None,
            frame: false,
            change: InChange::None,
        };
        match self.receive(&mut reply) {
            Ok(state) => {
                let connected = state != Received::NoSender;
                if connected != self.connected || self.report || self.failed.is_some() {
                    reply.change = if connected {
                        let name = self.receiver.as_ref().and_then(SpoutReceiver::sender_name);
                        InChange::Connected(name.unwrap_or_default())
                    } else {
                        InChange::Lost
                    };
                    self.connected = connected;
                    self.report = false;
                    self.failed = None;
                }
                reply.frame = state == Received::NewFrame;
            }
            Err(Some(e)) => {
                self.connected = false;
                if self.failed.as_ref() != Some(&e) {
                    self.failed = Some(e.clone());
                    reply.change = InChange::Failed(e);
                }
            }
            Err(None) => {}
        }
        reply
    }

    /// One receive into the landing texture, replacing it when the sender's
    /// size or format differs. `Err(None)`: waiting to retry a receiver.
    fn receive(&mut self, reply: &mut InReply) -> Result<Received, Option<String>> {
        if self.receiver.is_none() {
            if self.retry_at.is_some_and(|t| Instant::now() < t) {
                return Err(None);
            }
            match SpoutReceiver::connect(&self.device, self.wanted.as_deref()) {
                Ok(receiver) => self.receiver = Some(receiver),
                Err(e) => {
                    self.retry_at = Some(Instant::now() + RETRY);
                    return Err(Some(describe(&e)));
                }
            }
        }
        let receiver = self.receiver.as_mut().expect("connected above");
        if self.landing.is_none() {
            // A placeholder until the sender's size is known.
            let t =
                SharedTexture::create(&self.device, 1, 1, wgpu::TextureFormat::Rgba8Unorm, None)
                    .map_err(|e| Some(describe(&e)))?;
            reply.landing = Some(t.texture.clone());
            self.landing = Some(t);
        }
        // Twice: a resize is followed by the copy.
        for _ in 0..2 {
            let landing = self.landing.as_ref().expect("created above");
            match receiver
                .receive_into_shared(&self.queue, landing)
                .map_err(|e| Some(describe(&e)))?
            {
                Received::Resize {
                    width,
                    height,
                    format,
                } => {
                    self.landing = None;
                    let t = SharedTexture::create(&self.device, width, height, format, None)
                        .map_err(|e| Some(describe(&e)))?;
                    reply.landing = Some(t.texture.clone());
                    self.landing = Some(t);
                }
                state => return Ok(state),
            }
        }
        // The sender changed size again in between: next poll.
        Ok(Received::NoNewFrame)
    }
}
