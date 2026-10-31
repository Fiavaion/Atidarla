# Atidarla v1.0 decisions

Exported 2026-10-31 19:15

## PRD decisions marked Proposed

### A62: In-show cues
- Decision: Approve

### A63: Point caches and Sensor In
- Decision: Approve

### A64: Text and table data on wires (DAT)
- Decision: Approve

### A65: Reading point data back into channels
- Decision: Approve

### A66: Animated vector and colour parameters
- Decision: Approve

### A67: Python trust gate for projects
- Decision: Approve

### A67a: Should a project with Python ask on every open, instead of being remembered?
- Decision: Yes

### A67b: Should projects you create yourself allow their own Python automatically?
- Decision: No

### A67c: Is it OK that a pasted copy of allowed Python keeps running?
- Decision: Yes

### A67d: With serve --allow-python-ops, should Python an agent writes run without a prompt?
- Decision: Yes

### A67e: Should OSC parameter changes require --auto-accept?
- Decision: No

## Assistant eval

### EVAL: Accept my changes to the eval criteria
- Decision: Approve

### KEYS: Cloud API keys
- Decision: I'll set the keys

## Packaging and release

### PY: Bundle Python in the installers
- Decision: Bundle it

### VCRT: Microsoft C++ runtime on Windows
- Decision: Include it in the installer

### IDS: Permanent app identity
- Decision: Keep the current values
- macOS bundle ID: org.atidarla.Atidarla
- MSI UpgradeCode: 6B0E3C52-6D4A-4C55-9C1F-2A9B7A3C1D10

### SIGN: Code signing
- Decision: Set up macOS and Windows

### ICON: App icon
- Decision: Generate concepts with local ComfyUI (recommended: I'll supply the artwork)

### TAG: When to tag v1.0
- Decision: After the hand checks pass

## Operators and file formats

### MSH1: Remesh and Bevel scope
- Decision: Add open-border bevelling now (recommended: Fine for v1.0)

### MSH2: Remesh speed
- Decision: Record it as a known cost

### MSH3: Clean defaults
- Decision: Confirm
- Fuse distance: 1e-5
- Fix winding (on/off): on

### SIGK: Rename the ImgToSig operator kind
- Decision: Rename to GpuToSig
- Other name: (blank)

### PERC: The “(per component)” label
- Decision: Keep it as is
- Preferred wording: (blank)

### VAT: VAT texture layout
- Decision: I'll provide a real export
- Path to a VAT export: (blank)

### SOG: SOG splat coordinate frame
- Decision: I'll provide a real SOG capture
- Path to a SOG file: (blank)

### GAPS: Known gaps: which to build before v1.0
- Decision: Build the ticked ones
- Build before v1.0: Fix: Window Out fullscreen and display ignore driven values; Fix: intermittent macOS test crash at process exit

## Hand checks

### H-RTX: PT-3, PT-4 and 1M splats at 60 fps on the RTX 4070
- Decision: (undecided)

### H-RIG: NCAD capture-rig splat renders at 60 fps
- Decision: (undecided)

### H-SOAK: 24-hour PT-10 soak on the NCAD Windows image
- Decision: (undecided)

### H-MSI: Build and install the MSI on Windows
- Decision: (undecided)

### H-LAN: Real NVIDIA LAN host for AI
- Decision: (undecided)

### H-SOV: Sovereign mode packet capture
- Decision: (undecided)

### H-METAL: Install the Xcode Metal toolchain on this Mac
- Decision: (undecided)

### H-USER: Five-person usability test
- Decision: (undecided)
