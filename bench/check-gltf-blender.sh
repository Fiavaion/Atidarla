#!/usr/bin/env bash
# Opens a glTF/GLB written by Atidarla in Blender (headless) and prints what it found, then runs
# the Khronos validator when npx is available. Manual check (M2 AC: "Boolean -> UV Project ->
# Mesh File Out glTF opens in Blender"); CI does not depend on Blender.
#
#   bench/check-gltf-blender.sh                       # renders the example, then checks its .glb
#   bench/check-gltf-blender.sh path/to/file.glb      # checks that file only
#
# BLENDER overrides the Blender binary (default: the macOS app, else `blender` on PATH).
set -euo pipefail
cd "$(dirname "$0")/.."

blender="${BLENDER:-/Applications/Blender.app/Contents/MacOS/Blender}"
[ -x "$blender" ] || blender="$(command -v blender)"

file="${1:-}"
if [ -z "$file" ]; then
  file="examples/mesh-boolean-gltf.atidarla/.cache/boolean.glb"
  cargo run -q -p atd-cli --release -- render examples/mesh-boolean-gltf.atidarla \
    --out .scratch/mesh-boolean-gltf --range 0-1
fi
[ -f "$file" ] || { echo "no such file: $file" >&2; exit 1; }

"$blender" -b --factory-startup --python-expr "
import bpy
for o in list(bpy.context.scene.objects):
    bpy.data.objects.remove(o, do_unlink=True)  # drop the default cube
bpy.ops.import_scene.gltf(filepath='$file')
m = [o for o in bpy.context.scene.objects if o.type == 'MESH']
for o in m:
    o.data.calc_loop_triangles()
print('BLENDER', bpy.app.version_string)
print('MESHES', len(m), 'polygons', [len(o.data.polygons) for o in m],
      'triangles', [len(o.data.loop_triangles) for o in m],
      'uv_layers', [len(o.data.uv_layers) for o in m])
" 2>&1 | grep -E '^(BLENDER|MESHES)|Error|Traceback'

# Khronos validator (npm package `gltf-validator`: a library, no command line, so call its API).
if command -v npm >/dev/null; then
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  (cd "$tmp" && npm init -y >/dev/null 2>&1 && npm i gltf-validator >/dev/null 2>&1)
  node -e "
const v = require('$tmp/node_modules/gltf-validator');
const f = process.argv[1];
v.validateBytes(new Uint8Array(require('fs').readFileSync(f)), { uri: f }).then(r => {
  const i = r.issues;
  console.log('VALIDATOR', r.validatorVersion, 'errors', i.numErrors, 'warnings', i.numWarnings, 'infos', i.numInfos);
  for (const m of i.messages) console.log(' ', m.severity, m.code, m.pointer, m.message);
});" "$file"
else
  echo "npm not found: skipped the Khronos validator"
fi
