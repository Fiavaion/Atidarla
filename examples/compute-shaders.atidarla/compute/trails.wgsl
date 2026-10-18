// Trails: a state image remembers where the input was lit, fading by
// decay() per frame; a buffer counts how long each pixel has stayed lit.
#bind input in0
#bind state trail r32float
#bind buffer age pixels f32
#bind param p1 decay f32
#bind param p2 tint vec3f
#bind output dst rgba8unorm

fn kernel(id: vec2u) {
    let c = textureLoad(in0, vec2i(id), 0);
    let lit = dot(c.rgb, vec3f(0.2126, 0.7152, 0.0722)) * c.a;
    let v = max(textureLoad(trail, vec2i(id), 0).r * decay(), lit);
    textureStore(trail_out, id, vec4f(v, 0.0, 0.0, 1.0));
    let i = id.y * size().x + id.x;
    age[i] = select(age[i] * 0.98, age[i] + 1.0, lit > 0.5);
    let warm = clamp(age[i] / 30.0, 0.0, 1.0);
    textureStore(dst, id, vec4f(v * mix(tint(), vec3f(1.0, 0.95, 0.85), warm), 1.0));
}
