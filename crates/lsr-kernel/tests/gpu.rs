//! GPU semantics checks: the generated WGSL, run on a real device, gives the same
//! numbers as the reference interpreter and the Rust noise library.
//!
//! Bind group layouts are built from `Compiled::bindings` alone, as the engine would,
//! so this also checks that the binding description is complete. Skips without a GPU
//! unless `LSR_REQUIRE_GPU` is set; tests run one at a time (two WARP devices in
//! parallel crash).

use std::sync::Mutex;

use lsr_kernel::interp::{Attribute, Geometry, run_points};
use lsr_kernel::{Access, CompileOptions, Compiled, FrameGlobals, Scalar, Target, compile, noise};

static GPU: Mutex<()> = Mutex::new(());

fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = lsr_gpu::primary_instance();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        let required = std::env::var("LSR_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(
            !required,
            "LSR_REQUIRE_GPU is set but there is no GPU adapter"
        );
        eprintln!("skipping: no GPU adapter");
        return None;
    };
    eprintln!("adapter: {:?}", adapter.get_info().name);
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

/// Runs `c` for `frames` steps on the GPU, the way the engine would: the input of each
/// frame is the output of the last; snapshot bindings get a copy of the input.
fn run_gpu(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    c: &Compiled,
    geo: &Geometry,
    values: &[[f32; 4]],
    frames: &[FrameGlobals],
) -> Geometry {
    let b = &c.bindings;
    let count = geo.count;
    let storage =
        wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST;
    let buffer = |label: &str, words: &[u32], usage| {
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: (words.len().max(4) * 4) as u64,
            usage,
            mapped_at_creation: false,
        });
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        queue.write_buffer(&buf, 0, &bytes);
        buf
    };
    let header = buffer("header", &[count as u32, count as u32], storage);
    let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("uniforms"),
        size: b.uniform_size,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let attrs: Vec<wgpu::Buffer> = b
        .attributes
        .iter()
        .map(|a| {
            let words = geo
                .get(&a.name)
                .map(|x| x.words.clone())
                .unwrap_or_else(|| Attribute::zeroed(a.ty, count).words);
            buffer(&a.wgsl_name, &words, storage)
        })
        .collect();

    let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty,
        count: None,
    };
    let storage_ty = |read_only| wgpu::BindingType::Buffer {
        ty: wgpu::BufferBindingType::Storage { read_only },
        has_dynamic_offset: false,
        min_binding_size: None,
    };
    let h = b.header.as_ref().expect("points have a header");
    let g0 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("kernel g0"),
        entries: &[
            entry(h.binding, storage_ty(true)),
            entry(
                b.uniforms.binding,
                wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
            ),
        ],
    });
    let g1_entries: Vec<wgpu::BindGroupLayoutEntry> = b
        .attributes
        .iter()
        .map(|a| entry(a.binding, storage_ty(a.access == Access::Read)))
        .collect();
    let g1 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("kernel g1"),
        entries: &g1_entries,
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("kernel"),
        bind_group_layouts: &[Some(&g0), Some(&g1)],
        immediate_size: 0,
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("kernel"),
        source: wgpu::ShaderSource::Wgsl(c.wgsl.as_str().into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("kernel"),
        layout: Some(&layout),
        module: &module,
        entry_point: Some(&b.entry_point),
        compilation_options: Default::default(),
        cache: None,
    });
    let bind0 = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("kernel g0"),
        layout: &g0,
        entries: &[
            wgpu::BindGroupEntry {
                binding: h.binding,
                resource: header.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: b.uniforms.binding,
                resource: uniforms.as_entire_binding(),
            },
        ],
    });
    let entries: Vec<wgpu::BindGroupEntry> = b
        .attributes
        .iter()
        .zip(&attrs)
        .map(|(a, buf)| wgpu::BindGroupEntry {
            binding: a.binding,
            resource: buf.as_entire_binding(),
        })
        .collect();
    let bind1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("kernel g1"),
        layout: &g1,
        entries: &entries,
    });

    let groups = (count as u32).div_ceil(b.workgroup_size).max(1);
    let x = groups.min(65_535);
    let y = groups.div_ceil(x);
    let mut ubytes = vec![0u8; b.uniform_size as usize];
    for g in frames {
        c.write_uniforms(g, values, &mut ubytes);
        queue.write_buffer(&uniforms, 0, &ubytes);
        let mut enc = device.create_command_encoder(&Default::default());
        // Snapshots hold this frame's input.
        for (i, a) in b.attributes.iter().enumerate() {
            if a.snapshot
                && let Some(j) = b
                    .attributes
                    .iter()
                    .position(|o| o.name == a.name && !o.snapshot)
            {
                enc.copy_buffer_to_buffer(&attrs[j], 0, &attrs[i], 0, attrs[j].size());
            }
        }
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("kernel"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind0, &[]);
            pass.set_bind_group(1, &bind1, &[]);
            pass.dispatch_workgroups(x, y, 1);
        }
        queue.submit([enc.finish()]);
    }

    // Read back every attribute the kernel binds (not the snapshots).
    let mut out = Geometry {
        count,
        attributes: Vec::new(),
    };
    for (a, buf) in b.attributes.iter().zip(&attrs) {
        if a.snapshot {
            continue;
        }
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: buf.size(),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_buffer_to_buffer(buf, 0, &staging, 0, buf.size());
        queue.submit([enc.finish()]);
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        rx.recv().expect("map callback").expect("map");
        let data = slice.get_mapped_range().expect("mapped");
        let words: Vec<u32> = data
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .take(count * (a.stride as usize / 4))
            .collect();
        drop(data);
        staging.unmap();
        out.attributes
            .push((a.name.clone(), Attribute { ty: a.ty, words }));
    }
    out
}

/// A deterministic cloud of points with `P`, `v`, `age` and `id`.
fn cloud(n: usize) -> Geometry {
    let p: Vec<[f32; 3]> = (0..n)
        .map(|i| {
            let t = i as f32;
            [
                (t * 0.37).sin() * 2.0,
                (t * 0.11).cos() * 1.5,
                (t * 0.07) % 3.0 - 1.5,
            ]
        })
        .collect();
    let v: Vec<[f32; 3]> = (0..n)
        .map(|i| [0.1 * (i % 5) as f32, -0.05, 0.02 * (i % 3) as f32])
        .collect();
    Geometry {
        count: n,
        attributes: vec![
            ("P".into(), Attribute::from_vec3(&p)),
            ("v".into(), Attribute::from_vec3(&v)),
            (
                "age".into(),
                Attribute::from_f32(&(0..n).map(|i| i as f32 * 0.01).collect::<Vec<_>>()),
            ),
            (
                "id".into(),
                Attribute::from_u32(&(0..n as u32).map(|i| i * 7 + 3).collect::<Vec<_>>()),
            ),
        ],
    }
}

/// Compares every attribute in `gpu` with `cpu`: integers exactly, floats within
/// `abs + rel * |x|`. Returns the largest error seen.
fn compare(name: &str, cpu: &Geometry, gpu: &Geometry, abs: f32, rel: f32) -> f32 {
    let mut worst = 0.0f32;
    for (attr, g) in &gpu.attributes {
        let c = cpu
            .get(attr)
            .unwrap_or_else(|| panic!("{name}: {attr} missing on the CPU"));
        for i in 0..gpu.count {
            let (cw, gw) = (c.element(i), g.element(i));
            for k in 0..usize::from(g.ty.components) {
                if g.ty.scalar != Scalar::F32 {
                    assert_eq!(
                        cw[k], gw[k],
                        "{name}: @{attr}[{i}].{k} cpu {} gpu {}",
                        cw[k], gw[k]
                    );
                    continue;
                }
                let (a, b) = (f32::from_bits(cw[k]), f32::from_bits(gw[k]));
                let err = (a - b).abs();
                assert!(
                    err <= abs + rel * a.abs() || (a.is_nan() && b.is_nan()),
                    "{name}: @{attr}[{i}].{k} cpu {a} gpu {b}"
                );
                worst = worst.max(err);
            }
        }
    }
    worst
}

fn frames(n: usize) -> Vec<FrameGlobals> {
    (0..n)
        .map(|f| FrameGlobals {
            time: f as f32 / 60.0,
            time_inc: 1.0 / 60.0,
            frame: f as f32,
            width: 0,
            height: 0,
        })
        .collect()
}

#[test]
fn noise_matches_the_rust_library_on_a_grid() {
    let _lock = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let Some((device, queue)) = device() else {
        return;
    };
    // A 24^3 grid spanning several lattice cells, offset off the integers.
    let n = 24usize;
    let pts: Vec<[f32; 3]> = (0..n * n * n)
        .map(|i| {
            let (x, y, z) = (i % n, (i / n) % n, i / (n * n));
            [
                x as f32 * 0.31 - 3.7,
                y as f32 * 0.27 - 2.9,
                z as f32 * 0.23 + 0.05,
            ]
        })
        .collect();
    let geo = Geometry {
        count: pts.len(),
        attributes: vec![("P".into(), Attribute::from_vec3(&pts))],
    };
    let a = compile(
        "f@p3 = perlin(@P); f@p2 = perlin(@P.xy); f@s3 = simplex(@P); f@s2 = simplex(@P.xy); f@w3 = worley(@P);",
        &CompileOptions::new(Target::Points),
    )
    .expect("compiles");
    let b = compile(
        "f@w2 = worley(@P.xy); v@c = curlnoise(@P); f@r1 = rand(@ptnum); f@r3 = rand(@P);",
        &CompileOptions::new(Target::Points),
    )
    .expect("compiles");
    let ga = run_gpu(&device, &queue, &a, &geo, &[], &frames(1));
    let gb = run_gpu(&device, &queue, &b, &geo, &[], &frames(1));
    let get = |g: &Geometry, name: &str| g.get(name).cloned().expect(name);
    let mut worst = [0.0f32; 9];
    for (i, p) in pts.iter().enumerate() {
        let p2 = [p[0], p[1]];
        let checks = [
            (get(&ga, "p3").f32(i), noise::perlin3(*p)),
            (get(&ga, "p2").f32(i), noise::perlin2(p2)),
            (get(&ga, "s3").f32(i), noise::simplex3(*p)),
            (get(&ga, "s2").f32(i), noise::simplex2(p2)),
            (get(&ga, "w3").f32(i), noise::worley3(*p)),
            (get(&gb, "w2").f32(i), noise::worley2(p2)),
        ];
        for (k, (gpu, cpu)) in checks.into_iter().enumerate() {
            let e = (gpu - cpu).abs();
            assert!(e < 1e-4, "noise {k} at {p:?}: gpu {gpu} cpu {cpu}");
            worst[k] = worst[k].max(e);
        }
        let c = get(&gb, "c").vec3(i);
        let r = noise::curlnoise(*p);
        for k in 0..3 {
            let e = (c[k] - r[k]).abs();
            assert!(
                e < 1e-3 * (1.0 + r[k].abs()),
                "curl at {p:?}: gpu {c:?} cpu {r:?}"
            );
            worst[6] = worst[6].max(e);
        }
        // Integer hashing is exact.
        assert_eq!(
            get(&gb, "r1").f32(i),
            noise::rand_i(i as i32),
            "rand(int) at {i}"
        );
        assert_eq!(
            get(&gb, "r3").f32(i),
            noise::rand_v3(*p),
            "rand(vec3) at {p:?}"
        );
    }
    eprintln!(
        "largest |gpu - cpu|: perlin3 {:e} perlin2 {:e} simplex3 {:e} simplex2 {:e} worley3 {:e} worley2 {:e} curl {:e}",
        worst[0], worst[1], worst[2], worst[3], worst[4], worst[5], worst[6]
    );
}

#[test]
fn kernels_match_the_interpreter() {
    let _lock = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let Some((device, queue)) = device() else {
        return;
    };
    let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let cases = [
        ("curl_particles", 60, 1000),
        ("control_flow", 1, 1000),
        ("matrices", 1, 500),
        ("noise", 1, 500),
        ("neighbours", 3, 300),
    ];
    for (name, steps, n) in cases {
        let src = std::fs::read_to_string(golden.join(format!("{name}.points.atk"))).expect("read");
        let c = compile(&src, &CompileOptions::new(Target::Points)).expect("compiles");
        let geo = cloud(n);
        let values: Vec<[f32; 4]> = c.params.iter().map(|p| p.default).collect();
        let gpu = run_gpu(&device, &queue, &c, &geo, &values, &frames(steps));
        let mut cpu = geo.clone();
        for g in frames(steps) {
            run_points(&c, &mut cpu, &values, &g).expect("interpreter runs");
        }
        let worst = compare(name, &cpu, &gpu, 1e-4, 1e-4);
        eprintln!("{name}: {n} points, {steps} steps, largest float error {worst:e}");
    }
}

/// AC2 groundwork: the ten-line curl-noise kernel is deterministic on the GPU: two runs
/// of 120 steps give byte-identical attribute buffers.
#[test]
fn curl_kernel_is_deterministic_on_the_gpu() {
    let _lock = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let Some((device, queue)) = device() else {
        return;
    };
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden/curl_particles.points.atk"),
    )
    .expect("read");
    let c = compile(&src, &CompileOptions::new(Target::Points)).expect("compiles");
    let values: Vec<[f32; 4]> = c.params.iter().map(|p| p.default).collect();
    let geo = cloud(5000);
    let a = run_gpu(&device, &queue, &c, &geo, &values, &frames(120));
    let b = run_gpu(&device, &queue, &c, &geo, &values, &frames(120));
    assert_eq!(a, b);
    assert_ne!(a.get("P"), geo.get("P"), "the particles moved");
}
