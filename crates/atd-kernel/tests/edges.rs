//! Constructs WGSL is strict about, each compiled and validated by naga, plus the
//! binding layout rules the engine relies on.

use atd_kernel::{
    Access, AttrSchema, AttrType, CompileOptions, FrameGlobals, Scalar, Target, Type, compile,
};

fn ok(src: &str, target: Target) -> atd_kernel::Compiled {
    compile(src, &CompileOptions::new(target)).unwrap_or_else(|d| {
        panic!(
            "does not compile:\n{src}\n{}",
            d.iter().map(|d| d.render("k", src)).collect::<String>()
        )
    })
}

#[test]
fn strict_wgsl_constructs_validate() {
    let points = [
        "",
        "// only a comment",
        "return; @P.y = 1;",
        "for (int i = 0; i < 3; i++) { break; @P.y = i; }",
        "for (int i = 3; i > 0; --i) { continue; }",
        "foreach (int a; nearpoints(0, @P, 1, 4)) { foreach (int b; nearpoints(0, @P, 1, 4)) { @P.y += a * b; } }",
        "float loop = 1; float var = 2; float fn = 3; float target = loop + var + fn; @P.y = target;",
        "void bump() { return; }\nbump();\nfloat one() { return 1; }\none();",
        "float main(float main) { return main; }\n@P.y = main(2);",
        "mat3 m = ident3(); mat3 n = m * 2; n *= m; n = n + m - m; @P = n * @P; @P = @P * n;",
        "mat4 m = ident(); m[2] = {0, 0, 1, 0}; int k = 1; m[k] = m[k] * 2; @P.x = m[k][k];",
        "vec4 v = 0; int i = 5; v[i] = 1; @P.y = v[i] + v[i - 8];",
        "@P.y = true == false ? 1 : 0; @P.x = int(true) << 3 >> 1;",
        "int m = -2147483648; @P.y = m / -1 + m % -1;",
        "@P = 1; @P += 2; @P.x++; @P[1]--;",
        "vec3 a = select_free(); vec3 select_free() { return 1; }",
        "@P = point(0, \"P\", @ptnum - 1) + point(0, \"v\", 0); v@v = 0;",
        "i@id = i@id * 0x9E3779B1; f@r = rand(i@id);",
        "@Cd = texture(3, @P.xy); f@g = texture(@P.xz).g;",
        "float f() { return ch(\"k\") + float(chi(\"n\", 2)); }\n@P.y = f() + length(chu(\"u\", {1, 2})) + chp(\"q\", 1).w;",
        "if (1) { } else if (0.5) { } else { @P.y = 2; }",
        "bool b = @P.x > 0 && !(@P.y < 0) || @ptnum == 0; if (b) @P.z = 1;",
    ];
    for src in points {
        ok(src, Target::Points);
    }
    let pixels = [
        "",
        "return;",
        "@Cd.r = @ix / 2 + @iy % 3; @Cd.g = @P.x * @res.x;",
        "vec4 a = texture(1, @P); vec4 b = texture(2, @P); @Cd = a * b;",
        "float f(vec2 uv) { return texture(uv).r; }\n@Cd = f(@P);",
        "@Cd = {@Time, @TimeInc, @Frame, 1};",
    ];
    for src in pixels {
        ok(src, Target::Pixels);
    }
}

#[test]
fn bindings_follow_the_layout_rules() {
    let c = ok(
        "v@v += 1; @P.y = point(0, \"P\", 0).y; @P.x += f@age; i@id = 1; u@uv = 0;",
        Target::Points,
    );
    let b = &c.bindings;
    assert_eq!(b.workgroup_size, atd_kernel::bindings::WORKGROUP_SIZE);
    assert_eq!(b.entry_point, "main");
    let h = b.header.as_ref().expect("header");
    assert_eq!((h.group, h.binding), (0, 0));
    assert_eq!((b.uniforms.group, b.uniforms.binding), (0, 1));
    assert!(b.sampler.is_none());
    let rows: Vec<(&str, u32, u32, Access, bool, u32)> = b
        .attributes
        .iter()
        .map(|a| {
            (
                a.name.as_str(),
                a.group,
                a.binding,
                a.access,
                a.snapshot,
                a.stride,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("P", 1, 0, Access::ReadWrite, false, 16),
            ("P", 1, 1, Access::Read, true, 16),
            ("age", 1, 2, Access::Read, false, 4),
            ("id", 1, 3, Access::ReadWrite, false, 4),
            ("uv", 1, 4, Access::ReadWrite, false, 8),
            ("v", 1, 5, Access::ReadWrite, false, 16),
        ]
    );
    let id = &b.attributes[3];
    assert_eq!(id.ty, AttrType::U32);
    assert_eq!(id.kernel_type, Type::Int);
    assert_eq!(b.storage_buffers(), 7);
    assert_eq!(
        c.state_keys(),
        [
            ("P".to_owned(), AttrType::VEC3),
            ("id".to_owned(), AttrType::U32),
            ("uv".to_owned(), AttrType::VEC2),
            ("v".to_owned(), AttrType::VEC3),
        ]
    );
    assert!(c.wgsl.contains("array<vec3<f32>>"));

    let c = ok("@Cd = texture(2, @P.xy);", Target::Points);
    let s = c.bindings.sampler.as_ref().expect("sampler");
    assert_eq!((s.group, s.binding), (0, 2));
    let imgs: Vec<(u32, u32, u32)> = c
        .bindings
        .images
        .iter()
        .map(|i| (i.input, i.group, i.binding))
        .collect();
    assert_eq!(imgs, [(2, 2, 2)]);

    let c = ok("@Cd = texture(3, @P);", Target::Pixels);
    let b = &c.bindings;
    assert!(b.header.is_none());
    assert_eq!(b.entry_point, "fs");
    assert_eq!(b.vertex_entry_point.as_deref(), Some("vs"));
    assert_eq!((b.uniforms.group, b.uniforms.binding), (0, 0));
    let imgs: Vec<(u32, u32, u32)> = b
        .images
        .iter()
        .map(|i| (i.input, i.group, i.binding))
        .collect();
    assert_eq!(imgs, [(0, 1, 0), (3, 1, 3)]);
}

#[test]
fn input_schema_types_untyped_attributes() {
    let mut opts = CompileOptions::new(Target::Points);
    opts.input_schema = Some(vec![
        AttrSchema {
            name: "Cd".into(),
            ty: AttrType::VEC3,
        },
        AttrSchema {
            name: "mask".into(),
            ty: AttrType::new(Scalar::I32, 1),
        },
        AttrSchema {
            name: "big".into(),
            ty: AttrType::new(Scalar::U32, 3),
        },
    ]);
    // Without a schema `Cd` is a vec4; the input says vec3.
    let c = compile("@Cd = @Cd * 0.5; @P.y = @mask;", &opts).expect("compiles");
    let cd = c
        .bindings
        .attributes
        .iter()
        .find(|a| a.name == "Cd")
        .expect("Cd");
    assert_eq!(cd.ty, AttrType::VEC3);
    let d = compile("@P.y = p@Cd.x;", &opts).expect_err("sigil disagrees with the input");
    assert!(
        d[0].message.contains("the input's `Cd` is a `vec3`"),
        "{d:?}"
    );
    // Written with another sigil, the attribute is redefined (FR-K4: an edit in a
    // running simulation may change a type; the new attribute starts at zero).
    let c = compile("p@Cd = 1;", &opts).expect("a written attribute is redefined");
    let cd = c
        .bindings
        .attributes
        .iter()
        .find(|a| a.name == "Cd")
        .expect("Cd");
    assert_eq!(cd.ty, AttrType::VEC4);
    let d = compile("@P.y = @big.x;", &opts).expect_err("unsupported storage");
    assert!(d[0].message.contains("u32x3"), "{d:?}");
    // No schema: the sigil wins over the standard table.
    let c = ok("v@Cd = 1;", Target::Points);
    assert_eq!(c.bindings.attributes[0].ty, AttrType::VEC3);
}

#[test]
fn params_and_uniform_layout() {
    let c = ok(
        "@P.y = ch(\"a\", 0.5, 0, 1) + chi(\"n\", -3) + chv(\"d\", {1, 2, 3}).z + ch(\"a\");",
        Target::Points,
    );
    type Row<'a> = (&'a str, u32, [f32; 4], Option<f32>, Option<f32>);
    let p: Vec<Row> = c
        .params
        .iter()
        .map(|p| (p.name.as_str(), p.slot, p.default, p.min, p.max))
        .collect();
    assert_eq!(
        p,
        [
            ("a", 0, [0.5, 0.0, 0.0, 0.0], Some(0.0), Some(1.0)),
            ("n", 1, [-3.0, 0.0, 0.0, 0.0], None, None),
            ("d", 2, [1.0, 2.0, 3.0, 0.0], None, None),
        ]
    );
    assert_eq!(c.bindings.uniform_size, 32 + 3 * 16);
    let mut buf = vec![0u8; c.bindings.uniform_size as usize];
    let g = FrameGlobals {
        time: 2.0,
        time_inc: 0.25,
        frame: 8.0,
        width: 0,
        height: 0,
    };
    c.write_uniforms(&g, &[[0.75, 0.0, 0.0, 0.0]], &mut buf);
    let word = |i: usize| u32::from_le_bytes(buf[i * 4..i * 4 + 4].try_into().expect("4 bytes"));
    assert_eq!(f32::from_bits(word(0)), 2.0);
    assert_eq!(f32::from_bits(word(1)), 0.25);
    assert_eq!(f32::from_bits(word(2)), 8.0);
    assert_eq!(f32::from_bits(word(8)), 0.75, "given value");
    assert_eq!(word(12) as i32, -3, "int default as i32 bits");
    assert_eq!(f32::from_bits(word(18)), 3.0, "vec3 default .z");
}
