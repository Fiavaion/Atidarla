/*{
    "DESCRIPTION": "Concentric rings that drift outwards, glowing.",
    "CATEGORIES": ["Generator"],
    "INPUTS": [
        {"NAME": "rings", "TYPE": "float", "DEFAULT": 12.0, "MIN": 1.0, "MAX": 64.0},
        {"NAME": "tint", "TYPE": "color", "DEFAULT": [0.3, 0.7, 1.0, 1.0]}
    ]
}*/
void main() {
    vec2 p = isf_FragNormCoord - vec2(0.5);
    float d = length(p);
    float v = 0.5 + 0.5 * sin(d * rings * 6.2831 - TIME * 2.0);
    gl_FragColor = vec4(tint.rgb * pow(v, 3.0), 1.0);
}
