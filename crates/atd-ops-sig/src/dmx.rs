//! DMX Out (PRD §6, SIG devices & protocols): input channels to DMX512
//! slots over Art-Net or sACN, via `atd_io::dmx` (sent from its own thread,
//! rule 1).

use atd_core::registry::ParamSchema;
use atd_gpu::engine::{SigCtx, SigOp};
use atd_io::dmx::{DmxSender, Protocol};

use crate::Def;
use crate::ops::sig;

const PROTOCOLS: &[&str] = &["artnet", "sacn"];

#[derive(Debug, Default)]
pub struct DmxOut {
    /// (protocol, host, port, universe) the sender was opened with.
    config: Option<(String, String, i64, i64)>,
    sender: Option<DmxSender>,
    slots: Vec<u8>,
}

impl SigOp for DmxOut {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        if p.mode().is_offline() {
            // Sinks do not emit in an offline render (FR-H1): pass through.
            match ctx.inputs.get(0) {
                Some(input) => ctx.output.copy_from(input),
                None => ctx.output.set_single(&[], &[]),
            }
            return Ok(());
        }
        let (protocol, host) = (p.str("protocol"), p.str("host"));
        let port = p.i64("port").clamp(0, 65_535);
        let universe = p.i64("universe").clamp(0, 63_999);
        // Compared in place: steady-state cooks must not allocate (rule 2).
        let same = self.config.as_ref().is_some_and(|(pr, h, po, u)| {
            pr == protocol && h == host && *po == port && *u == universe
        });
        if !same {
            let config = (protocol.to_owned(), host.to_owned(), port, universe);
            let protocol = if config.0 == "sacn" {
                Protocol::Sacn
            } else {
                Protocol::ArtNet
            };
            let port = (config.2 > 0).then_some(config.2 as u16);
            self.sender = None;
            self.config = Some(config.clone());
            self.sender = Some(
                DmxSender::new(protocol, &config.1, port, config.3 as u16)
                    .map_err(|e| format!("DMX out: {e}"))?,
            );
            self.slots.clear();
            self.slots.reserve(512);
        }
        let Some(input) = ctx.inputs.get(0) else {
            ctx.output.set_single(&[], &[]);
            return Ok(());
        };
        ctx.output.copy_from(input);
        let Some(sender) = &self.sender else {
            return Err("DMX out is not open".into());
        };
        let start = (p.i64("start").clamp(1, 512) - 1) as usize;
        let count = input.len().min(512 - start);
        self.slots.clear();
        self.slots.resize(start + count, 0);
        for c in 0..count {
            self.slots[start + c] = (input.last(c).clamp(0.0, 1.0) * 255.0).round() as u8;
        }
        sender.publish(&self.slots);
        Ok(())
    }
}

pub static DEFS4: &[Def] = &[Def {
    schema: || {
        let mut s = sig(
            "dmx_out",
            "DMX Out",
            "Sends input channels (0–1) as DMX512 slots from Start, over Art-Net or sACN (E1.31). The universe is sent when it changes (at most 44 times a second) and repeated every second.",
            &["in"],
            vec![
                ParamSchema::menu("protocol", "Protocol", PROTOCOLS, "artnet")
                    .help("`artnet` (UDP 6454) or `sacn` (E1.31, UDP 5568)."),
                ParamSchema::string("host", "Host", "")
                    .help("Destination node; empty broadcasts (Art-Net) or uses the universe's multicast group (sACN)."),
                ParamSchema::int("port", "Port", 0, Some(0), Some(65_535))
                    .help("UDP port; 0 for the protocol's standard port."),
                ParamSchema::int("universe", "Universe", 1, Some(0), Some(63_999))
                    .help("Art-Net port address (0–32767) or sACN universe (1–63999)."),
                ParamSchema::int("start", "Start", 1, Some(1), Some(512))
                    .help("DMX slot (channel) for the first input channel."),
            ],
            false,
        );
        s.sink = true;
        s
    },
    create: || Box::<DmxOut>::default(),
}];

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::UdpSocket;
    use std::time::Duration;

    use atd_core::graph::{Edit, Network, Node, Param};
    use atd_core::registry::Registry;
    use atd_gpu::engine::{Channels, Params, SigInputs};
    use serde_json::json;

    use super::*;

    #[test]
    fn dmx_out_sends_scaled_slots_from_start() {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        rx.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let port = rx.local_addr().unwrap().port();

        let mut reg = Registry::default();
        crate::register(&mut reg);
        let mut net = Network::default();
        let id = net.alloc_id();
        let src = net.alloc_id();
        net.apply(
            &reg,
            Edit::AddNode {
                node: Node::new(id, "sig.dmx_out", "dmx1"),
            },
        )
        .unwrap();
        net.apply(
            &reg,
            Edit::AddNode {
                node: Node::new(src, "sig.constant", "c1"),
            },
        )
        .unwrap();
        for (p, v) in [
            ("host", json!("127.0.0.1")),
            ("port", json!(port)),
            ("universe", json!(5)),
            ("start", json!(3)),
        ] {
            net.apply(
                &reg,
                Edit::SetParam {
                    node: id,
                    param: p.into(),
                    value: Some(Param::constant(v)),
                },
            )
            .unwrap();
        }
        let mut sig = HashMap::new();
        sig.insert(src, Channels::single(&["a", "b", "c"], &[1.0, 0.5, -1.0]));
        let sources = [Some(src)];
        let mut op = DmxOut::default();
        let mut out = Channels::default();
        let mut ctx = SigCtx {
            inputs: SigInputs::new(&sources, &sig),
            output: &mut out,
            params: Params::new(&net, &reg, &sig, id),
            time: 0.0,
            dt: 0.016,
            frame: 0,
        };
        op.cook(&mut ctx).unwrap();
        assert_eq!(out.value("b"), Some(0.5), "passes the input through");

        let mut buf = [0u8; 600];
        let n = rx.recv(&mut buf).unwrap();
        assert_eq!(&buf[..8], b"Art-Net\0");
        assert_eq!((buf[14], buf[15]), (5, 0), "universe 5");
        // Slots 1–2 unused, then 255, 128, 0 (clamped); padded to even.
        assert_eq!(&buf[18..n], &[0, 0, 255, 128, 0, 0]);
    }
}
