//! Drawing the TUI (#35): the legacy dashboard's look, in ASCII only.
//!
//! Every glyph is ASCII and one cell wide, so nothing drifts on panel-PC and Windows console
//! fonts (the lesson of the legacy `Glyphs` table). That covers box borders, status icons, edge
//! shafts, the spinner and the sparkline.
//!
//! Colours follow the legacy palette (xterm-256):
//! - category hues for the nodes;
//! - a breathing border on the node that runs right now;
//! - an afterglow on the node that just ran;
//! - a comet along each data edge.

use std::time::Instant;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout as Split, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use super::image::{Pixel, cells};
use super::layout::{BOX_H, BOX_W};
use super::state::{App, Conn, Live, Node, Tone, View, duration};

/// ASCII box border.
pub const ASCII: border::Set = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

/// The selected node's border.
const SELECTED: border::Set = border::Set {
    top_left: "#",
    top_right: "#",
    bottom_left: "#",
    bottom_right: "#",
    vertical_left: "#",
    vertical_right: "#",
    horizontal_top: "=",
    horizontal_bottom: "=",
};

const fn c(i: u8) -> Color {
    Color::Indexed(i)
}

const SKY: Color = c(39);
const PURPLE: Color = c(140);
const PINK: Color = c(205);
const ORCHID: Color = c(170);
const GOLD: Color = c(220);
const SPRING: Color = c(49);
const AQUA: Color = c(122);
const GREY84: Color = c(252);
const GREY58: Color = c(246);
const GREY54: Color = c(245);
const GREY42: Color = c(242);
const GREY35: Color = c(240);
const YELLOW: Color = c(226);
const RED: Color = c(196);
const GREEN: Color = c(40);
const STEEL: Color = c(75);
const STEEL3: Color = c(68);
const ORANGE: Color = c(208);
const WHITE: Color = c(15);

const SPINNER: &[u8] = b"|/-\\";
const BREATHE: [Color; 3] = [ORANGE, GOLD, YELLOW];
const AFTERGLOW_MS: u128 = 220;

fn category_colour(category: &str) -> Color {
    match category {
        "source" => SKY,
        "compute" => PURPLE,
        "classify" => PINK,
        "flow" => GOLD,
        "sink" => SPRING,
        "value" => AQUA,
        _ => GREY84,
    }
}

fn tone(t: Tone) -> Color {
    match t {
        Tone::Info => GREY84,
        Tone::Good => GREEN,
        Tone::Warn => GOLD,
        Tone::Bad => RED,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max - 1).collect();
        t.push('~');
        t
    }
}

/// How a node is hosted, as one tinted letter.
fn badge(n: &Node, backend: &str) -> (char, Color) {
    if n.primitive.is_some() {
        return ('P', GOLD);
    }
    match backend {
        "worker" => ('W', STEEL),
        "oci" => ('C', ORCHID),
        "compute" | "gpu" => ('G', GREEN),
        _ => ('B', SPRING),
    }
}

/// Triangle wave index for the breathing border.
fn breathe(ms: u128) -> Color {
    let p = (ms % 800) as f64 / 800.0;
    let t = if p < 0.5 { p * 2.0 } else { (1.0 - p) * 2.0 };
    BREATHE[((t * 3.0) as usize).min(2)]
}

/// Draws the whole frame.
pub fn draw(f: &mut Frame, app: &mut App, started: Instant) {
    let area = f.area();
    let ms = started.elapsed().as_millis();
    if area.width < 40 || area.height < 12 {
        f.render_widget(
            Paragraph::new("terminal too small for mvf tui (40x12 needed)")
                .style(Style::new().fg(GOLD)),
            area,
        );
        return;
    }
    let rows = Split::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(6),
            Constraint::Length(1),
        ])
        .split(area);
    header(f, app, rows[0], ms);
    status_line(f, app, rows[1]);
    match app.view {
        View::Graph => {
            let log_h = (rows[2].height / 3).clamp(4, 10);
            let body = Split::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(4), Constraint::Length(log_h)])
                .split(rows[2]);
            graph(f.buffer_mut(), app, body[0], ms);
            feed_panel(f, app, body[1], None);
        }
        View::Detail => detail(f, app, rows[2]),
    }
    footer(f, app, rows[3]);
    if app.help {
        help(f, area);
    }
}

fn header(f: &mut Frame, app: &App, r: Rect, ms: u128) {
    let live = app.status == "running" && app.conn == Conn::Live;
    // The logo's colour wave runs while the pipeline does.
    let logo: Vec<Span> = "MVF"
        .chars()
        .enumerate()
        .map(|(i, ch)| {
            let colour = if live && ((ms / 150) as usize % 3) == i {
                WHITE
            } else {
                SKY
            };
            Span::styled(
                ch.to_string(),
                Style::new().fg(colour).add_modifier(Modifier::BOLD),
            )
        })
        .collect();
    let (status, colour) = match (&app.conn, app.status.as_str()) {
        (Conn::Lost(_), _) => (" DISCONNECTED ", RED),
        (Conn::Connecting, _) => (" .. CONNECTING ", GREY54),
        (_, "paused") => (" || PAUSED ", GOLD),
        (_, "running") => (" >> RUNNING ", YELLOW),
        (_, "stopped") => match app.end_node() {
            Some((_, "failed")) => (" !! FAILED ", RED),
            _ => (" OK DONE ", GREEN),
        },
        _ => (" .. STARTING ", GREY54),
    };
    let restarts: u64 = app.live.values().map(|l| l.restarts).sum();
    let failures: u64 = app.live.values().map(|l| l.failures).sum();
    let elapsed = app.runtime["startedAtMs"].as_f64().map(|t| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as f64)
            .unwrap_or(t);
        duration((now - t).max(0.0) * 1e3)
    });
    let mut spans = logo;
    spans.extend([
        Span::raw(" "),
        Span::styled(
            app.pipeline.clone(),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(status, Style::new().fg(Color::Black).bg(colour)),
        Span::styled("  cyc:", Style::new().fg(GREY54)),
        Span::styled(app.cycle.to_string(), Style::new().fg(WHITE)),
        Span::styled(
            format!("  {:.1}/s", app.cycles_per_s),
            Style::new().fg(GREY58),
        ),
        Span::styled("  t:", Style::new().fg(GREY54)),
        Span::styled(
            elapsed.unwrap_or_else(|| "-".into()),
            Style::new().fg(GREY58),
        ),
        Span::styled("  mode:", Style::new().fg(GREY54)),
        Span::styled(
            app.runtime["mode"].as_str().unwrap_or("-").to_string(),
            Style::new().fg(GREY58),
        ),
    ]);
    if failures > 0 || restarts > 0 {
        spans.push(Span::styled(
            format!("  fail:{failures} rst:{restarts}"),
            Style::new().fg(RED),
        ));
    }
    if app.gaps > 0 {
        spans.push(Span::styled(
            format!("  gaps:{}", app.gaps),
            Style::new().fg(GOLD),
        ));
    }
    if let Some(pid) = app.runtime["pid"].as_u64() {
        spans.push(Span::styled(
            format!("  pid:{pid}"),
            Style::new().fg(GREY42),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), r);
}

fn status_line(f: &mut Frame, app: &App, r: Rect) {
    let line = if let Some((text, t)) = &app.notice {
        Line::styled(text.clone(), Style::new().fg(tone(*t)))
    } else if let Conn::Lost(why) = &app.conn {
        Line::styled(
            format!("connection lost ({why}); reconnecting..."),
            Style::new().fg(RED),
        )
    } else if app.status == "stopped" {
        Line::styled(
            format!("run ended: {}", app.end_text()),
            Style::new().fg(GOLD),
        )
    } else {
        let sel = app
            .selected_node()
            .map(|n| n.id.clone())
            .unwrap_or_else(|| "-".into());
        Line::from(vec![
            Span::styled("sel:", Style::new().fg(GREY54)),
            Span::styled(sel, Style::new().fg(SKY)),
            Span::styled(
                if app.follow {
                    "  follow:on"
                } else {
                    "  follow:off"
                },
                Style::new().fg(GREY42),
            ),
            Span::styled("   * src ", Style::new().fg(SKY)),
            Span::styled("* compute ", Style::new().fg(PURPLE)),
            Span::styled("* classify ", Style::new().fg(PINK)),
            Span::styled("* flow ", Style::new().fg(GOLD)),
            Span::styled("* sink ", Style::new().fg(SPRING)),
            Span::styled("* value ", Style::new().fg(AQUA)),
            Span::styled(" --- data  === control", Style::new().fg(GREY42)),
        ])
    };
    f.render_widget(Paragraph::new(line), r);
}

fn footer(f: &mut Frame, app: &App, r: Rect) {
    let keys = match (app.view, app.editing.is_some()) {
        (_, true) => "type a value | enter send | esc cancel".to_string(),
        (View::Graph, _) => "<> move | ^v layer | enter details | f follow | shift+arrows/hjkl pan | space pause | ? help | q quit".into(),
        (View::Detail, _) => "^v field | enter edit | p preview | space pause | esc back | ? help | q quit".into(),
    };
    f.render_widget(
        Paragraph::new(Line::styled(keys, Style::new().fg(GREY42))),
        r,
    );
}

/// Keeps the selected (or followed) node's box in view unless the viewport was panned by hand.
fn fit(app: &mut App, vw: u16, vh: u16) {
    let (cw, ch) = (app.layout.width + 2, app.layout.height + 2);
    if !app.pinned
        && let Some(sel) = app.selected
        && sel < app.layout.node_item.len()
    {
        let (bx, by) = app.layout.origin(sel);
        let (bx, by) = (bx + 1, by + 1);
        let (mut sx, mut sy) = app.scroll;
        if bx < sx {
            sx = bx.saturating_sub(1);
        } else if bx + BOX_W > sx + vw {
            sx = (bx + BOX_W + 1).saturating_sub(vw);
        }
        if by < sy {
            sy = by.saturating_sub(1);
        } else if by + BOX_H > sy + vh {
            sy = (by + BOX_H + 1).saturating_sub(vh);
        }
        app.scroll = (sx, sy);
    }
    app.scroll.0 = app.scroll.0.min(cw.saturating_sub(vw));
    app.scroll.1 = app.scroll.1.min(ch.saturating_sub(vh));
}

fn graph(buf: &mut Buffer, app: &mut App, r: Rect, ms: u128) {
    if app.nodes.is_empty() {
        buf.set_stringn(
            r.x,
            r.y,
            "waiting for the topology...",
            r.width as usize,
            Style::new().fg(GREY54),
        );
        return;
    }
    fit(app, r.width, r.height);
    let canvas = canvas(app, ms);
    let (sx, sy) = app.scroll;
    for y in 0..r.height {
        for x in 0..r.width {
            let (cx, cy) = (sx + x, sy + y);
            if cx < canvas.area.width
                && cy < canvas.area.height
                && let (Some(src), Some(dst)) =
                    (canvas.cell((cx, cy)), buf.cell_mut((r.x + x, r.y + y)))
            {
                *dst = src.clone();
            }
        }
    }
    // Scroll hints when the graph is larger than the view.
    let style = Style::new().fg(GOLD);
    if sx > 0 {
        buf.set_string(r.x, r.y + r.height / 2, "<", style);
    }
    if sx + r.width < canvas.area.width {
        buf.set_string(r.x + r.width - 1, r.y + r.height / 2, ">", style);
    }
    if sy > 0 {
        buf.set_string(r.x + r.width / 2, r.y, "^", style);
    }
    if sy + r.height < canvas.area.height {
        buf.set_string(r.x + r.width / 2, r.y + r.height - 1, "v", style);
    }
}

/// The whole graph, drawn once per frame on its own buffer (margin 1).
pub fn canvas(app: &App, ms: u128) -> Buffer {
    let l = &app.layout;
    let mut buf = Buffer::empty(Rect::new(0, 0, l.width + 2, l.height + 2));
    let animate = app.status == "running" && app.conn == Conn::Live;
    let active: Vec<bool> = app
        .nodes
        .iter()
        .map(|n| app.live.get(&n.id).is_some_and(|x| x.active))
        .collect();
    // Edges first, boxes on top.
    for route in &l.routes {
        let control = route.cells.iter().any(|c| c.ch == '=');
        let from_active = app
            .edges
            .get(route.edge)
            .and_then(|e| app.nodes.iter().position(|n| n.id == e.from))
            .is_some_and(|i| active[i]);
        let base = if control { ORCHID } else { STEEL3 };
        let len = route.cells.len().max(1) as u128;
        let period = len * if from_active { 25 } else { 45 };
        let head = ((ms % period) * len / period) as usize;
        for (i, cell) in route.cells.iter().enumerate() {
            let colour = if animate && !control && cell.ch != '>' {
                match head as i64 - i as i64 {
                    0 => {
                        if from_active {
                            WHITE
                        } else {
                            STEEL
                        }
                    }
                    1 | 2 => STEEL,
                    _ => base,
                }
            } else if cell.ch == '>' {
                if control { ORCHID } else { STEEL }
            } else {
                base
            };
            buf.set_string(
                cell.x + 1,
                cell.y + 1,
                cell.ch.to_string(),
                Style::new().fg(colour),
            );
        }
        if let Some((x, y, text)) = &route.label {
            buf.set_string(x + 1, y + 1, text, Style::new().fg(GREY54));
        }
    }
    let end = app.end_node();
    for (i, n) in app.nodes.iter().enumerate() {
        if i >= l.node_item.len() {
            continue;
        }
        let (x, y) = l.origin(i);
        let live = app.live_of(&n.id);
        let ended = end.as_ref().filter(|(id, _)| *id == n.id).map(|(_, k)| *k);
        node_box(
            &mut buf,
            x + 1,
            y + 1,
            n,
            &live,
            app.selected == Some(i),
            ended,
            ms,
            animate,
        );
    }
    buf
}

#[allow(clippy::too_many_arguments)]
fn node_box(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    n: &Node,
    live: &Live,
    selected: bool,
    ended: Option<&str>,
    ms: u128,
    animate: bool,
) {
    let cat = category_colour(n.category());
    let failed = ended == Some("failed");
    let (icon, icon_colour) = if live.active && !failed {
        let ch = if animate {
            SPINNER[(ms / 100) as usize % SPINNER.len()] as char
        } else {
            '>'
        };
        (ch, YELLOW)
    } else if failed {
        ('x', RED)
    } else if ended == Some("exhausted") {
        ('o', GOLD)
    } else if live.executed > 0 {
        ('+', GREEN)
    } else {
        ('-', GREY42)
    };
    let glow = animate
        && live
            .fired
            .is_some_and(|t| t.elapsed().as_millis() < AFTERGLOW_MS);
    let border_style = if selected {
        Style::new().fg(SKY).add_modifier(Modifier::BOLD)
    } else if live.active && !failed {
        Style::new()
            .fg(if animate { breathe(ms) } else { YELLOW })
            .add_modifier(Modifier::BOLD)
    } else if failed {
        Style::new().fg(RED)
    } else if live.executed > 0 {
        let s = Style::new().fg(cat);
        if glow {
            s.add_modifier(Modifier::BOLD)
        } else {
            s
        }
    } else {
        Style::new().fg(GREY35)
    };
    let set = if selected { SELECTED } else { ASCII };
    let area = Rect::new(x, y, BOX_W, BOX_H);
    let block = Block::bordered().border_set(set).border_style(border_style);
    ratatui::widgets::Widget::render(block, area, buf);
    let w = (BOX_W - 2) as usize;
    let editable = if n.fields.is_empty() { "" } else { " *" };
    buf.set_string(x + 1, y + 1, icon.to_string(), Style::new().fg(icon_colour));
    let name = truncate(&n.id, w - 2 - editable.len());
    let name_style = if selected {
        Style::new().fg(Color::Black).bg(SKY)
    } else {
        Style::new().fg(cat)
    };
    buf.set_string(x + 3, y + 1, &name, name_style);
    if !editable.is_empty() {
        buf.set_string(
            x + 3 + name.len() as u16,
            y + 1,
            editable,
            Style::new().fg(GREY42),
        );
    }
    let backend = live.backend.as_deref().unwrap_or(&n.backend);
    let (b, bc) = badge(n, backend);
    buf.set_string(x + 1, y + 2, b.to_string(), Style::new().fg(bc));
    let contract = live.contract.as_deref().unwrap_or(&n.contract);
    buf.set_string(
        x + 3,
        y + 2,
        truncate(contract, w - 2),
        Style::new().fg(GREY54),
    );
    let mut spans: Vec<(String, Color)> = Vec::new();
    if live.executed == 0 {
        spans.push(("waiting".into(), GREY42));
    } else {
        spans.push((format!("x{}", live.executed), WHITE));
        spans.push((format!(" {}", duration(live.p50)), GREY58));
    }
    if live.running > 1 {
        spans.push((format!(" |{}", live.running), YELLOW));
    }
    if live.failures > 0 {
        spans.push((format!(" !{}", live.failures), RED));
    }
    if live.restarts > 0 {
        spans.push((format!(" r{}", live.restarts), RED));
    }
    if live.generation > 1 {
        spans.push((format!(" g{}", live.generation), GOLD));
    }
    let mut cx = x + 1;
    for (text, colour) in spans {
        let room = (x + 1 + w as u16).saturating_sub(cx) as usize;
        if room == 0 {
            break;
        }
        let t = truncate(&text, room);
        buf.set_string(cx, y + 3, &t, Style::new().fg(colour));
        cx += t.chars().count() as u16;
    }
}

fn panel(title: &str, colour: Color) -> Block<'static> {
    Block::bordered()
        .border_set(ASCII)
        .border_style(Style::new().fg(colour))
        .title(Span::styled(format!(" {title} "), Style::new().fg(colour)))
}

/// Events and logs, side by side when there is room. With `node`, only that node's.
fn feed_panel(f: &mut Frame, app: &App, r: Rect, node: Option<&str>) {
    let rows = r.height.saturating_sub(2) as usize;
    let events: Vec<Line> = last(
        app.events
            .iter()
            .filter(|e| node.is_none() || e.node.as_deref() == node),
        rows,
    )
    .into_iter()
    .map(|e| {
        let who = e
            .node
            .as_deref()
            .map(|n| format!("{n}: "))
            .unwrap_or_default();
        Line::styled(format!("{who}{}", e.text), Style::new().fg(tone(e.tone)))
    })
    .collect();
    let logs: Vec<Line> = match node {
        Some(n) => app
            .node_logs
            .get(n)
            .map(|q| {
                q.iter()
                    .rev()
                    .take(rows)
                    .rev()
                    .map(|l| Line::styled(l.clone(), Style::new().fg(GREY84)))
                    .collect()
            })
            .unwrap_or_default(),
        None => app
            .logs
            .iter()
            .rev()
            .take(rows)
            .rev()
            .map(|l| {
                let colour = match l.level.as_str() {
                    "error" | "stderr" => RED,
                    "warn" | "warning" => GOLD,
                    "supervisor" => GREY54,
                    _ => GREY84,
                };
                Line::from(vec![
                    Span::styled(
                        l.node
                            .as_deref()
                            .map(|n| format!("{n} "))
                            .unwrap_or_default(),
                        Style::new().fg(SKY),
                    ),
                    Span::styled(l.message.clone(), Style::new().fg(colour)),
                ])
            })
            .collect(),
    };
    let dropped = if app.dropped_logs > 0 {
        format!("logs ({} dropped by the runtime)", app.dropped_logs)
    } else {
        "logs".into()
    };
    let empty = |lines: Vec<Line<'static>>, text: &str| {
        if lines.is_empty() {
            vec![Line::styled(text.to_string(), Style::new().fg(GREY42))]
        } else {
            lines
        }
    };
    if r.width >= 100 {
        let cols = Split::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(r);
        f.render_widget(
            Paragraph::new(empty(events, "(no events)")).block(panel("events", GREY54)),
            cols[0],
        );
        f.render_widget(
            Paragraph::new(empty(logs, "(no logs yet)")).block(panel(&dropped, GREY54)),
            cols[1],
        );
    } else {
        let mut all = events;
        all.extend(logs);
        let start = all.len().saturating_sub(rows);
        f.render_widget(
            Paragraph::new(empty(all.split_off(start), "(nothing yet)"))
                .block(panel("events and logs", GREY54)),
            r,
        );
    }
}

/// The last `n` items.
fn last<T>(it: impl Iterator<Item = T>, n: usize) -> Vec<T> {
    let mut all: Vec<T> = it.collect();
    let start = all.len().saturating_sub(n);
    all.split_off(start)
}

/// An ASCII sparkline of `values` in `width` cells.
fn sparkline(values: &[u64], width: usize) -> String {
    const BARS: &[u8] = b"_.-=+*#";
    let tail = &values[values.len().saturating_sub(width)..];
    let max = tail.iter().copied().max().unwrap_or(0).max(1);
    tail.iter()
        .map(|&v| BARS[(v as usize * (BARS.len() - 1)) / max as usize] as char)
        .collect()
}

fn detail(f: &mut Frame, app: &App, r: Rect) {
    let Some(n) = app.selected_node() else {
        return;
    };
    let live = app.live_of(&n.id);
    let contract = live.contract.clone().unwrap_or_else(|| n.contract.clone());
    let backend = live.backend.clone().unwrap_or_else(|| n.backend.clone());
    let rows = Split::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(8),
            Constraint::Percentage(40),
        ])
        .split(r);
    let mut title = vec![
        Span::styled("# NODE ", Style::new().fg(SKY).add_modifier(Modifier::BOLD)),
        Span::styled(n.id.clone(), Style::new().add_modifier(Modifier::BOLD)),
        Span::styled("  contract:", Style::new().fg(GREY54)),
        Span::styled(contract, Style::new().fg(GREY84)),
        Span::styled("  backend:", Style::new().fg(GREY54)),
        Span::styled(backend, Style::new().fg(GREY84)),
        Span::styled("  gen:", Style::new().fg(GREY54)),
        Span::styled(live.generation.max(1).to_string(), Style::new().fg(GREY84)),
        Span::styled("  cat:", Style::new().fg(GREY54)),
        Span::styled(n.category(), Style::new().fg(category_colour(n.category()))),
    ];
    if let Some(p) = &n.primitive {
        title.push(Span::styled(
            format!("  primitive:{p}"),
            Style::new().fg(GOLD),
        ));
    }
    if n.parallelism > 1 {
        title.push(Span::styled(
            format!("  x{} instances", n.parallelism),
            Style::new().fg(GREY84),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(title)), rows[0]);

    let top = Split::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[1]);
    let left = Split::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(top[0]);

    // Ports, with what each is wired to.
    let mut lines = Vec::new();
    for (dir, list, incoming) in [("in ", &n.inputs, true), ("out", &n.outputs, false)] {
        if list.is_empty() {
            lines.push(Line::styled(format!("{dir} -"), Style::new().fg(GREY42)));
        }
        for p in list.iter() {
            let peers: Vec<String> = app
                .edges
                .iter()
                .filter(|e| {
                    if incoming {
                        e.to == n.id && e.to_port == p.name
                    } else {
                        e.from == n.id && e.from_port == p.name
                    }
                })
                .map(|e| {
                    if incoming {
                        format!("{}.{}", e.from, e.from_port)
                    } else {
                        format!("{}.{}", e.to, e.to_port)
                    }
                })
                .collect();
            let colour = if p.channel == "control" {
                ORCHID
            } else {
                STEEL
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{dir} "), Style::new().fg(GREY54)),
                Span::styled(p.name.clone(), Style::new().fg(colour)),
                Span::styled(
                    format!(": {} ({})", p.ty, p.channel),
                    Style::new().fg(GREY58),
                ),
                Span::styled(
                    if peers.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " {} {}",
                            if incoming { "<-" } else { "->" },
                            peers.join(", ")
                        )
                    },
                    Style::new().fg(GREY42),
                ),
            ]));
        }
    }
    f.render_widget(
        Paragraph::new(lines)
            .block(panel("ports", STEEL))
            .wrap(Wrap { trim: false }),
        left[0],
    );

    // Editable fields.
    let mut lines = Vec::new();
    if n.fields.is_empty() {
        lines.push(Line::styled(
            "no live-editable fields",
            Style::new().fg(GREY42),
        ));
    }
    for (i, fld) in n.fields.iter().enumerate() {
        let sel = i == app.field;
        let mut bounds = Vec::new();
        if let Some(s) = &fld.schema {
            if let Some(min) = s["minimum"].as_f64() {
                bounds.push(format!(">={min}"));
            }
            if let Some(max) = s["maximum"].as_f64() {
                bounds.push(format!("<={max}"));
            }
            if let Some(e) = s["enum"].as_array() {
                bounds.push(format!(
                    "one of {}",
                    e.iter()
                        .map(|v| v.to_string())
                        .collect::<Vec<_>>()
                        .join("|")
                ));
            }
        }
        let ty = if fld.shape == "list" {
            format!("list<{}>", fld.elem)
        } else {
            fld.elem.clone()
        };
        let set_here = app
            .applied
            .get(&(n.id.clone(), fld.field.clone()))
            .map(|v| format!(" = {v} (set here)"))
            .unwrap_or_default();
        let origin = fld
            .binding
            .as_deref()
            .map(|b| format!("-> {b} (persisted)"))
            .unwrap_or_else(|| "(this run only)".into());
        lines.push(Line::from(vec![
            Span::styled(if sel { "> " } else { "  " }, Style::new().fg(SKY)),
            Span::styled(
                fld.field.clone(),
                if sel {
                    Style::new().fg(Color::Black).bg(SKY)
                } else {
                    Style::new().fg(WHITE)
                },
            ),
            Span::styled(set_here, Style::new().fg(GREEN)),
            Span::styled(
                format!(" {ty} {}", bounds.join(" ")),
                Style::new().fg(GREY58),
            ),
            Span::styled(format!("  {origin}"), Style::new().fg(GREY42)),
        ]));
        if sel && let Some(text) = &app.editing {
            lines.push(Line::from(vec![
                Span::styled("  new value: ", Style::new().fg(GOLD)),
                Span::styled(
                    format!("{text}_"),
                    Style::new().fg(WHITE).add_modifier(Modifier::BOLD),
                ),
            ]));
        }
    }
    if !n.fields.is_empty() {
        lines.push(Line::styled(
            "current values are not exposed by management; edits go through configure/set-value",
            Style::new().fg(GREY42),
        ));
    }
    f.render_widget(
        Paragraph::new(lines)
            .block(panel("editable", GOLD))
            .wrap(Wrap { trim: false }),
        left[1],
    );

    // Stats and the latency history.
    let right = Split::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(6), Constraint::Length(4)])
        .split(top[1]);
    let end = app.end_node().filter(|(id, _)| *id == n.id).map(|(_, k)| k);
    let (state, colour) = match (live.active, end) {
        (_, Some("failed")) => ("failed (ended the run)", RED),
        (_, Some("exhausted")) => ("exhausted (no more input)", GOLD),
        (true, _) => ("active (executing now)", YELLOW),
        _ if live.executed > 0 => ("idle (has run)", GREEN),
        _ => ("idle (not run yet)", GREY54),
    };
    let kv = |k: &str, v: String, colour: Color| {
        Line::from(vec![
            Span::styled(format!("{k:<10}"), Style::new().fg(GREY54)),
            Span::styled(v, Style::new().fg(colour)),
        ])
    };
    let mut stats = vec![
        kv("status", state.into(), colour),
        kv(
            "executed",
            format!(
                "{}  skipped {}  rate {:.1}/s",
                live.executed, live.skipped, live.per_second
            ),
            WHITE,
        ),
        kv(
            "failures",
            format!(
                "{}  restarts {}  dropped {}",
                live.failures, live.restarts, live.dropped
            ),
            if live.failures + live.restarts + live.dropped > 0 {
                RED
            } else {
                GREY84
            },
        ),
        kv(
            "latency",
            format!(
                "p50 {}  p95 {}  p99 {}",
                duration(live.p50),
                duration(live.p95),
                duration(live.p99)
            ),
            GREY84,
        ),
        kv(
            "",
            format!("mean {}  max {}", duration(live.mean), duration(live.max)),
            GREY84,
        ),
    ];
    if let Some(s) = &live.last_skip {
        stats.push(kv("last skip", s.clone(), GOLD));
    }
    f.render_widget(Paragraph::new(stats).block(panel("stats", STEEL)), right[0]);
    let history: Vec<u64> = live.history.iter().copied().collect();
    let width = right[1].width.saturating_sub(2) as usize;
    let max = history.iter().copied().max().unwrap_or(0);
    let spark = vec![
        Line::styled(sparkline(&history, width), Style::new().fg(STEEL)),
        Line::styled(
            format!("window mean, newest right; max {}", duration(max as f64)),
            Style::new().fg(GREY42),
        ),
    ];
    f.render_widget(
        Paragraph::new(spark).block(panel("latency", STEEL)),
        right[1],
    );

    // Bottom: preview (when open) or the node's events, then its logs.
    let bottom = Split::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[2]);
    if app.preview.node.is_some() {
        preview(f, app, bottom[0]);
    } else {
        let lines: Vec<Line> = last(
            app.events
                .iter()
                .filter(|e| e.node.as_deref() == Some(&n.id)),
            bottom[0].height.saturating_sub(2) as usize,
        )
        .into_iter()
        .map(|e| Line::styled(e.text.clone(), Style::new().fg(tone(e.tone))))
        .collect();
        let lines = if lines.is_empty() {
            vec![Line::styled(
                "(no events for this node)",
                Style::new().fg(GREY42),
            )]
        } else {
            lines
        };
        f.render_widget(
            Paragraph::new(lines).block(panel("events", GREY54)),
            bottom[0],
        );
    }
    let logs: Vec<Line> = app
        .node_logs
        .get(&n.id)
        .map(|q| {
            q.iter()
                .rev()
                .take(bottom[1].height.saturating_sub(2) as usize)
                .rev()
                .map(|l| Line::styled(l.clone(), Style::new().fg(GREY84)))
                .collect()
        })
        .unwrap_or_default();
    let logs = if logs.is_empty() {
        vec![Line::styled(
            "(no logs from this node)",
            Style::new().fg(GREY42),
        )]
    } else {
        logs
    };
    f.render_widget(Paragraph::new(logs).block(panel("logs", GREY54)), bottom[1]);
}

fn preview(f: &mut Frame, app: &App, r: Rect) {
    let title = match &app.preview.image {
        Some((cycle, img)) => format!("preview cycle {cycle} ({}x{})", img.width, img.height),
        None => "preview".into(),
    };
    let block = panel(&title, AQUA);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let buf = f.buffer_mut();
    let mut y = inner.y;
    if let Some((_, img)) = &app.preview.image {
        let colour = super::image::truecolor();
        let reserve = u16::from(app.preview.note.is_some());
        for (row, line) in cells(
            img,
            inner.width as usize,
            inner.height.saturating_sub(reserve) as usize,
            colour,
        )
        .iter()
        .enumerate()
        {
            for (col, px) in line.iter().enumerate() {
                let (x, yy) = (inner.x + col as u16, inner.y + row as u16);
                if let Some(cell) = buf.cell_mut((x, yy)) {
                    match *px {
                        Pixel::Colour(r, g, b) => {
                            cell.set_symbol(" ");
                            cell.set_bg(Color::Rgb(r, g, b));
                        }
                        Pixel::Ramp(ch) => {
                            cell.set_symbol(&ch.to_string());
                            cell.set_fg(GREY84);
                        }
                    }
                }
            }
            y = inner.y + row as u16 + 1;
        }
    }
    if let Some(note) = &app.preview.note
        && y < inner.y + inner.height
    {
        buf.set_stringn(
            inner.x,
            y,
            note,
            inner.width as usize,
            Style::new().fg(GREY54),
        );
    }
}

fn help(f: &mut Frame, area: Rect) {
    let text = [
        "graph",
        "  <- ->, tab      previous / next node (reading order)",
        "  up down         node above / below in the layer",
        "  shift+arrows    pan the graph (also h j k l)",
        "  f               follow the running node on / off",
        "  enter           node details",
        "details",
        "  up down         choose an editable field",
        "  enter, e        edit it (configure / set-value; operator only)",
        "  p               preview the node's output (needs --preview NODE)",
        "  esc             back to the graph",
        "anywhere",
        "  space           pause / resume (needs a loop)",
        "  q, ctrl+c       quit (mvf run --tui: also stops the run)",
        "",
        "  any key closes this help",
    ];
    let w = 70.min(area.width);
    let h = (text.len() as u16 + 2).min(area.height);
    let r = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(text.iter().map(|t| Line::raw(*t)).collect::<Vec<_>>())
            .block(panel("keys", SKY)),
        r,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::json;

    fn app() -> App {
        let mut app = App::new("t");
        app.conn = Conn::Live;
        app.apply(&json!({"op": "snapshot", "seq": 1, "snapshot": {
            "runtime": {"pipeline": "inspect", "mode": "serial", "pid": 7, "startedAtMs": 0},
            "status": "running", "cycle": 12,
            "topology": {"pipeline": "inspect", "nodes": [
                {"id": "cam", "contract": "mvf.synthetic-source@1.0.0", "backend": "in-process", "parallelism": 1, "inputs": [], "outputs": [{"name": "frame", "type": "frame", "channel": "data"}]},
                {"id": "gate", "contract": "mvf.brightness-gate@1.0.0", "backend": "in-process", "parallelism": 1, "inputs": [{"name": "frame", "type": "frame", "channel": "data"}], "outputs": [{"name": "bright", "type": "bool", "channel": "control"}]},
                {"id": "score", "contract": "py.brightness-classifier@1.0.0", "backend": "worker", "parallelism": 1, "inputs": [{"name": "frame", "type": "frame", "channel": "data"}], "outputs": [{"name": "class", "type": "classification", "channel": "data"}]},
                {"id": "sink", "contract": "mvf.record@1.0.0", "backend": "in-process", "parallelism": 1, "inputs": [{"name": "frame", "type": "frame", "channel": "data"}, {"name": "enable", "type": "bool", "channel": "control"}], "outputs": []}
            ], "edges": {
                "data": [{"from": "cam.frame", "to": "gate.frame", "type": "frame"}, {"from": "cam.frame", "to": "score.frame", "type": "frame"}, {"from": "cam.frame", "to": "sink.frame", "type": "frame"}],
                "control": [{"from": "gate.bright", "to": "sink.enable", "type": "bool"}]}},
            "nodes": {"score": {"executed": 12, "failures": 1, "restarts": 1, "latencyUs": {"p50": 1500.0}, "window": {"executed": 3, "meanUs": 1500.0}}},
            "activity": {"score": 1},
            "recentLogs": [{"node": "score", "level": "info", "message": "model loaded"}],
            "recentEvents": []
        }}));
        app
    }

    fn text(buf: &Buffer) -> String {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf.cell((x, y)).map_or(" ", |c| c.symbol()).to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_graph_screen_shows_nodes_edges_status_and_only_ascii() {
        let mut app = app();
        let mut term = Terminal::new(TestBackend::new(140, 40)).expect("terminal");
        let started = Instant::now();
        term.draw(|f| draw(f, &mut app, started)).expect("draw");
        let screen = text(term.backend().buffer());
        for want in [
            "MVF",
            "inspect",
            "RUNNING",
            "cyc:12",
            "cam",
            "gate",
            "score",
            "sink",
            "x12",
            "!1",
            "r1",
            "W py.brightness",
            "model loaded",
            "=",
            ">",
        ] {
            assert!(screen.contains(want), "missing {want:?}:\n{screen}");
        }
        assert!(screen.is_ascii(), "ASCII only:\n{screen}");
    }

    #[test]
    fn a_graph_wider_than_the_terminal_scrolls_to_the_selection() {
        let mut app = app();
        let mut term = Terminal::new(TestBackend::new(60, 30)).expect("terminal");
        let started = Instant::now();
        app.follow = false;
        app.selected = app.nodes.iter().position(|n| n.id == "sink");
        term.draw(|f| draw(f, &mut app, started)).expect("draw");
        assert!(app.scroll.0 > 0, "scrolled right to reach the last layer");
        let screen = text(term.backend().buffer());
        assert!(screen.contains("sink"), "{screen}");
        assert!(
            screen.contains('<'),
            "a hint that there is more on the left"
        );
        // Too small a terminal gets a message, not a panic.
        let mut tiny = Terminal::new(TestBackend::new(30, 8)).expect("terminal");
        tiny.draw(|f| draw(f, &mut app, started)).expect("draw");
        assert!(text(tiny.backend().buffer()).contains("too small"));
    }

    #[test]
    fn the_detail_screen_shows_ports_stats_logs_and_the_preview() {
        let mut app = app();
        app.selected = app.nodes.iter().position(|n| n.id == "score");
        app.view = View::Detail;
        app.preview.node = Some("score".into());
        let png = mvf_management::preview::png(16, 8, 3, &[90; 16 * 8 * 3]);
        app.preview_message(&json!({"op": "preview", "format": "png", "cycle": 11, "data": mvf_management::preview::base64(&png)}));
        let mut term = Terminal::new(TestBackend::new(140, 44)).expect("terminal");
        term.draw(|f| draw(f, &mut app, Instant::now()))
            .expect("draw");
        let screen = text(term.backend().buffer());
        for want in [
            "# NODE",
            "score",
            "backend:worker",
            "in  frame: frame (data) <- cam.frame",
            "out class: classification",
            "active (executing now)",
            "restarts 1",
            "p50 1.5ms",
            "model loaded",
            "preview cycle 11 (16x8)",
        ] {
            assert!(screen.contains(want), "missing {want:?}:\n{screen}");
        }
        // An unsupported payload is said to be one.
        app.preview_message(&json!({"op": "preview", "format": "none", "reason": "a blob without a known image signature"}));
        term.draw(|f| draw(f, &mut app, Instant::now()))
            .expect("draw");
        assert!(
            text(term.backend().buffer())
                .contains("no image: a blob without a known image signature")
        );
    }
}
