//! `mvf-cube`: an interactive 3D renderer node (#30, Rust SDK side of the Display Fabric).
//!
//! A protocol-v1 analyzer worker: each camera frame becomes the texture of a cube's front face,
//! rendered on the CPU straight into a managed surface (no copy, no window library). Drag with the
//! left button to orbit, wheel to zoom, `r` to reset, arrows to turn. It renders at the size the
//! host asks for (resize, DPI) and keeps processing headless when its window is closed. The JSON
//! output reports the view, so interaction becomes typed data in the graph.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use mvf_display::{InputKind, PixelFormat, Presented, SurfaceWriter};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq)]
struct View {
    yaw: f32,
    pitch: f32,
    zoom: f32,
}

const RESET: View = View {
    yaw: 0.0,
    pitch: 0.0,
    zoom: 1.0,
};

struct Texture<'a> {
    data: &'a [u8],
    width: usize,
    height: usize,
    channels: usize,
}

impl Texture<'_> {
    /// `(r, g, b)` at normalized `(u, v)`.
    fn sample(&self, u: f32, v: f32) -> (f32, f32, f32) {
        let x = ((u.clamp(0.0, 0.9999) * self.width as f32) as usize).min(self.width - 1);
        let y = ((v.clamp(0.0, 0.9999) * self.height as f32) as usize).min(self.height - 1);
        let o = (y * self.width + x) * self.channels;
        let at = |i: usize| f32::from(self.data.get(o + i).copied().unwrap_or(0));
        if self.channels >= 3 {
            (at(0), at(1), at(2))
        } else {
            (at(0), at(0), at(0))
        }
    }
}

type V3 = [f32; 3];

fn rotate(p: V3, v: &View) -> V3 {
    let (sy, cy) = v.yaw.sin_cos();
    let (sp, cp) = v.pitch.sin_cos();
    let x = p[0] * cy + p[2] * sy;
    let z = -p[0] * sy + p[2] * cy;
    let y = p[1] * cp - z * sp;
    [x, y, p[1] * sp + z * cp]
}

fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Faces: four corners (counter-clockwise from outside), normal, flat colour (`None` = textured).
/// Four corners, the normal, and a flat colour (`None` = textured).
type Face = ([V3; 4], V3, Option<(f32, f32, f32)>);

const FACES: [Face; 6] = [
    (
        [[-1., -1., 1.], [1., -1., 1.], [1., 1., 1.], [-1., 1., 1.]],
        [0., 0., 1.],
        None,
    ),
    (
        [
            [1., -1., -1.],
            [-1., -1., -1.],
            [-1., 1., -1.],
            [1., 1., -1.],
        ],
        [0., 0., -1.],
        Some((70., 110., 200.)),
    ),
    (
        [[1., -1., 1.], [1., -1., -1.], [1., 1., -1.], [1., 1., 1.]],
        [1., 0., 0.],
        Some((230., 140., 50.)),
    ),
    (
        [
            [-1., -1., -1.],
            [-1., -1., 1.],
            [-1., 1., 1.],
            [-1., 1., -1.],
        ],
        [-1., 0., 0.],
        Some((40., 170., 160.)),
    ),
    (
        [[-1., 1., 1.], [1., 1., 1.], [1., 1., -1.], [-1., 1., -1.]],
        [0., 1., 0.],
        Some((200., 200., 200.)),
    ),
    (
        [
            [-1., -1., -1.],
            [1., -1., -1.],
            [1., -1., 1.],
            [-1., -1., 1.],
        ],
        [0., -1., 0.],
        Some((90., 90., 90.)),
    ),
];
const UV: [(f32, f32); 4] = [(0., 1.), (1., 1.), (1., 0.), (0., 0.)];
const DISTANCE: f32 = 4.0;

/// Renders the cube into a BGRA buffer.
fn render(
    out: &mut [u8],
    stride: usize,
    w: usize,
    h: usize,
    view: &View,
    tex: Option<&Texture<'_>>,
) {
    for y in 0..h {
        let shade = 24 + (y * 24 / h.max(1)) as u8;
        for x in 0..w {
            let o = y * stride + x * 4;
            out[o..o + 4].copy_from_slice(&[shade + 10, shade + 4, shade, 255]);
        }
    }
    let f = w.min(h) as f32 * 1.1 * view.zoom;
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let light = {
        let l = [-0.4f32, 0.6, 1.0];
        let n = dot(l, l).sqrt();
        [l[0] / n, l[1] / n, l[2] / n]
    };
    for (corners, normal, colour) in FACES.iter() {
        let n = rotate(*normal, view);
        let r: Vec<V3> = corners.iter().map(|c| rotate(*c, view)).collect();
        let centre = [
            (r[0][0] + r[2][0]) / 2.0,
            (r[0][1] + r[2][1]) / 2.0,
            (r[0][2] + r[2][2]) / 2.0,
        ];
        let to_camera = [-centre[0], -centre[1], DISTANCE - centre[2]];
        if dot(n, to_camera) <= 0.0 {
            continue; // back face: a convex solid needs no depth buffer
        }
        let intensity = 0.3 + 0.7 * dot(n, light).max(0.0);
        // Screen position, 1/depth and uv/depth per corner (perspective-correct texturing).
        let p: Vec<(f32, f32, f32, f32, f32)> = r
            .iter()
            .zip(UV.iter())
            .map(|(v, (u, t))| {
                let d = DISTANCE - v[2];
                let inv = 1.0 / d;
                (
                    cx + f * v[0] * inv,
                    cy - f * v[1] * inv,
                    inv,
                    u * inv,
                    t * inv,
                )
            })
            .collect();
        for tri in [[0, 1, 2], [0, 2, 3]] {
            let [a, b, c] = tri.map(|i| p[i]);
            let area = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
            if area.abs() < 1e-6 {
                continue;
            }
            let min_x = a.0.min(b.0).min(c.0).floor().max(0.0) as usize;
            let max_x = (a.0.max(b.0).max(c.0).ceil() as usize).min(w);
            let min_y = a.1.min(b.1).min(c.1).floor().max(0.0) as usize;
            let max_y = (a.1.max(b.1).max(c.1).ceil() as usize).min(h);
            for y in min_y..max_y {
                for x in min_x..max_x {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    let w0 = ((b.0 - px) * (c.1 - py) - (b.1 - py) * (c.0 - px)) / area;
                    let w1 = ((c.0 - px) * (a.1 - py) - (c.1 - py) * (a.0 - px)) / area;
                    let w2 = 1.0 - w0 - w1;
                    if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                        continue;
                    }
                    let (rr, gg, bb) = match (colour, tex) {
                        (Some(c), _) => *c,
                        (None, Some(t)) => {
                            let inv = w0 * a.2 + w1 * b.2 + w2 * c.2;
                            let u = (w0 * a.3 + w1 * b.3 + w2 * c.3) / inv;
                            let v = (w0 * a.4 + w1 * b.4 + w2 * c.4) / inv;
                            t.sample(u, v)
                        }
                        (None, None) => (128.0, 128.0, 128.0),
                    };
                    let o = y * stride + x * 4;
                    out[o] = (bb * intensity) as u8;
                    out[o + 1] = (gg * intensity) as u8;
                    out[o + 2] = (rr * intensity) as u8;
                    out[o + 3] = 255;
                }
            }
        }
    }
}

fn send(v: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

fn log(message: &str) {
    send(&json!({"type": "log", "level": "info", "message": message}));
}

fn main() {
    let arena = std::env::var_os("MVF_ARENA_PATH").and_then(|p| {
        std::fs::File::open(p)
            .ok()
            // SAFETY: read-only mapping of the engine's arena; descriptors are validated.
            .and_then(|f| unsafe { memmap2::Mmap::map(&f) }.ok())
    });
    let mut surface = std::env::var_os("MVF_DISPLAY_SURFACES")
        .and_then(|s| std::env::split_paths(&s).next())
        .map(|p: PathBuf| SurfaceWriter::open(&p));
    send(
        &json!({"type": "hello", "protocol": 1, "moduleId": "rs.cube-inspector", "capability": "analyzer"}),
    );
    match &surface {
        Some(Ok(s)) => log(&format!("cube: rendering into {}", s.path().display())),
        Some(Err(e)) => log(&format!("cube: headless, surface unusable: {e}")),
        None => log("cube: headless (no display surface granted)"),
    }
    let mut view = RESET;
    let mut drag: Option<(f32, f32)> = None;
    let (mut events_total, mut lost_total) = (0u64, 0u64);
    let mut closed = false;
    let mut events = Vec::new();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match msg["type"].as_str() {
            Some("execute") => {}
            Some("shutdown") => break,
            _ => continue,
        }
        let id = msg["id"].clone();
        let texture = msg["frame"]["shm"]["offset"]
            .as_u64()
            .zip(arena.as_ref())
            .and_then(|(off, map)| {
                let off = off as usize;
                let header = map.get(off..off + mvf_memory::HEADER_SIZE)?;
                let d = mvf_memory::Descriptor::read_header(header).ok()?;
                let len = d.byte_len()? as usize;
                let data =
                    map.get(off + mvf_memory::HEADER_SIZE..off + mvf_memory::HEADER_SIZE + len)?;
                let (h, w, c) = match d.shape.as_slice() {
                    [h, w, c] => (*h as usize, *w as usize, *c as usize),
                    [h, w] => (*h as usize, *w as usize, 1),
                    _ => return None,
                };
                (w > 0 && h > 0 && w * h * c == len).then_some(Texture {
                    data,
                    width: w,
                    height: h,
                    channels: c,
                })
            });

        let mut presented = false;
        let mut standby = false;
        let mut size = [0u32, 0];
        let mut render_ms = 0.0;
        let mut attached = false;
        if let Some(Ok(s)) = surface.as_mut() {
            events.clear();
            lost_total += s.poll_input(&mut events);
            events_total += events.len() as u64;
            for e in &events {
                match e.kind {
                    InputKind::PointerDown if e.code == 0 => drag = Some((e.x, e.y)),
                    InputKind::PointerUp => drag = None,
                    InputKind::PointerMove => {
                        if let Some((x0, y0)) = drag {
                            view.yaw += (e.x - x0) * 0.01;
                            view.pitch = (view.pitch + (e.y - y0) * 0.01).clamp(-1.5, 1.5);
                            drag = Some((e.x, e.y));
                        }
                    }
                    InputKind::Wheel => view.zoom = (view.zoom * 1.1f32.powf(e.dy)).clamp(0.3, 4.0),
                    InputKind::KeyDown if e.text == "r" => view = RESET,
                    InputKind::KeyDown if e.text == "ArrowLeft" => view.yaw -= 0.1,
                    InputKind::KeyDown if e.text == "ArrowRight" => view.yaw += 0.1,
                    InputKind::CloseRequested => closed = true,
                    _ => {}
                }
            }
            let host = s.host();
            attached = host.is_some_and(|h| h.attached(Duration::from_secs(2)));
            // The host's flag is authoritative: a new host after a close (or a restart) shows the
            // surface again.
            if let Some(h) = host.filter(|_| attached) {
                closed = h.closed();
            }
            if !closed {
                let g = s.geometry();
                let (w, h) = host
                    .filter(|h| h.requested_width > 0 && h.requested_height > 0)
                    .map(|h| (h.requested_width, h.requested_height))
                    .unwrap_or((640, 480));
                let (w, h) = (w.min(g.max_width), h.min(g.max_height));
                let t0 = Instant::now();
                let v = view;
                let tex = texture.as_ref();
                match s.present_with(w, h, PixelFormat::Bgra8, |buf, stride| {
                    render(buf, stride, w as usize, h as usize, &v, tex)
                }) {
                    Ok(Presented::Shown { .. }) => presented = true,
                    Ok(Presented::Standby) => standby = true,
                    Err(e) => log(&format!("cube: {e}")),
                }
                render_ms = t0.elapsed().as_secs_f64() * 1000.0;
                size = [w, h];
            }
        }
        send(&json!({
            "type": "result",
            "id": id,
            "frame": null,
            "value": {
                "yaw": view.yaw, "pitch": view.pitch, "zoom": view.zoom,
                "size": size, "presented": presented, "standby": standby,
                "closed": closed, "hostAttached": attached,
                "events": events_total, "lostEvents": lost_total,
                "textured": texture.is_some(), "renderMs": render_ms,
            }
        }));
    }
}
