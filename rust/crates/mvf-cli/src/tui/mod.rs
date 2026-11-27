//! The interactive terminal dashboard (#35): `mvf manage tui` and `mvf run --tui`.
//!
//! The TUI is a management client, not a second execution path. It reads the same subscription
//! as `mvf manage watch` and sends the same commands as `mvf manage command`, so the runtime's
//! roles, validation and audit apply to it.
//!
//! - **The run never waits for it.** A slow, detached or crashed TUI never stops the runtime:
//!   - it only reads a feed the runtime publishes without blocking;
//!   - commands and previews run on their own threads and connections;
//!   - in `mvf run --tui` a TUI panic restores the terminal and the run goes on.
//! - **It reconnects:**
//!   - after a lost connection it subscribes again from the last sequence it saw;
//!   - after a gap, the fresh snapshot replaces its state;
//!   - after a restarted runtime it starts from a new snapshot.
//! - **Memory is bounded** (see [`state`]).

pub mod image;
pub mod layout;
pub mod render;
pub mod state;

use std::io::IsTerminal;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use mvf_management::Client;
use ratatui::crossterm::event::{self, Event};
#[cfg(test)]
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};
#[cfg(test)]
use std::collections::HashMap;

use state::{Action, App, Conn};

/// Finds the endpoint to (re)connect to.
pub type Discover = Arc<dyn Fn() -> Result<String, String> + Send + Sync>;

enum Msg {
    Hello(Value),
    Feed(Value),
    Lost(String),
    Reply(String, Result<Value, String>),
    Preview(u64, Value),
    PreviewFailed(u64, String),
}

/// Refuses to start without an interactive terminal.
pub fn interactive() -> Result<(), String> {
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        Ok(())
    } else {
        Err("the TUI needs an interactive terminal (stdin and stdout); for automation use `mvf manage watch` or `mvf manage status --json`".into())
    }
}

const CLIENT: &str = "mvf-tui";

/// Subscribes and forwards every message; reconnects until `stop`.
fn feed_thread(discover: Discover, tx: Sender<Msg>, stop: Arc<AtomicBool>) {
    let mut last: Option<(u64, Value)> = None; // (last seq, runtime identity)
    while !stop.load(Ordering::Relaxed) {
        let client =
            discover().and_then(|e| Client::connect(&e, CLIENT).map_err(|e| e.to_string()));
        let client = match client {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(Msg::Lost(e));
                std::thread::sleep(Duration::from_millis(1000));
                continue;
            }
        };
        let identity = json!([client.runtime()["pid"], client.runtime()["startedAtMs"]]);
        // The same runtime: continue after the last entry. Another one: start from a snapshot.
        let from = match &last {
            Some((seq, id)) if *id == identity => Some(seq + 1),
            _ => None,
        };
        if tx.send(Msg::Hello(client.hello().clone())).is_err() {
            return;
        }
        let mut sub = match client.subscribe(from) {
            Ok(s) => s,
            Err(e) => {
                let _ = tx.send(Msg::Lost(e.to_string()));
                continue;
            }
        };
        let why = loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            match sub.next() {
                Some(Ok(v)) => {
                    if let Some(seq) = sub.last_seq() {
                        last = Some((seq, identity.clone()));
                    }
                    if tx.send(Msg::Feed(v)).is_err() {
                        return;
                    }
                }
                Some(Err(e)) => break e.to_string(),
                None => break "the runtime closed the connection".to_string(),
            }
        };
        let _ = tx.send(Msg::Lost(why));
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn command(discover: Discover, tx: Sender<Msg>, id: String, name: String, args: Value) {
    std::thread::spawn(move || {
        let reply = discover().and_then(|e| {
            Client::connect(&e, CLIENT)
                .and_then(|mut c| c.command(&id, &name, args, Some(5000)))
                .map_err(|e| e.to_string())
        });
        let _ = tx.send(Msg::Reply(id, reply));
    });
}

fn preview(discover: Discover, tx: Sender<Msg>, generation: Arc<AtomicU64>, node: String) {
    let mine = generation.load(Ordering::SeqCst);
    std::thread::spawn(move || {
        let fail = |e: String| {
            let _ = tx.send(Msg::PreviewFailed(mine, e));
        };
        let mut c =
            match discover().and_then(|e| Client::connect(&e, CLIENT).map_err(|e| e.to_string())) {
                Ok(c) => c,
                Err(e) => return fail(e),
            };
        let mut next = c.request(&json!({"op": "preview", "node": node, "fps": 2}));
        // Each message ends a wait of at most a second (heartbeats), so a closed preview is noticed.
        while generation.load(Ordering::SeqCst) == mine {
            match next {
                Ok(m) if m["op"] == "preview" => {
                    if tx.send(Msg::Preview(mine, m)).is_err() {
                        return;
                    }
                }
                Ok(m) if m["op"] == "error" => {
                    return fail(match m["code"].as_str() {
                        Some("not-enabled") => format!(
                            "previews are off for `{node}`: start the runtime with --preview {node}"
                        ),
                        Some("forbidden") => {
                            "this session may not see previews (previewer role needed)".into()
                        }
                        _ => format!("{}: {}", m["code"], m["message"]),
                    });
                }
                Ok(_) => {}
                Err(e) => return fail(e.to_string()),
            }
            next = c.read_next();
        }
    });
}

/// Renders one frame of the dashboard as plain text (`mvf manage tui --print COLSxROWS`): the same
/// feed, model, layout and drawing as the TUI, without a terminal. For screenshots, and for a
/// smoke test wherever there is no TTY (Windows CI).
pub fn print(discover: Discover, cols: u16, rows: u16, settle: Duration) -> Result<String, String> {
    let (tx, rx): (Sender<Msg>, Receiver<Msg>) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    {
        let (d, tx, stop) = (Arc::clone(&discover), tx.clone(), Arc::clone(&stop));
        std::thread::spawn(move || feed_thread(d, tx, stop));
    }
    let mut app = App::new("print");
    let until = Instant::now() + settle;
    let mut lost = None;
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(Msg::Hello(h)) => {
                app.hello(&h);
                app.conn = Conn::Live;
            }
            Ok(Msg::Feed(v)) => {
                app.conn = Conn::Live;
                app.apply(&v);
            }
            Ok(Msg::Lost(why)) => lost = Some(why),
            Ok(_) => {}
            Err(_) => break,
        }
    }
    stop.store(true, Ordering::Relaxed);
    if app.nodes.is_empty() {
        return Err(lost.unwrap_or_else(|| "no snapshot from the runtime".into()));
    }
    app.track();
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(cols, rows))
        .map_err(|e| e.to_string())?;
    term.draw(|f| render::draw(f, &mut app, Instant::now()))
        .map_err(|e| e.to_string())?;
    let buf = term.backend().buffer();
    Ok((0..buf.area.height)
        .map(|y| {
            let line: String = (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            line.trim_end().to_string() + "\n"
        })
        .collect())
}

/// Runs the TUI until the operator quits. `on_quit` runs when they do (it stops the run in
/// `mvf run --tui`).
pub fn run(discover: Discover, on_quit: Option<Box<dyn FnOnce() + Send>>) -> Result<(), String> {
    interactive()?;
    let (tx, rx): (Sender<Msg>, Receiver<Msg>) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    {
        let (d, tx, stop) = (Arc::clone(&discover), tx.clone(), Arc::clone(&stop));
        std::thread::Builder::new()
            .name("mvf-tui-feed".into())
            .spawn(move || feed_thread(d, tx, stop))
            .map_err(|e| e.to_string())?;
    }
    let session = format!(
        "{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    );
    let mut app = App::new(&session);
    let previews = Arc::new(AtomicU64::new(0));
    let mut terminal = ratatui::try_init().map_err(|e| e.to_string())?;
    let started = Instant::now();
    let result = (|| -> std::io::Result<()> {
        loop {
            while let Ok(m) = rx.try_recv() {
                match m {
                    Msg::Hello(h) => {
                        app.hello(&h);
                        app.conn = Conn::Live;
                    }
                    Msg::Feed(v) => {
                        app.conn = Conn::Live;
                        app.apply(&v);
                    }
                    Msg::Lost(why) => app.conn = Conn::Lost(why),
                    Msg::Reply(id, r) => app.command_reply(&id, r),
                    Msg::Preview(g, m) if g == previews.load(Ordering::SeqCst) => {
                        app.preview_message(&m)
                    }
                    Msg::PreviewFailed(g, e) if g == previews.load(Ordering::SeqCst) => {
                        app.preview_error(&e)
                    }
                    _ => {}
                }
            }
            app.track();
            terminal.draw(|f| render::draw(f, &mut app, started))?;
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            // Resize needs nothing: the next frame is drawn at the new size.
            if let Event::Key(k) = event::read()? {
                match app.key(k) {
                    Action::Quit => return Ok(()),
                    Action::Command {
                        id,
                        command: name,
                        args,
                    } => {
                        command(Arc::clone(&discover), tx.clone(), id, name, args);
                    }
                    Action::Preview(node) => {
                        previews.fetch_add(1, Ordering::SeqCst);
                        if let Some(n) = node {
                            preview(Arc::clone(&discover), tx.clone(), Arc::clone(&previews), n);
                        }
                    }
                    Action::None => {}
                }
            }
        }
    })();
    ratatui::restore();
    stop.store(true, Ordering::Relaxed);
    previews.fetch_add(1, Ordering::SeqCst);
    if let Some(q) = on_quit {
        q();
    }
    result.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mvf_engine::{
        CancellationToken, Engine, HostResolver, HostServices, RunOptions, default_backends,
    };
    use mvf_management::{
        Control, Feed, FeedConfig, RuntimeInfo, Server, ServerOptions, Tap, Topology,
    };
    use std::sync::Mutex;

    const PIPELINE: &str = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: tui }\nspec:\n  nodes:\n    - { id: cycle, primitive: loop, config: { mode: forever } }\n    - { id: cam, builtin: synthetic-source, config: { width: 8, height: 8, frames: 50 } }\n    - { id: slow, builtin: fault, config: { delayMs: 25 } }\n    - { id: sink, builtin: record }\n    - { id: thr, primitive: value, config: { type: int, literal: 40, schema: { type: integer, minimum: 0, maximum: 255 } } }\n  edges:\n    data:\n      - { from: cam.frame, to: slow.frame }\n      - { from: slow.frame, to: sink.frame }\n    loop:\n      - { from: sink, to: cycle }\n";

    fn drain(rx: &Receiver<Msg>, app: &mut App, for_ms: u64, mut each: impl FnMut(&App)) {
        let until = Instant::now() + Duration::from_millis(for_ms);
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(Msg::Hello(h)) => {
                    app.hello(&h);
                    app.conn = Conn::Live;
                }
                Ok(Msg::Feed(v)) => {
                    app.conn = Conn::Live;
                    app.apply(&v);
                    each(app);
                }
                Ok(Msg::Lost(why)) => app.conn = Conn::Lost(why),
                Ok(Msg::Reply(id, r)) => app.command_reply(&id, r),
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }

    /// A real engine and management server; the TUI's own feed and command threads.
    #[test]
    fn the_model_follows_a_real_run_shows_the_running_node_and_commands_through_management() {
        let base = std::env::temp_dir().join(format!("mvf-tui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("dir");
        let resolver = HostResolver::new(&base);
        let graph = mvf_graph::build_graph(PIPELINE, &resolver).expect("graph");
        let mut engine =
            Engine::build(graph, &default_backends(), &HostServices::default()).expect("build");
        let feed = Feed::new(
            RuntimeInfo {
                pipeline: "tui".into(),
                version: "test".into(),
                mode: "serial".into(),
            },
            FeedConfig::default(),
        );
        feed.set_topology(&Topology::of(engine.graph(), &[]));
        let cancel = CancellationToken::new();
        let resolver = Arc::new(resolver);
        let start = |dir: &str| {
            Server::start(
                feed.clone(),
                ServerOptions {
                    dir: Some(base.join(dir)),
                    control: Some(Control {
                        handle: engine.control(),
                        cancel: cancel.clone(),
                        resolver: resolver.clone(),
                    }),
                    ..ServerOptions::default()
                },
            )
            .expect("server")
        };
        let server = start("run-a");
        let endpoint = Arc::new(Mutex::new(server.endpoint().to_string()));
        let mut tap = Tap::new(
            feed.clone(),
            engine.graph().nodes.iter().map(|n| n.id.clone()),
        );
        let options = RunOptions {
            cancel: cancel.clone(),
            ..RunOptions::default()
        };
        let second = start("run-b");
        let guard = cancel.clone();
        std::thread::scope(|scope| {
            let run = scope.spawn(move || {
                let r = engine.run(&options, &mut tap);
                tap.finish(&r);
                r
            });
            // A failed assertion must stop the run, or the scope waits for it forever.
            struct Stop(CancellationToken);
            impl Drop for Stop {
                fn drop(&mut self) {
                    self.0.cancel();
                }
            }
            let _stop = Stop(guard);
            let (tx, rx) = mpsc::channel();
            let stop = Arc::new(AtomicBool::new(false));
            let ep = Arc::clone(&endpoint);
            let discover: Discover = Arc::new(move || Ok(ep.lock().expect("lock").clone()));
            {
                let (d, tx, stop) = (Arc::clone(&discover), tx.clone(), Arc::clone(&stop));
                std::thread::spawn(move || feed_thread(d, tx, stop));
            }
            let mut app = App::new("test");
            // Which node the runtime said was running, sample by sample.
            let mut seen: HashMap<String, u64> = HashMap::new();
            let mut sample = |app: &App| {
                for n in &app.nodes {
                    if app.live_of(&n.id).active {
                        *seen.entry(n.id.clone()).or_default() += 1;
                    }
                }
            };
            drain(&rx, &mut app, 1500, &mut sample);
            assert_eq!(app.conn, Conn::Live);
            assert!(app.commands, "the local owner is an operator");
            assert_eq!(app.nodes.len(), 5);
            let thr = app.nodes.iter().find(|n| n.id == "thr").expect("thr");
            assert_eq!(thr.fields[0].command, "set-value");
            assert!(
                app.live_of("sink").executed >= 20,
                "{:?}",
                app.live_of("sink")
            );
            let slow = seen.get("slow").copied().unwrap_or(0);
            let others: u64 = seen
                .iter()
                .filter(|(k, _)| *k != "slow")
                .map(|(_, v)| *v)
                .sum();
            assert!(
                slow > others,
                "the 25 ms node is the one seen running: {seen:?}"
            );

            // Commands through the management API: a typed edit, then pause and resume.
            let edit = |app: &mut App, text: &str| {
                app.selected = app.nodes.iter().position(|n| n.id == "thr");
                app.view = state::View::Detail;
                app.field = 0;
                app.editing = Some(text.into());
                match app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
                    Action::Command {
                        id,
                        command: name,
                        args,
                    } => {
                        command(Arc::clone(&discover), tx.clone(), id, name, args);
                    }
                    other => panic!("{other:?}"),
                }
            };
            edit(&mut app, "41");
            drain(&rx, &mut app, 800, |_| {});
            assert!(
                app.notice.as_ref().is_some_and(
                    |n| n.1 == state::Tone::Good && n.0.contains("thr.value = 41: applied")
                ),
                "{:?}",
                app.notice
            );
            for (name, status) in [("pause", "paused"), ("resume", "running")] {
                let Action::Command {
                    id,
                    command: c,
                    args,
                } = app.key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))
                else {
                    panic!("space sends {name}");
                };
                assert_eq!(c, name);
                command(Arc::clone(&discover), tx.clone(), id, c, args);
                drain(&rx, &mut app, 700, |_| {});
                assert_eq!(app.status, status, "{:?}", app.notice);
            }

            // The endpoint goes away and comes back elsewhere: the TUI reconnects and continues
            // from the last sequence it saw, with no gap.
            let before = app.live_of("sink").executed;
            drop(server);
            *endpoint.lock().expect("lock") = second.endpoint().to_string();
            drain(&rx, &mut app, 2500, |_| {});
            assert_eq!(app.conn, Conn::Live);
            assert_eq!(app.gaps, 0);
            assert!(app.live_of("sink").executed > before, "still following");

            // The screen of the live run.
            let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 32))
                .expect("terminal");
            app.view = state::View::Graph;
            term.draw(|f| render::draw(f, &mut app, Instant::now()))
                .expect("draw");
            let buf = term.backend().buffer();
            let screen: String = (0..buf.area.height)
                .map(|y| {
                    (0..buf.area.width)
                        .map(|x| buf[(x, y)].symbol().to_string())
                        .collect::<String>()
                        + "\n"
                })
                .collect();
            eprintln!("{screen}");
            for want in ["tui", "slow", "cam", "sink", "thr", "cycle"] {
                assert!(screen.contains(want), "{want}:\n{screen}");
            }

            cancel.cancel();
            let report = run.join().expect("run");
            drain(&rx, &mut app, 1500, |_| {});
            assert_eq!(app.status, "stopped");
            assert!(
                app.nodes.iter().all(|n| !app.live_of(&n.id).active),
                "nothing runs after the end"
            );
            // `fault` reports a cancel during its delay as its own failure, of kind `Cancelled`.
            assert!(
                matches!(&report.end, mvf_engine::EndReason::Cancelled)
                    || matches!(&report.end, mvf_engine::EndReason::NodeFailed { error, .. } if error.kind == mvf_engine::NodeErrorKind::Cancelled),
                "{:?}",
                report.end
            );
            stop.store(true, Ordering::Relaxed);
        });
        drop(second);
    }
}
