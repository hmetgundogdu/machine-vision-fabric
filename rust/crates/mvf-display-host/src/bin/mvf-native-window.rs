//! `mvf-native-window`: the *native UI* profile proof of concept (#30).
//!
//! Stands in for an existing application that owns its windows with its own toolkit (here winit +
//! softbuffer; Qt, OpenCV `imshow` or WPF would do the same). It speaks protocol v1 as an analyzer
//! on a background thread and draws the latest camera frame in its own window. MVF only decides
//! whether it may run (an interactive session) and manages its lifecycle; it does not mediate the
//! window or its input. Closing the window does not stop the node: it keeps processing headless.

use std::io::{BufRead, Write};
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use mvf_display::{FrameInfo, PixelFormat};
use mvf_display_host::present::blit;
use serde_json::{Value, json};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy, OwnedDisplayHandle};
use winit::window::{Window, WindowId};

enum UserEvent {
    Frame(Vec<u8>, FrameInfo),
    Exit,
}

#[derive(Default)]
struct Shared {
    window_open: AtomicBool,
    drawn: AtomicU64,
}

fn send(v: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

/// The protocol side: reads frames from the arena and hands copies to the UI thread.
fn serve(proxy: EventLoopProxy<UserEvent>, shared: Arc<Shared>) {
    let arena = std::env::var_os("MVF_ARENA_PATH").and_then(|p| {
        std::fs::File::open(p)
            .ok()
            // SAFETY: read-only mapping of the engine's arena; descriptors are validated.
            .and_then(|f| unsafe { memmap2::Mmap::map(&f) }.ok())
    });
    send(
        &json!({"type": "hello", "protocol": 1, "moduleId": "rs.native-window", "capability": "analyzer"}),
    );
    let mut frames = 0u64;
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
        frames += 1;
        let frame = msg["frame"]["shm"]["offset"]
            .as_u64()
            .zip(arena.as_ref())
            .and_then(|(off, map)| {
                let off = off as usize;
                let d = mvf_memory::Descriptor::read_header(
                    map.get(off..off + mvf_memory::HEADER_SIZE)?,
                )
                .ok()?;
                let len = d.byte_len()? as usize;
                let data =
                    map.get(off + mvf_memory::HEADER_SIZE..off + mvf_memory::HEADER_SIZE + len)?;
                let (h, w, c) = match d.shape.as_slice() {
                    [h, w, c] => (*h as u32, *w as u32, *c as u32),
                    [h, w] => (*h as u32, *w as u32, 1),
                    _ => return None,
                };
                let format = match c {
                    1 => PixelFormat::Gray8,
                    3 => PixelFormat::Rgb8,
                    4 => PixelFormat::Rgba8,
                    _ => return None,
                };
                let info = FrameInfo {
                    seq: frames,
                    width: w,
                    height: h,
                    stride: w * c,
                    format,
                    timestamp_us: 0,
                };
                Some((data.to_vec(), info))
            });
        if let Some((data, info)) = frame
            && shared.window_open.load(Ordering::Acquire)
        {
            let _ = proxy.send_event(UserEvent::Frame(data, info));
        }
        send(&json!({
            "type": "result", "id": msg["id"], "frame": null,
            "value": {
                "windowOpen": shared.window_open.load(Ordering::Acquire),
                "framesDrawn": shared.drawn.load(Ordering::Acquire),
                "frames": frames,
            }
        }));
    }
    let _ = proxy.send_event(UserEvent::Exit);
}

/// A window's drawing target.
type Canvas = softbuffer::Surface<OwnedDisplayHandle, Rc<Window>>;

struct App {
    context: softbuffer::Context<OwnedDisplayHandle>,
    window: Option<(Rc<Window>, Canvas)>,
    frame: Option<(Vec<u8>, FrameInfo)>,
    shared: Arc<Shared>,
    created: bool,
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.created {
            return;
        }
        self.created = true;
        let title = std::env::var("MVF_NATIVE_TITLE")
            .unwrap_or_else(|_| "Native window (own toolkit)".into());
        let attrs = Window::default_attributes()
            .with_title(title)
            .with_inner_size(LogicalSize::new(640, 480));
        match el.create_window(attrs) {
            Ok(w) => {
                let w = Rc::new(w);
                match softbuffer::Surface::new(&self.context, w.clone()) {
                    Ok(s) => {
                        eprintln!(
                            "native-window: opened {:?} scale {}",
                            w.inner_size(),
                            w.scale_factor()
                        );
                        self.window = Some((w, s));
                        self.shared.window_open.store(true, Ordering::Release);
                    }
                    Err(e) => eprintln!("native-window: cannot draw: {e}"),
                }
            }
            Err(e) => eprintln!("native-window: cannot open a window: {e}"),
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Frame(data, info) => {
                self.frame = Some((data, info));
                if let Some((w, _)) = &self.window {
                    w.request_redraw();
                }
            }
            UserEvent::Exit => el.exit(),
        }
    }

    fn window_event(&mut self, _el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                // The application decides: this one keeps processing without a window.
                self.window = None;
                self.shared.window_open.store(false, Ordering::Release);
                eprintln!("native-window: window closed by the user; continuing headless");
            }
            WindowEvent::Resized(_) => {
                if let Some((w, _)) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                let Some((w, surface)) = self.window.as_mut() else {
                    return;
                };
                let size = w.inner_size();
                let (Some(nw), Some(nh)) =
                    (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                else {
                    return;
                };
                if surface.resize(nw, nh).is_err() {
                    return;
                }
                let Ok(mut buffer) = surface.buffer_mut() else {
                    return;
                };
                match &self.frame {
                    Some((data, info)) => {
                        blit(data, info, &mut buffer, size.width, size.height);
                    }
                    None => buffer.fill(mvf_display_host::present::BACKGROUND),
                }
                if buffer.present().is_ok() && self.frame.is_some() {
                    self.shared.drawn.fetch_add(1, Ordering::AcqRel);
                }
            }
            _ => {}
        }
    }
}

fn main() {
    let shared = Arc::new(Shared::default());
    let event_loop = match EventLoop::<UserEvent>::with_user_event().build() {
        Ok(l) => l,
        Err(e) => {
            // No window system: still a valid node, just without its window.
            eprintln!("native-window: no window system ({e}); running headless");
            serve_headless(shared);
            return;
        }
    };
    let proxy = event_loop.create_proxy();
    let worker_shared = shared.clone();
    let protocol = std::thread::spawn(move || serve(proxy, worker_shared));
    let context = match softbuffer::Context::new(event_loop.owned_display_handle()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("native-window: cannot draw ({e}); running headless");
            let _ = protocol.join();
            return;
        }
    };
    let mut app = App {
        context,
        window: None,
        frame: None,
        shared,
        created: false,
    };
    let _ = event_loop.run_app(&mut app);
    let _ = protocol.join();
}

fn serve_headless(shared: Arc<Shared>) {
    send(
        &json!({"type": "hello", "protocol": 1, "moduleId": "rs.native-window", "capability": "analyzer"}),
    );
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match msg["type"].as_str() {
            Some("execute") => send(&json!({
                "type": "result", "id": msg["id"], "frame": null,
                "value": {"windowOpen": false, "framesDrawn": shared.drawn.load(Ordering::Acquire)}
            })),
            Some("shutdown") => break,
            _ => {}
        }
    }
}
