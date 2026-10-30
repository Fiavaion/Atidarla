//! MIDI/OSC mappings at run time (FR-S5, PT-8, decision A54).
//!
//! I/O threads (an OSC socket, one thread per MIDI input) hand every
//! message to a [`ControlHub`]. The hub matches it against the project's
//! mappings there and keeps, per mapping, only the latest value plus what
//! toggles and encoders need (presses, summed steps). Once per frame the
//! app calls [`MappingRuntime::update`]: it takes those slots under one
//! short lock, turns them into parameter values (range, curve, mode,
//! pick-up) and leaves them in [`MappingRuntime::writes`] for
//! `Engine::set_live`. Nothing queues behind the cook: a control moved
//! 50 times between two frames gives one write, the latest.
//!
//! The same hub carries learn (arm a parameter; the next control becomes
//! its mapping), a monitor of the last [`MONITOR_LEN`] messages, and the
//! OSCQuery routes (`/node/param` addresses set parameters directly).
//!
//! After warm-up, `update` allocates nothing for number, toggle and
//! integer parameters; vector, colour and menu values are new
//! `serde_json` values (an array or a string) when they change.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use atd_core::graph::{Network, NodeId, assign_value};
use atd_core::registry::{ParamKind, Registry};
use atd_format::mappings::{Curve, Mapping, Mappings, MidiControl, Mode, Source, Target};
use rosc::{OscPacket, OscType};
use serde_json::Value;

use crate::midi::{self, Message};

/// Messages the monitor keeps.
pub const MONITOR_LEN: usize = 64;

/// Most OSCQuery routes (advertised addresses) the hub matches.
pub const MAX_ROUTES: usize = 16_384;

/// Pick-up engages when the control comes within this fraction of the
/// output range of the parameter's value (or passes it).
pub const PICKUP_TOLERANCE: f64 = 0.02;

const POLL: Duration = Duration::from_millis(50);

// ---------------------------------------------------------------- maths

/// `x` as 0–1 within `input` (either direction), clamped.
pub fn normalise(x: f64, input: [f64; 2]) -> f64 {
    let span = input[1] - input[0];
    if span == 0.0 || !span.is_finite() {
        return if x >= input[0] { 1.0 } else { 0.0 };
    }
    ((x - input[0]) / span).clamp(0.0, 1.0)
}

/// The curve applied to `t` in 0–1.
pub fn shape(curve: Curve, t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    match curve {
        Curve::Linear => t,
        Curve::Exp(a) if a.abs() < 1e-9 => t,
        Curve::Exp(a) => ((a * t).exp() - 1.0) / (a.exp() - 1.0),
        Curve::Step(n) => {
            let n = n.max(2);
            let level = (t * f64::from(n)).floor().min(f64::from(n - 1));
            level / f64::from(n - 1)
        }
    }
}

/// An absolute control value mapped through input range, curve and
/// output range.
pub fn map_value(m: &Mapping, x: f64) -> f64 {
    let t = shape(m.curve, normalise(x, m.input));
    m.output[0] + t * (m.output[1] - m.output[0])
}

/// The step an encoder value means. MIDI CC uses two's complement
/// (1–63 up, 65–127 down, 64 and 0 nothing); OSC sends the signed step.
pub fn relative_step(source: &Source, raw: f64) -> f64 {
    match source {
        Source::Midi {
            control: MidiControl::Cc(_) | MidiControl::Note(_),
            ..
        } => {
            let v = raw.round();
            if v >= 64.0 { v - 128.0 } else { v }
        }
        Source::Midi { .. } | Source::Osc { .. } => raw,
    }
}

/// A sensible output range for a parameter: its limits, 0–1 for
/// unlimited numbers, toggles, colours and vectors, the option indices for
/// menus.
pub fn output_range(kind: &ParamKind) -> [f64; 2] {
    match kind {
        ParamKind::Float { min, max } => [min.unwrap_or(0.0), max.unwrap_or(1.0)],
        ParamKind::Int { min, max } => [
            min.unwrap_or(0) as f64,
            max.unwrap_or(min.unwrap_or(0) + 100) as f64,
        ],
        ParamKind::Menu { options } => [0.0, options.len().saturating_sub(1) as f64],
        _ => [0.0, 1.0],
    }
}

/// Whether a parameter kind can be driven by a control.
pub fn mappable(kind: &ParamKind) -> bool {
    !matches!(kind, ParamKind::String | ParamKind::Path)
}

/// Components of a vector or colour parameter (0 for the rest).
pub fn components(kind: &ParamKind) -> usize {
    match kind {
        ParamKind::Colour => 4,
        ParamKind::Vec { n } => usize::from(*n),
        _ => 0,
    }
}

/// A parameter value (or one component of it) as a number: booleans are
/// 0/1, menus their option index.
pub fn as_number(kind: &ParamKind, value: &Value, component: Option<usize>) -> Option<f64> {
    match (kind, value) {
        (_, Value::Bool(b)) => Some(if *b { 1.0 } else { 0.0 }),
        (_, Value::Number(n)) => n.as_f64(),
        (ParamKind::Menu { options }, Value::String(s)) => {
            options.iter().position(|o| o == s).map(|i| i as f64)
        }
        (_, Value::Array(a)) => a.get(component.unwrap_or(0)).and_then(Value::as_f64),
        _ => None,
    }
}

/// The value to set for `y` on a parameter whose value is `current`:
/// numbers, rounded integers, booleans (≥ 0.5), menu options by index,
/// or `current` with one component (all for `None`) replaced.
pub fn live_value(
    kind: &ParamKind,
    current: &Value,
    component: Option<usize>,
    y: f64,
) -> Option<Value> {
    if !y.is_finite() {
        return None;
    }
    Some(match kind {
        ParamKind::Float { .. } => Value::from(y),
        ParamKind::Int { .. } => Value::from(y.round() as i64),
        ParamKind::Toggle | ParamKind::Pulse => Value::Bool(y >= 0.5),
        ParamKind::Menu { options } => {
            let last = options.len().checked_sub(1)?;
            Value::String(options[(y.round().max(0.0) as usize).min(last)].clone())
        }
        ParamKind::Colour | ParamKind::Vec { .. } => {
            let n = components(kind);
            let mut a = match current {
                Value::Array(a) => a.clone(),
                _ => Vec::new(),
            };
            a.resize(n.max(a.len()), Value::from(0.0));
            match component {
                Some(c) if c < a.len() => a[c] = Value::from(y),
                Some(_) => return None,
                None => a.iter_mut().for_each(|v| *v = Value::from(y)),
            }
            Value::Array(a)
        }
        ParamKind::String | ParamKind::Path => return None,
    })
}

/// A parameter's kind and current value: `live` if set, else its stored
/// value or schema default.
pub fn resolve<'a>(
    net: &'a Network,
    registry: &'a Registry,
    node: NodeId,
    param: &str,
    live: Option<&'a Value>,
) -> Option<(&'a ParamKind, &'a Value)> {
    let n = net.node(node)?;
    let schema = registry.get(&n.type_id)?;
    let p = schema.params.iter().find(|p| p.name == param)?;
    let value = live.or_else(|| net.param_value(registry, node, param))?;
    Some((&p.kind, value))
}

// ---------------------------------------------------------------- hub

/// What a mapping slot received since the last frame.
#[derive(Debug, Default, Clone, Copy)]
struct Slot {
    fresh: bool,
    raw: f64,
    /// Summed encoder steps.
    delta: f64,
    /// Rising edges (press, or crossing the middle of the input range).
    presses: u32,
    /// Held: note on, or above the middle of the input range.
    gate: bool,
}

#[derive(Debug, Default, Clone, Copy)]
struct RouteSlot {
    fresh: bool,
    args: [f64; 4],
    n: u8,
}

/// One message as shown in the monitor.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorEntry {
    pub at: Instant,
    /// `nanoKONTROL2 · CC 7 ch1 = 64`, `OSC /fader/1 0.5 1`.
    pub text: String,
    /// The control, as a learn would map it (`None` for note-offs and
    /// non-numeric OSC).
    pub source: Option<Source>,
}

#[derive(Debug, Default)]
struct HubState {
    mappings: Vec<Mapping>,
    /// Lower-case device patterns, by mapping.
    patterns: Vec<String>,
    slots: Vec<Slot>,
    routes: HashMap<String, usize>,
    route_slots: Vec<RouteSlot>,
    monitor: VecDeque<MonitorEntry>,
    armed: Option<Target>,
    learned: Option<(Target, Source)>,
    received: u64,
}

impl HubState {
    fn log(&mut self, text: String, source: Option<Source>) {
        self.received += 1;
        if self.monitor.len() >= MONITOR_LEN {
            self.monitor.pop_front();
        }
        self.monitor.push_back(MonitorEntry {
            at: Instant::now(),
            text,
            source,
        });
    }

    fn learn(&mut self, source: &Option<Source>) {
        if let (Some(source), Some(_)) = (source, &self.armed) {
            let target = self.armed.take().expect("checked");
            self.learned = Some((target, source.clone()));
        }
    }

    fn feed(&mut self, i: usize, x: f64, gate: Option<bool>) {
        let m = &self.mappings[i];
        let high = gate.unwrap_or_else(|| normalise(x, m.input) >= 0.5);
        let step = relative_step(&m.source, x);
        let s = &mut self.slots[i];
        if high && !s.gate {
            s.presses += 1;
        }
        s.gate = high;
        s.raw = x;
        s.delta += step;
        s.fresh = true;
    }
}

/// Where I/O threads deliver MIDI and OSC; shared with the runtime.
/// Messages can also be pushed directly (tests, virtual controls).
#[derive(Debug, Clone, Default)]
pub struct ControlHub {
    state: Arc<Mutex<HubState>>,
    waker: Arc<Mutex<Option<Waker>>>,
}

/// Called (on the I/O thread) after each message, so an idle UI can
/// schedule a frame (`egui::Context::request_repaint`).
#[derive(Clone)]
pub struct Waker(pub Arc<dyn Fn() + Send + Sync>);

impl std::fmt::Debug for Waker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Waker")
    }
}

/// A channel-voice message as (control, channel, value, note gate).
fn midi_control(msg: &Message) -> Option<(MidiControl, u8, f64, Option<bool>)> {
    Some(match *msg {
        Message::Control {
            channel,
            controller,
            value,
        } => (MidiControl::Cc(controller), channel, f64::from(value), None),
        Message::NoteOn {
            channel,
            note,
            velocity,
        } => (
            MidiControl::Note(note),
            channel,
            f64::from(velocity),
            Some(true),
        ),
        Message::NoteOff { channel, note } => (MidiControl::Note(note), channel, 0.0, Some(false)),
        Message::PitchBend { channel, value } => {
            (MidiControl::Bend, channel, f64::from(value), None)
        }
        _ => return None,
    })
}

impl ControlHub {
    fn lock(&self) -> MutexGuard<'_, HubState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A MIDI message from the input named `device`.
    pub fn push_midi(&self, device: &str, msg: Message) {
        let Some((control, channel, x, gate)) = midi_control(&msg) else {
            return;
        };
        let lower = device.to_lowercase();
        let mut st = self.lock();
        for i in 0..st.mappings.len() {
            let hit = match &st.mappings[i].source {
                Source::Midi {
                    control: c,
                    channel: ch,
                    ..
                } => {
                    *c == control
                        && ch.is_none_or(|c| c == channel)
                        && lower.contains(st.patterns[i].as_str())
                }
                Source::Osc { .. } => false,
            };
            if hit {
                st.feed(i, x, gate);
            }
        }
        let text = format!("{device} · {} = {x}", control_label(control, channel));
        let learnable = (gate != Some(false)).then(|| Source::Midi {
            control,
            channel: Some(channel),
            device: device.to_owned(),
        });
        st.learn(&learnable);
        st.log(text, learnable);
        drop(st);
        self.wake();
    }

    /// An OSC message with its numeric arguments (others are skipped).
    pub fn push_osc(&self, address: &str, args: &[f64]) {
        let mut st = self.lock();
        for i in 0..st.mappings.len() {
            if let Source::Osc { address: a, arg } = &st.mappings[i].source
                && a == address
                && let Some(x) = args.get(*arg).copied()
            {
                st.feed(i, x, None);
            }
        }
        if let Some(&r) = st.routes.get(address) {
            let s = &mut st.route_slots[r];
            s.n = args.len().min(4) as u8;
            s.args[..usize::from(s.n)].copy_from_slice(&args[..usize::from(s.n)]);
            s.fresh = true;
        }
        let mut text = format!("OSC {address}");
        for a in args {
            text.push_str(&format!(" {a}"));
        }
        let learnable = (!args.is_empty()).then(|| Source::Osc {
            address: address.to_owned(),
            arg: 0,
        });
        st.learn(&learnable);
        st.log(text, learnable);
        drop(st);
        self.wake();
    }

    /// Sets what to call after each message.
    pub fn set_waker(&self, waker: Option<Waker>) {
        *self.waker.lock().unwrap_or_else(|e| e.into_inner()) = waker;
    }

    fn wake(&self) {
        let w = self.waker.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(w) = w {
            (w.0)();
        }
    }

    /// Messages received since start.
    pub fn received(&self) -> u64 {
        self.lock().received
    }
}

fn control_label(control: MidiControl, channel: u8) -> String {
    match control {
        MidiControl::Cc(n) => format!("CC {n} ch{channel}"),
        MidiControl::Note(n) => format!("Note {n} ch{channel}"),
        MidiControl::Bend => format!("Pitch bend ch{channel}"),
    }
}

// ---------------------------------------------------------------- I/O

fn osc_numbers(arg: &OscType) -> Option<f64> {
    let v = match arg {
        OscType::Float(f) => f64::from(*f),
        OscType::Double(d) => *d,
        OscType::Int(i) => f64::from(*i),
        OscType::Long(l) => *l as f64,
        OscType::Bool(b) => f64::from(u8::from(*b)),
        _ => return None,
    };
    v.is_finite().then_some(v)
}

fn deliver(packet: OscPacket, hub: &ControlHub, args: &mut Vec<f64>) {
    match packet {
        OscPacket::Message(m) => {
            args.clear();
            args.extend(m.args.iter().filter_map(osc_numbers));
            hub.push_osc(&m.addr, args);
        }
        OscPacket::Bundle(b) => {
            for p in b.content {
                deliver(p, hub, args);
            }
        }
    }
}

/// An OSC socket feeding a hub from its own thread.
#[derive(Debug)]
struct OscListener {
    stop: Arc<AtomicBool>,
    port: u16,
    thread: Option<JoinHandle<()>>,
}

impl OscListener {
    fn bind(addr: IpAddr, port: u16, hub: ControlHub) -> io::Result<Self> {
        let socket = UdpSocket::bind((addr, port))?;
        socket.set_read_timeout(Some(POLL))?;
        let port = socket.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let s = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name(format!("atd-osc-map-{port}"))
            .spawn(move || {
                let mut buf = [0u8; rosc::decoder::MTU];
                let mut args = Vec::new();
                while !s.load(Ordering::Relaxed) {
                    match socket.recv_from(&mut buf) {
                        Ok((n, _)) => match rosc::decoder::decode_udp(&buf[..n]) {
                            Ok((_, packet)) => deliver(packet, &hub, &mut args),
                            Err(e) => log::debug!("bad OSC packet: {e:?}"),
                        },
                        Err(e)
                            if matches!(
                                e.kind(),
                                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                            ) => {}
                        Err(e) => log::warn!("OSC receive failed: {e}"),
                    }
                }
            })?;
        Ok(Self {
            stop,
            port,
            thread: Some(thread),
        })
    }
}

impl Drop for OscListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A MIDI input feeding a hub (the connection lives on its own thread).
#[derive(Debug)]
struct MidiListener {
    name: String,
    error: Arc<OnceLock<String>>,
    _stop: Sender<()>,
}

impl MidiListener {
    fn open(name: &str, hub: ControlHub) -> Self {
        let (stop, stopped) = channel::<()>();
        let error = Arc::new(OnceLock::new());
        let (n, e) = (name.to_owned(), Arc::clone(&error));
        let spawned = std::thread::Builder::new()
            .name("atd-midi-map".into())
            .spawn(move || {
                if let Err(msg) = run_midi(&n, hub, &stopped) {
                    log::warn!("MIDI mapping input {n}: {msg}");
                    let _ = e.set(msg);
                }
            });
        if let Err(e) = spawned {
            let _ = error.set(format!("cannot start MIDI thread: {e}"));
        }
        Self {
            name: name.to_owned(),
            error,
            _stop: stop,
        }
    }
}

fn run_midi(name: &str, hub: ControlHub, stop: &Receiver<()>) -> Result<(), String> {
    let input = midir::MidiInput::new("Atidarla mappings").map_err(|e| e.to_string())?;
    let port = input
        .ports()
        .into_iter()
        .find(|p| input.port_name(p).is_ok_and(|n| n == name))
        .ok_or_else(|| format!("no MIDI input named {name:?}"))?;
    let device = name.to_owned();
    let _connection = input
        .connect(
            &port,
            "atidarla-mappings",
            move |_, bytes, _| {
                if let Some(msg) = midi::parse(bytes) {
                    hub.push_midi(&device, msg);
                }
            },
            (),
        )
        .map_err(|e| e.to_string())?;
    let _ = stop.recv();
    Ok(())
}

// ---------------------------------------------------------------- runtime

/// An address advertised over OSCQuery and the parameter it sets.
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub address: String,
    pub node: NodeId,
    pub param: String,
    pub kind: ParamKind,
}

#[derive(Debug, Default, Clone, Copy)]
struct MapState {
    /// Pick-up has engaged (always true without pick-up).
    engaged: bool,
    /// The last output a pick-up mapping computed (to see it pass).
    prev: Option<f64>,
    /// The value this mapping last wrote (reset when something else moves
    /// the parameter, for pick-up).
    written: Option<f64>,
    /// The output this mapping last wrote, kept until the mapping or its
    /// target changes: what saving stores ([`MappingRuntime::written`]).
    sent: Option<f64>,
    /// Toggle state.
    on: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Origin {
    Mapping(usize),
    Route(usize),
}

/// One live value to set this frame.
#[derive(Debug)]
struct Write {
    node: NodeId,
    from: Origin,
    value: Value,
}

/// Mappings applied once per frame; see the module docs.
#[derive(Debug, Default)]
pub struct MappingRuntime {
    hub: ControlHub,
    osc: Option<OscListener>,
    midi: Vec<MidiListener>,
    synced: Vec<Mapping>,
    state: Vec<MapState>,
    snap: Vec<Slot>,
    routes: Vec<Route>,
    route_snap: Vec<RouteSlot>,
    out: Vec<Write>,
    /// Per mapped (node, parameter): the whole live value mappings last
    /// set on it.
    held: Vec<(NodeId, String, Value)>,
    /// Values of parameters no mapping drives any more, to release this
    /// frame (see [`Self::released`]).
    released: Vec<(NodeId, String, Value)>,
}

impl MappingRuntime {
    /// No sockets or devices yet: see [`Self::listen_osc`],
    /// [`Self::open_midi`], or push messages through [`Self::hub`].
    pub fn new() -> Self {
        Self::default()
    }

    pub fn hub(&self) -> &ControlHub {
        &self.hub
    }

    /// Receives OSC on `port` (0 picks one), on every interface or only
    /// this computer. Replaces an earlier socket; returns the bound port.
    pub fn listen_osc(&mut self, port: u16, loopback: bool) -> io::Result<u16> {
        self.osc = None;
        let addr: IpAddr = if loopback {
            Ipv4Addr::LOCALHOST.into()
        } else {
            Ipv4Addr::UNSPECIFIED.into()
        };
        let l = OscListener::bind(addr, port, self.hub.clone())?;
        let port = l.port;
        self.osc = Some(l);
        Ok(port)
    }

    pub fn stop_osc(&mut self) {
        self.osc = None;
    }

    /// The bound OSC port, if listening.
    pub fn osc_port(&self) -> Option<u16> {
        self.osc.as_ref().map(|l| l.port)
    }

    /// Opens every MIDI input not open yet (the hub tells devices apart
    /// by name, for device patterns). Never blocks.
    pub fn open_midi(&mut self) {
        for name in midi::ports().0 {
            if !self.midi.iter().any(|m| m.name == name) {
                self.midi.push(MidiListener::open(&name, self.hub.clone()));
            }
        }
    }

    /// Closes the MIDI inputs.
    pub fn close_midi(&mut self) {
        self.midi.clear();
    }

    /// Open MIDI inputs and their errors.
    pub fn midi_inputs(&self) -> Vec<(String, Option<String>)> {
        self.midi
            .iter()
            .map(|m| (m.name.clone(), m.error.get().cloned()))
            .collect()
    }

    /// Arms `target`: the next control that arrives becomes its mapping
    /// (collect it with [`Self::take_learned`]).
    pub fn arm(&self, target: Target) {
        let mut st = self.hub.lock();
        st.armed = Some(target);
        st.learned = None;
    }

    pub fn cancel_learn(&self) {
        self.hub.lock().armed = None;
    }

    /// The parameter waiting for a control.
    pub fn armed(&self) -> Option<Target> {
        self.hub.lock().armed.clone()
    }

    /// A learned (target, source), once.
    pub fn take_learned(&self) -> Option<(Target, Source)> {
        self.hub.lock().learned.take()
    }

    /// Learns into `maps` if a control arrived: adds a mapping with an
    /// output range and mode fitting the parameter (`kind`, from the
    /// caller's registry) and returns its ID.
    pub fn learn_into(
        &self,
        maps: &mut Mappings,
        kind: impl FnOnce(&Target) -> Option<ParamKind>,
    ) -> Option<u64> {
        let (target, source) = self.take_learned()?;
        let kind = kind(&target)?;
        let id = maps.add(source, target, output_range(&kind));
        let m = maps.mappings.last_mut().expect("just added");
        match kind {
            ParamKind::Toggle => m.mode = Mode::Toggle,
            ParamKind::Pulse => m.mode = Mode::Momentary,
            ParamKind::Menu { options } if options.len() >= 2 => {
                m.curve = Curve::Step(options.len() as u32);
            }
            _ => {}
        }
        Some(id)
    }

    /// The last messages, oldest first.
    pub fn monitor(&self) -> Vec<MonitorEntry> {
        self.hub.lock().monitor.iter().cloned().collect()
    }

    /// Sets the OSCQuery routes: OSC to these addresses sets the parameter
    /// directly (all arguments, for vectors and colours).
    pub fn set_routes(&mut self, routes: Vec<Route>) {
        let mut st = self.hub.lock();
        st.routes = routes
            .iter()
            .take(MAX_ROUTES)
            .enumerate()
            .map(|(i, r)| (r.address.clone(), i))
            .collect();
        st.route_slots = vec![RouteSlot::default(); st.routes.len()];
        drop(st);
        self.routes = routes;
        self.routes.truncate(MAX_ROUTES);
        self.route_snap.clear();
    }

    pub fn routes(&self) -> &[Route] {
        &self.routes
    }

    fn sync(&mut self, maps: &Mappings) {
        if self.synced == maps.mappings {
            return;
        }
        // Keep the state of mappings that did not change, and what an
        // edited mapping last wrote while its target stays.
        let state = maps
            .mappings
            .iter()
            .map(|m| {
                if let Some(i) = self.synced.iter().position(|s| s == m) {
                    return self.state[i];
                }
                let sent = self
                    .synced
                    .iter()
                    .position(|s| s.id == m.id && s.target == m.target)
                    .and_then(|i| self.state[i].sent);
                MapState {
                    sent,
                    ..MapState::default()
                }
            })
            .collect();
        self.state = state;
        self.synced = maps.mappings.clone();
        // A parameter no mapping drives any more is released, or it would
        // stay frozen at the last value.
        let mut i = 0;
        while i < self.held.len() {
            let (node, param, _) = &self.held[i];
            if maps
                .mappings
                .iter()
                .any(|m| m.target.node == *node && m.target.param == *param)
            {
                i += 1;
            } else {
                self.released.push(self.held.swap_remove(i));
            }
        }
        let mut st = self.hub.lock();
        st.patterns = maps
            .mappings
            .iter()
            .map(|m| match &m.source {
                Source::Midi { device, .. } => device.to_lowercase(),
                Source::Osc { .. } => String::new(),
            })
            .collect();
        // What arrived for a mapping whose control did not change stays.
        let slots = maps
            .mappings
            .iter()
            .map(|m| {
                st.mappings
                    .iter()
                    .position(|o| o.id == m.id && o.source == m.source)
                    .map_or_else(Slot::default, |i| st.slots[i])
            })
            .collect();
        st.slots = slots;
        st.mappings = maps.mappings.clone();
    }

    /// Takes what arrived since the last frame and computes this frame's
    /// live values (see [`Self::writes`]). `lookup` gives a parameter's
    /// kind and current value ([`resolve`] over the engine).
    pub fn update<'e>(
        &mut self,
        maps: &Mappings,
        lookup: impl Fn(NodeId, &str) -> Option<(&'e ParamKind, &'e Value)>,
    ) {
        self.released.clear();
        self.sync(maps);
        self.out.clear();
        {
            let mut st = self.hub.lock();
            self.snap.clear();
            self.snap.extend_from_slice(&st.slots);
            for s in &mut st.slots {
                *s = Slot {
                    gate: s.gate,
                    raw: s.raw,
                    ..Slot::default()
                };
            }
            self.route_snap.clear();
            self.route_snap.extend_from_slice(&st.route_slots);
            for s in &mut st.route_slots {
                s.fresh = false;
            }
        }
        for i in 0..self.snap.len().min(self.synced.len()) {
            let slot = self.snap[i];
            if !slot.fresh {
                continue;
            }
            let m = &self.synced[i];
            let t = &m.target;
            let Some((kind, stored)) = lookup(t.node, &t.param) else {
                continue;
            };
            // An earlier write this frame to the same parameter (another
            // component of a colour) is the current value.
            let pending = self
                .out
                .iter()
                .position(|w| w.node == t.node && self.param(w.from) == t.param);
            let current = pending.map_or(stored, |p| &self.out[p].value);
            let cur = as_number(kind, current, t.component);
            let Some(y) = step(m, &mut self.state[i], slot, cur) else {
                continue;
            };
            let Some(value) = live_value(kind, current, t.component, y) else {
                continue;
            };
            self.state[i].written = as_number(kind, &value, t.component);
            self.state[i].sent = Some(y);
            match pending {
                Some(p) => self.out[p].value = value,
                None => self.out.push(Write {
                    node: t.node,
                    from: Origin::Mapping(i),
                    value,
                }),
            }
        }
        self.hold();
        for r in 0..self.route_snap.len().min(self.routes.len()) {
            let slot = self.route_snap[r];
            if !slot.fresh || slot.n == 0 {
                continue;
            }
            let route = &self.routes[r];
            let Some((_, current)) = lookup(route.node, &route.param) else {
                continue;
            };
            let value = match &route.kind {
                ParamKind::Colour | ParamKind::Vec { .. } => {
                    let mut a = current.as_array().cloned().unwrap_or_default();
                    a.resize(components(&route.kind).max(a.len()), Value::from(0.0));
                    for (v, x) in a.iter_mut().zip(&slot.args[..usize::from(slot.n)]) {
                        *v = Value::from(*x);
                    }
                    Some(Value::Array(a))
                }
                kind => live_value(kind, current, None, slot.args[0]),
            };
            if let Some(value) = value {
                self.out.push(Write {
                    node: route.node,
                    from: Origin::Route(r),
                    value,
                });
            }
        }
    }

    /// Remembers the values mappings write this frame (allocates only
    /// for a parameter written the first time, or a value changing shape).
    fn hold(&mut self) {
        for w in &self.out {
            let Origin::Mapping(i) = w.from else { continue };
            let param = &self.synced[i].target.param;
            match self
                .held
                .iter_mut()
                .find(|(n, p, _)| *n == w.node && p == param)
            {
                Some((_, _, v)) => assign_value(v, &w.value),
                None => self.held.push((w.node, param.clone(), w.value.clone())),
            }
        }
    }

    /// Parameters no mapping drives since this frame's update (a mapping
    /// was deleted or retargeted), with the value mappings last set: clear
    /// each live value if it is still that one (`Engine::clear_live_if`).
    pub fn released(&self) -> impl Iterator<Item = (NodeId, &str, &Value)> {
        self.released.iter().map(|(n, p, v)| (*n, p.as_str(), v))
    }

    /// Per mapping that has written since it was added (or its target
    /// last changed): its target and the output it last wrote. Saving
    /// stores these, and only these, as parameter values.
    pub fn written(&self) -> impl Iterator<Item = (&Target, f64)> {
        self.synced
            .iter()
            .zip(&self.state)
            .filter_map(|(m, st)| Some((&m.target, st.sent?)))
    }

    /// Forgets what mappings wrote (another project was opened, and its
    /// live values cleared).
    pub fn forget_written(&mut self) {
        for st in &mut self.state {
            st.sent = None;
            st.written = None;
        }
        self.held.clear();
        self.released.clear();
    }

    fn param(&self, from: Origin) -> &str {
        match from {
            Origin::Mapping(i) => &self.synced[i].target.param,
            Origin::Route(r) => &self.routes[r].param,
        }
    }

    /// This frame's live values: (node, parameter, value).
    pub fn writes(&self) -> impl Iterator<Item = (NodeId, &str, &Value)> {
        self.out
            .iter()
            .map(|w| (w.node, self.param(w.from), &w.value))
    }

    /// The mapped parameters (for write-back on save and for the UI).
    pub fn targets(&self) -> impl Iterator<Item = &Target> {
        self.synced.iter().map(|m| &m.target)
    }
}

/// One mapping's output for what arrived (`None`: nothing to write, e.g.
/// waiting for pick-up). `cur` is the parameter's value as a number.
fn step(m: &Mapping, st: &mut MapState, slot: Slot, cur: Option<f64>) -> Option<f64> {
    let (lo, hi) = (m.output[0], m.output[1]);
    let tol = (hi - lo).abs() * PICKUP_TOLERANCE;
    match m.mode {
        Mode::Absolute => {
            let y = map_value(m, slot.raw);
            if !m.pickup {
                return Some(y);
            }
            // Something else moved the parameter: pick up again.
            if let (Some(w), Some(c)) = (st.written, cur)
                && (w - c).abs() > tol.max(1e-9)
            {
                st.engaged = false;
                st.written = None;
            }
            if !st.engaged {
                let Some(c) = cur else {
                    st.engaged = true;
                    return Some(y);
                };
                let near = (y - c).abs() <= tol;
                let passed = st
                    .prev
                    .is_some_and(|p| (p - c).signum() != (y - c).signum());
                st.prev = Some(y);
                if !(near || passed) {
                    return None;
                }
                st.engaged = true;
            }
            Some(y)
        }
        Mode::Toggle => {
            if slot.presses == 0 {
                return None;
            }
            let mid = (lo + hi) / 2.0;
            // Flip what the parameter is now (it may have been changed
            // elsewhere since the last press).
            let on = cur.map_or(st.on.unwrap_or(false), |c| {
                if hi >= lo { c >= mid } else { c <= mid }
            });
            let on = if slot.presses % 2 == 1 { !on } else { on };
            st.on = Some(on);
            Some(if on { hi } else { lo })
        }
        Mode::Momentary => Some(if slot.gate { hi } else { lo }),
        Mode::Relative => {
            if slot.delta == 0.0 {
                return None;
            }
            let span = (m.input[1] - m.input[0]).abs().max(1e-9);
            let c = cur.unwrap_or(lo);
            let y = c + slot.delta / span * (hi - lo);
            Some(y.clamp(lo.min(hi), lo.max(hi)))
        }
    }
}

#[cfg(test)]
mod tests;
