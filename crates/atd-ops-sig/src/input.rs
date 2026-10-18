//! Input Devices (PRD §6, SIG devices & protocols): mouse, keyboard and
//! gamepad as channels, via `atd_io::input` (snapshots taken with
//! `try_lock`; the gamepad poller runs on its own thread).

use atd_core::registry::ParamSchema;
use atd_gpu::engine::{SigCtx, SigOp};
use atd_io::input::{PAD_AXES, PAD_BUTTONS, Pad};

use crate::Def;
use crate::ops::sig;

const DEVICES: &[&str] = &["mouse", "keyboard", "gamepad"];

#[derive(Default)]
pub struct InputDevices {
    /// Last values, kept when a snapshot is busy.
    pointer: atd_io::input::Pointer,
    pad: Option<Pad>,
}

impl SigOp for InputDevices {
    fn cook(&mut self, ctx: &mut SigCtx<'_>) -> Result<(), String> {
        let p = ctx.params;
        // A live input is idle in an offline render (FR-H1): no key held,
        // no gamepad, the pointer at rest.
        let offline = p.mode().is_offline();
        match p.str("device") {
            "keyboard" if offline => {
                let mut w = ctx.output.writer();
                for k in p.str("keys").split_whitespace() {
                    w.push(k, 0.0);
                }
            }
            "keyboard" => {
                let keys = p.str("keys");
                let mut w = ctx.output.writer();
                let fresh = atd_io::input::with_keys(keys, |k, held| {
                    w.push(k, f32::from(u8::from(held)));
                });
                if !fresh {
                    // Busy this instant: keep last frame's values.
                    w.keep_previous();
                }
            }
            "gamepad" => {
                let index = p.i64("gamepad").clamp(1, 16) as usize - 1;
                if offline {
                    self.pad = None;
                } else {
                    match atd_io::input::pad(index)? {
                        Some(pad) => self.pad = Some(pad),
                        None => self.pad = None,
                    }
                }
                let pad = self.pad.unwrap_or_default();
                let mut w = ctx.output.writer();
                for (name, v) in PAD_AXES.iter().zip(pad.axes) {
                    w.push(name, v);
                }
                for (name, b) in PAD_BUTTONS.iter().zip(pad.buttons) {
                    w.push(name, f32::from(u8::from(b)));
                }
                w.push("connected", f32::from(u8::from(self.pad.is_some())));
            }
            _ => {
                if offline {
                    self.pointer = atd_io::input::Pointer::default();
                } else if let Some(ptr) = atd_io::input::pointer() {
                    self.pointer = ptr;
                }
                let m = self.pointer;
                let b = |i: usize| f32::from(u8::from(m.buttons[i]));
                ctx.output.set_single(
                    &["x", "y", "inside", "left", "middle", "right", "wheel"],
                    &[
                        m.x,
                        m.y,
                        f32::from(u8::from(m.inside)),
                        b(0),
                        b(1),
                        b(2),
                        m.wheel,
                    ],
                );
            }
        }
        Ok(())
    }
}

pub static DEFS8: &[Def] = &[Def {
    schema: || {
        sig(
            "input",
            "Input Devices",
            "Mouse, keyboard or gamepad as channels. Mouse: position over the viewed image (0–1, y up), buttons and wheel. Keyboard: 1 while each listed key is held. Gamepad: sticks, triggers and buttons.",
            &[],
            vec![
                ParamSchema::menu("device", "Device", DEVICES, "mouse")
                    .help("Which input to read."),
                ParamSchema::string("keys", "Keys", "space")
                    .help("Keyboard: key names separated by spaces (`a`, `space`, `arrowup`, `1`, `enter`); one channel each."),
                ParamSchema::int("gamepad", "Gamepad", 1, Some(1), Some(16))
                    .help("Gamepad: which connected gamepad (1 = first)."),
            ],
            true,
        )
    },
    create: || Box::<InputDevices>::default(),
}];

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use atd_core::graph::{Edit, Network, Node, Param};
    use atd_core::registry::Registry;
    use atd_gpu::engine::{Channels, Params, SigInputs};
    use serde_json::{Value, json};

    use super::*;

    fn run(op: &mut InputDevices, params: &[(&str, Value)]) -> Channels {
        let mut reg = Registry::default();
        crate::register(&mut reg);
        let mut net = Network::default();
        let id = net.alloc_id();
        net.apply(
            &reg,
            Edit::AddNode {
                node: Node::new(id, "sig.input", "in1"),
            },
        )
        .unwrap();
        for (p, v) in params {
            net.apply(
                &reg,
                Edit::SetParam {
                    node: id,
                    param: (*p).into(),
                    value: Some(Param::constant(v.clone())),
                },
            )
            .unwrap();
        }
        let sig = HashMap::new();
        let mut out = Channels::default();
        let mut ctx = SigCtx {
            inputs: SigInputs::new(&[], &sig),
            output: &mut out,
            params: Params::new(&net, &reg, &sig, id),
            time: 0.0,
            dt: 0.016,
            frame: 0,
        };
        op.cook(&mut ctx).unwrap();
        out
    }

    // One test: the desktop state is process-wide.
    #[test]
    fn mouse_keyboard_and_gamepad_channels() {
        atd_io::input::set_pointer(atd_io::input::Pointer {
            x: 0.25,
            y: 0.5,
            inside: true,
            buttons: [false, false, true],
            wheel: -1.0,
        });
        let mut op = InputDevices::default();
        let out = run(&mut op, &[]);
        assert_eq!(out.value("x"), Some(0.25));
        assert_eq!(out.value("inside"), Some(1.0));
        assert_eq!(out.value("right"), Some(1.0));
        assert_eq!(out.value("wheel"), Some(-1.0));

        atd_io::input::set_keys(["W", "space"]);
        let out = run(
            &mut op,
            &[("device", json!("keyboard")), ("keys", json!("w a space"))],
        );
        assert_eq!(out.names().collect::<Vec<_>>(), ["w", "a", "space"]);
        assert_eq!((out.value("w"), out.value("a")), (Some(1.0), Some(0.0)));
        atd_io::input::set_keys([]);

        // No gamepad connected here: zeros and `connected` 0 (or a clear
        // error where the platform has no gamepad support).
        let mut op = InputDevices::default();
        if atd_io::input::pad(0).is_ok() {
            let out = run(&mut op, &[("device", json!("gamepad"))]);
            assert_eq!(out.len(), PAD_AXES.len() + PAD_BUTTONS.len() + 1);
            assert_eq!(out.value("connected"), Some(0.0));
        }
    }
}
