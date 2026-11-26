//! `mvf-display-host`: presents Display Fabric surfaces in the user's session (#30).
//!
//! Runs as the logged-in user, separate from the engine (which may be a service). Discovers the
//! surfaces of every live engine under the rendezvous directory, opens one window per surface,
//! shows each node's latest frame and routes back only the input the node's contract grants.
//! Closing or crashing this process never stops a pipeline.
//!
//! ```text
//! mvf-display-host [--dir PATH] [--headless] [--max-windows N] [--exit-after-ms N] [--stats FILE]
//! mvf-display-host --probe        # print this session's kind and exit
//! ```

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use mvf_display::surface::modifiers as mods;
use mvf_display::{InputEvent, InputKind};
use mvf_display_host::HostCore;
use mvf_display_host::present::{BACKGROUND, Letterbox, blit, window_to_frame};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, OwnedDisplayHandle};
use winit::keyboard::{Key, ModifiersState};
use winit::window::{Window, WindowId};

#[derive(Debug)]
struct Args {
    dir: PathBuf,
    headless: bool,
    max_windows: usize,
    exit_after: Option<Duration>,
    stats: Option<PathBuf>,
    probe: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        dir: mvf_display::DisplayRegistry::from_env()
            .root()
            .to_path_buf(),
        headless: false,
        max_windows: 16,
        exit_after: None,
        stats: None,
        probe: false,
    };
    let mut it = std::env::args_os().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.to_string_lossy().as_ref() {
            "--dir" => a.dir = PathBuf::from(value("--dir")?),
            "--headless" => a.headless = true,
            "--probe" => a.probe = true,
            "--max-windows" => {
                a.max_windows = value("--max-windows")?
                    .to_string_lossy()
                    .parse()
                    .map_err(|e| format!("--max-windows: {e}"))?
            }
            "--exit-after-ms" => {
                a.exit_after = Some(Duration::from_millis(
                    value("--exit-after-ms")?
                        .to_string_lossy()
                        .parse()
                        .map_err(|e| format!("--exit-after-ms: {e}"))?,
                ))
            }
            "--stats" => a.stats = Some(PathBuf::from(value("--stats")?)),
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(a)
}

fn flush_log(core: &mut HostCore) {
    for line in core.log.drain(..) {
        eprintln!("mvf-display-host: {line}");
    }
}

fn write_stats(path: Option<&Path>, mut stats: serde_json::Value, extra: serde_json::Value) {
    let Some(path) = path else {
        return;
    };
    if let (Some(s), Some(e)) = (stats.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            s.insert(k.clone(), v.clone());
        }
    }
    let _ = std::fs::write(path, serde_json::to_vec_pretty(&stats).unwrap_or_default());
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("mvf-display-host: {e}");
            std::process::exit(2);
        }
    };
    let session = mvf_display::session::probe();
    if args.probe {
        println!("{}", serde_json::to_string(&session).unwrap_or_default());
        return;
    }
    eprintln!(
        "mvf-display-host: session {} | surfaces under {}",
        session.detail,
        args.dir.display()
    );
    if args.headless {
        run_headless(&args);
        return;
    }
    let event_loop = match EventLoop::new() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("mvf-display-host: no window system ({e}); use --headless");
            std::process::exit(3);
        }
    };
    let context = match softbuffer::Context::new(event_loop.owned_display_handle()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mvf-display-host: cannot draw ({e})");
            std::process::exit(3);
        }
    };
    let mut app = App {
        core: HostCore::new(&args.dir, args.max_windows),
        context,
        windows: HashMap::new(),
        by_path: HashMap::new(),
        started: Instant::now(),
        exit_after: args.exit_after,
        opened: Vec::new(),
        presents: 0,
        errors: Vec::new(),
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("mvf-display-host: event loop: {e}");
    }
    write_stats(
        args.stats.as_deref(),
        app.core.stats(),
        serde_json::json!({
            "mode": "windowed",
            "session": session.detail,
            "windowsOpened": app.opened,
            "presents": app.presents,
            "errors": app.errors,
        }),
    );
}

fn run_headless(args: &Args) {
    let mut core = HostCore::new(&args.dir, args.max_windows);
    let started = Instant::now();
    loop {
        core.scan(false);
        core.poll();
        core.heartbeat(false);
        flush_log(&mut core);
        if args.exit_after.is_some_and(|d| started.elapsed() >= d) {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    write_stats(
        args.stats.as_deref(),
        core.stats(),
        serde_json::json!({"mode": "headless"}),
    );
}

struct Win {
    window: Rc<Window>,
    surface: softbuffer::Surface<OwnedDisplayHandle, Rc<Window>>,
    path: PathBuf,
    letterbox: Letterbox,
    cursor: Option<(f64, f64)>,
    modifiers: u16,
}

struct App {
    core: HostCore,
    context: softbuffer::Context<OwnedDisplayHandle>,
    windows: HashMap<WindowId, Win>,
    by_path: HashMap<PathBuf, WindowId>,
    started: Instant,
    exit_after: Option<Duration>,
    opened: Vec<serde_json::Value>,
    presents: u64,
    errors: Vec<String>,
}

impl App {
    fn open(&mut self, el: &ActiveEventLoop, path: &Path) {
        let Some(d) = self.core.get(path).map(|s| s.descriptor.clone()) else {
            return;
        };
        let attrs = Window::default_attributes()
            .with_title(format!("{} - MVF", d.title))
            .with_inner_size(LogicalSize::new(d.width.max(64), d.height.max(64)));
        let window = match el.create_window(attrs) {
            Ok(w) => Rc::new(w),
            Err(e) => {
                self.errors.push(format!("create window: {e}"));
                return;
            }
        };
        let surface = match softbuffer::Surface::new(&self.context, window.clone()) {
            Ok(s) => s,
            Err(e) => {
                self.errors.push(format!("create surface: {e}"));
                return;
            }
        };
        let size = window.inner_size();
        let scale = window.scale_factor();
        self.core.status(path, |s| {
            s.requested_width = size.width;
            s.requested_height = size.height;
            s.scale_milli = (scale * 1000.0) as u32;
        });
        eprintln!(
            "mvf-display-host: window opened \"{}\" {}x{} px, scale {scale}",
            d.title, size.width, size.height
        );
        self.opened.push(serde_json::json!({
            "title": d.title, "size": [size.width, size.height], "scale": scale
        }));
        let id = window.id();
        window.request_redraw();
        self.by_path.insert(path.to_path_buf(), id);
        self.windows.insert(
            id,
            Win {
                window,
                surface,
                path: path.to_path_buf(),
                letterbox: Letterbox::default(),
                cursor: None,
                modifiers: 0,
            },
        );
    }

    fn redraw(&mut self, id: WindowId) {
        let Some(win) = self.windows.get_mut(&id) else {
            return;
        };
        let size = win.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return;
        };
        if let Err(e) = win.surface.resize(w, h) {
            self.errors.push(format!("resize: {e}"));
            return;
        }
        let mut buffer = match win.surface.buffer_mut() {
            Ok(b) => b,
            Err(e) => {
                self.errors.push(format!("buffer: {e}"));
                return;
            }
        };
        match self
            .core
            .get(&win.path)
            .and_then(|s| s.info.map(|i| (s, i)))
        {
            Some((shown, info)) => {
                win.letterbox = blit(&shown.frame, &info, &mut buffer, size.width, size.height);
            }
            None => buffer.fill(BACKGROUND),
        }
        if let Err(e) = buffer.present() {
            self.errors.push(format!("present: {e}"));
            return;
        }
        self.presents += 1;
    }

    fn pointer(&mut self, id: WindowId, kind: InputKind, button: u32) {
        let Some(win) = self.windows.get(&id) else {
            return;
        };
        let Some((cx, cy)) = win.cursor else {
            return;
        };
        let Some(info) = self.core.get(&win.path).and_then(|s| s.info) else {
            return;
        };
        let Some((x, y)) = window_to_frame(&win.letterbox, info.width, info.height, cx, cy) else {
            return; // outside the frame (letterbox bars)
        };
        let mut e = InputEvent::pointer(kind, x, y);
        e.code = button;
        e.modifiers = win.modifiers;
        let path = win.path.clone();
        self.core.route(&path, &e);
    }
}

fn key_name(key: &Key) -> String {
    match key {
        Key::Character(c) => c.to_string(),
        Key::Named(n) => format!("{n:?}"),
        _ => "Unidentified".into(),
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        el.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + Duration::from_millis(4),
        ));
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let delta = self.core.scan(false);
        for path in &delta.removed {
            if let Some(id) = self.by_path.remove(path) {
                self.windows.remove(&id);
            }
        }
        for path in &delta.added {
            self.open(el, path);
        }
        for path in self.core.poll() {
            if let Some(win) = self.by_path.get(&path).and_then(|id| self.windows.get(id)) {
                win.window.request_redraw();
            }
        }
        self.core.heartbeat(false);
        flush_log(&mut self.core);
        if self.exit_after.is_some_and(|d| self.started.elapsed() >= d) {
            el.exit();
            return;
        }
        el.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + Duration::from_millis(4),
        ));
    }

    fn window_event(&mut self, _el: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(path) = self.windows.get(&id).map(|w| w.path.clone()) else {
            return;
        };
        match event {
            WindowEvent::RedrawRequested => self.redraw(id),
            WindowEvent::Resized(size) => {
                self.core.status(&path, |s| {
                    s.requested_width = size.width;
                    s.requested_height = size.height;
                });
                self.core.route(
                    &path,
                    &InputEvent::pointer(InputKind::Resize, size.width as f32, size.height as f32),
                );
                if let Some(w) = self.windows.get(&id) {
                    w.window.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.core
                    .status(&path, |s| s.scale_milli = (scale_factor * 1000.0) as u32);
            }
            WindowEvent::CloseRequested => {
                self.core.close(&path);
                self.windows.remove(&id);
                flush_log(&mut self.core);
            }
            WindowEvent::Focused(focused) => self.core.focus(&path, focused),
            WindowEvent::Occluded(occluded) => self.core.visible(&path, !occluded),
            WindowEvent::ModifiersChanged(m) => {
                let s = m.state();
                let bits = [
                    (ModifiersState::SHIFT, mods::SHIFT),
                    (ModifiersState::CONTROL, mods::CTRL),
                    (ModifiersState::ALT, mods::ALT),
                    (ModifiersState::SUPER, mods::SUPER),
                ]
                .iter()
                .filter(|(f, _)| s.contains(*f))
                .fold(0, |acc, (_, b)| acc | b);
                if let Some(w) = self.windows.get_mut(&id) {
                    w.modifiers = bits;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(w) = self.windows.get_mut(&id) {
                    w.cursor = Some((position.x, position.y));
                }
                self.pointer(id, InputKind::PointerMove, 0);
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(w) = self.windows.get_mut(&id) {
                    w.cursor = None;
                }
                self.core
                    .route(&path, &InputEvent::new(InputKind::PointerLeave));
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let code = match button {
                    MouseButton::Left => 0,
                    MouseButton::Right => 1,
                    MouseButton::Middle => 2,
                    _ => 3,
                };
                let kind = match state {
                    ElementState::Pressed => InputKind::PointerDown,
                    ElementState::Released => InputKind::PointerUp,
                };
                self.pointer(id, kind, code);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x, y),
                    MouseScrollDelta::PixelDelta(p) => ((p.x / 40.0) as f32, (p.y / 40.0) as f32),
                };
                let mut e = InputEvent::new(InputKind::Wheel);
                e.dx = dx;
                e.dy = dy;
                if let Some(w) = self.windows.get(&id) {
                    e.modifiers = w.modifiers;
                }
                self.core.route(&path, &e);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let kind = match event.state {
                    ElementState::Pressed => InputKind::KeyDown,
                    ElementState::Released => InputKind::KeyUp,
                };
                let mut e = InputEvent::key(kind, &key_name(&event.logical_key));
                e.code = u32::from(event.repeat);
                if let Some(w) = self.windows.get(&id) {
                    e.modifiers = w.modifiers;
                }
                self.core.route(&path, &e);
            }
            _ => {}
        }
    }
}
