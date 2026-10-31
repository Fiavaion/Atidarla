# 2. Your first patch in 10 minutes

You will build a moving kaleidoscope from three nodes and one oscillator. By the end you will
know what a node, a wire and a parameter are, and how to make one thing move another.

This chapter is the short version of the first Learn quest,
[`learn/quests/01-first-patch.toml`](../../learn/quests/01-first-patch.toml). The quest is
the better way to do it the first time: it ticks each step off for you as you do it, and it
estimates 12 minutes. To use it, choose **View, Learn**, pick **Your first patch** and press
**Start in a new project**. The steps below are the same ones.

## The ideas in one minute

- A **patch** is a network of **nodes** joined by **wires**.
- Each node does one small job. A **Noise** node makes noise. A **Level** node changes
  brightness and contrast.
- The picture flows along the wires from **left to right**. A node's *outputs* are on its right
  and its *inputs* are on its left.
- A node's settings are its **parameters**. You change them in the **Parameters** panel.
- Not everything is a picture. **SIG** nodes make **signals**: numbers that change over time.
  You can plug a signal into a parameter to make it move.

## Before you start

1. Start Atidarla ([chapter 1](01-install.md)).
2. Choose **File, New Project** (Cmd/Ctrl+N) for an empty patch. If you used the Learn button,
   this is done for you.

If a panel is missing, choose it from the **View** menu.

## Step by step

### 1. Add a Noise node

Open the **Operators** panel on the left, unfold **IMG** (image operators) and click **Noise**.
A node called `noise1` appears on the **Patch** canvas.

Shortcut: press **Tab** over the canvas (or double-click it), type part of the name, and press
Enter.

### 2. Add a Kaleidoscope and wire it

Add **Kaleidoscope** from the IMG group. Then drag from the small socket on the **right** of
`noise1` to the socket named `in` on the **left** of `kaleidoscope1`.

A wire means "use this node's picture as that node's input". To remove a wire, right-click the
input socket it goes into.

### 3. Change a parameter

Click `kaleidoscope1` to select it. The **Parameters** panel shows its controls. Change
**Segments** to another number. Look at the **Viewer**: the picture changes.

Every change is an edit you can take back: press **Cmd/Ctrl+Z**. Redo is Cmd/Ctrl+Shift+Z.

### 4. Add a Level node at the end

Add **Level** (IMG) and wire `kaleidoscope1` into it. Level changes brightness, contrast and
gamma. It is the end of this chain.

### 5. Look at the result

Click the title bar of `level1`. The **Viewer** shows the picture of the selected node. A node
does its work (it *cooks*) every frame while something needs its picture.

If a node has a problem, its message appears under it on the canvas.

The Viewer has modes (RGBA, RGB, R, G, B, A) and an exposure control. Hover over the picture to
see pixel coordinates.

### 6. Make the contrast stronger

With `level1` selected, drag **Contrast** above 1. Values above 1 push light and dark apart.

### 7. Add an oscillator

Open the **SIG** group in the Operators panel and add **LFO** (a low-frequency oscillator). It
outputs a smooth wave in a signal called `lfo`.

### 8. Let the oscillator move the noise

Select `noise1`. Next to the **Z** parameter there is a **bind...** menu. Choose `lfo1:lfo`.
Z now follows the oscillator, and the noise drifts by itself.

Z moves the noise through its third dimension. Now change the LFO's **Frequency** and
**Amplitude** while you watch the Viewer.

You have just used a **binding**: a parameter that follows a signal. Chapter 5 explains
bindings and expressions in full.

### 9. Save your work

Choose **File, Save As...** (Cmd/Ctrl+Shift+S) and pick a name. Atidarla saves a **project
folder** ending in `.atidarla`. Inside, the files are plain text. You can open it again with
**File, Open Project...** (Cmd/Ctrl+O).

## Handy controls

| Do | How |
|---|---|
| Add an operator | Tab or double-click the canvas, type, Enter |
| Wire | Drag from an output (right) to an input (left); right-click an input to disconnect |
| Select | Click; Shift/Ctrl-click to add; Shift-drag a box; Ctrl+A |
| Copy, paste, duplicate | Ctrl+C, Ctrl+V (works across windows), Ctrl+D |
| Comment box | Press C to wrap the selection in one; drag its title bar to move it with its nodes; double-click to edit |
| Delete, undo, redo | Delete; Cmd/Ctrl+Z; Cmd/Ctrl+Shift+Z |
| Navigate | Drag the background or middle-drag to pan, scroll to zoom, click the minimap |
| Bind a parameter | The **bind...** menu, or drop a wire dragged from a SIG output onto the parameter's name |

(From the repository [README](../../README.md).)

## What next

- Try the other quests: [Learn mode](../learn.md) lists them. "Make a picture follow sound" is a
  good second one.
- Open an example project from **File, Open Project...**. The [examples](../../examples/README.md)
  folder says what each one shows.
- Read [the three views](03-views.md) to see how the same patch becomes a performance.
- Look up any node in the [operator reference](../operators/README.md), for example
  [Noise](../operators/img/noise.md), [Kaleidoscope](../operators/img/kaleidoscope.md),
  [Level](../operators/img/level.md) and [LFO](../operators/sig/lfo.md).
