# Accessibility

Lasair can be used without a mouse, at any interface size, and with colour-blind-safe wire
colours. View > Accessibility shows the scale, the wire legend and the keys below. The same keys
work on Windows and macOS (Ctrl on Windows and Linux is Cmd on macOS).

## Interface scale

Ctrl/Cmd `+` (or `=`) makes everything larger, Ctrl/Cmd `-` smaller, Ctrl/Cmd `0` returns to
100% (20% to 500%). View > Accessibility has the same as buttons. The scale is kept for your user
account, for every project, in `ui.toml` in the app's settings folder:

| OS | Folder |
|---|---|
| Windows | `%APPDATA%\Lasair` |
| macOS | `~/Library/Application Support/Lasair` |
| Linux | `$XDG_CONFIG_HOME/lasair` (or `~/.config/lasair`) |

`lasair-app --smoke` ignores the file and never writes it.

## Keyboard

| Keys | What they do |
|---|---|
| Tab / Shift+Tab | Move keyboard focus to the next / previous control |
| F6 / Shift+F6 | Move focus to the next / previous panel's tab bar (then Tab walks into the panel) |
| Arrows (in a panel) | Move focus between nearby controls |
| Enter or Space | Press the focused button or tab |
| Ctrl/Cmd `+` / `-` / `0` | Interface larger / smaller / 100% |
| Ctrl/Cmd+Z, Ctrl/Cmd+Shift+Z (or Ctrl/Cmd+Y) | Undo, redo |
| Ctrl/Cmd+N, O, S, Shift+S | New, open, save, save as |

### Patch canvas

Tab to the canvas: a ring shows it has focus and a line at the top lists the keys. Nodes and
sockets are not tab stops, so Tab leaves the canvas in one press.

| Keys | What they do |
|---|---|
| Arrows | Select the nearest node that way (nodes within 45° first). With nothing selected, start from the centre |
| Shift+arrows | Move the selection 20 units (Shift+Ctrl/Cmd+arrows: 100). Each press is one undo step |
| W | Start a wire from the selected node's first output; W again picks the next output |
| then arrows | Choose the node to wire to (it is outlined, the wire is previewed) |
| then I | Cycle the target's inputs (inputs of the wire's type first, free ones before connected ones) |
| then Enter | Connect; the target becomes the selection, so W, arrows, Enter chain a patch |
| then Esc | Cancel the wire |
| N | Add an operator at the centre (type to search, Enter adds, Esc closes); focus returns to the canvas |
| Delete / Backspace | Delete the selection |
| Ctrl/Cmd+A, Ctrl/Cmd+D | Select all, duplicate |
| Ctrl/Cmd+C, X, V | Copy, cut, paste |
| C | Put the selection in a comment box |
| F | Frame the whole patch |
| Esc (no wire) | Leave the canvas |

The parameter panel is ordinary controls: Tab to a field and type, or use the arrows on a focused
slider.

## Wire colours

Wires are coloured by type with six colours of the Okabe–Ito palette, chosen as the six that stay
furthest apart under simulated protanopia, deuteranopia and tritanopia. Colour is never the only
cue: each type has its own socket shape, socket tooltips name the type, and View > Accessibility >
"Label every wire with its type" draws the type on every wire.

| Type | Colour | Socket | Label |
|---|---|---|---|
| Image | sky blue `#56B4E9` | circle | IMG |
| Signal (channels) | yellow `#F0E442` | square | SIG |
| Geometry | blue `#0072B2` | triangle | GEO |
| Data | orange `#E69F00` | diamond | DAT |
| Material | vermillion `#D55E00` | hexagon | MAT |
| Composition | grey `#BBBBBB` | ring | CMP |

`crates/lsr-app/src/wires.rs` tests the palette: with the Machado, Oliveira & Fernandes (2009)
simulation at full severity, every pair stays at least 15 ΔE\*ab apart in normal vision and under
each dichromacy (the closest pair is yellow and orange under deuteranopia, 17), every colour has
at least 3:1 contrast against the dark canvas, and labels have at least 4.5:1 on their wire.
Feedback inputs keep their extra ring, and channel bindings and feedback wires stay dashed.
