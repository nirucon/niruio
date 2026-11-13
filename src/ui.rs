//! Drawing and the terminal loop.

use crate::app::{balance_text, short, App, Hit, Tab};
use crate::doctor::Severity;
use crate::meter::Level;
use crate::pw::{Kind, Node};
use crossterm::event::{self, Event};
use crossterm::{execute, terminal};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::io::Write;
use std::time::Instant;

pub fn run(tab: Tab) -> Result<(), String> {
    let mut app = App::new(tab, true);
    terminal::enable_raw_mode().map_err(|e| format!("terminal: {e}"))?;
    let mut out = std::io::stdout();
    execute!(
        out,
        terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture,
        crossterm::cursor::Hide
    )
    .map_err(|e| e.to_string())?;
    let restore = || {
        let mut out = std::io::stdout();
        let _ = out.write_all(crate::camera::kitty_delete().as_bytes());
        let _ = execute!(
            out,
            crossterm::event::DisableMouseCapture,
            terminal::LeaveAlternateScreen,
            crossterm::cursor::Show
        );
        let _ = terminal::disable_raw_mode();
    };
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut out = std::io::stdout();
        let _ = execute!(
            out,
            crossterm::event::DisableMouseCapture,
            terminal::LeaveAlternateScreen,
            crossterm::cursor::Show
        );
        let _ = terminal::disable_raw_mode();
        default_hook(info);
    }));
    let result = (|| -> Result<(), String> {
        let mut term =
            Terminal::new(CrosstermBackend::new(std::io::stdout())).map_err(|e| e.to_string())?;
        loop {
            app.tick();
            term.draw(|f| draw(f, &mut app))
                .map_err(|e| e.to_string())?;
            kitty_preview(&mut app, &mut term)?;
            if app.quit {
                return Ok(());
            }
            let wait = app.frame_interval();
            let deadline = Instant::now() + wait;
            while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                if !event::poll(left).map_err(|e| e.to_string())? {
                    break;
                }
                match event::read().map_err(|e| e.to_string())? {
                    Event::Key(k) if k.kind == event::KeyEventKind::Press => app.key(k),
                    Event::Mouse(m) => app.mouse(m),
                    Event::Resize(_, _) => {}
                    _ => continue,
                }
                break;
            }
        }
    })();
    app.shutdown();
    restore();
    result
}

/// Send the newest preview frame to kitty, at the preview area.
fn kitty_preview<B: Backend + Write>(app: &mut App, term: &mut Terminal<B>) -> Result<(), String> {
    if !app.kitty {
        return Ok(());
    }
    let show = app.preview.is_some() && app.tab == Tab::Cameras && app.popup.is_none() && !app.help;
    if !show {
        if app.kitty_visible {
            let b = term.backend_mut();
            let _ = b.write_all(crate::camera::kitty_delete().as_bytes());
            let _ = Write::flush(b);
            app.kitty_visible = false;
            app.kitty_seq = 0;
        }
        return Ok(());
    }
    let (Some(rect), Some(p)) = (app.preview_rect, app.preview.as_ref()) else {
        return Ok(());
    };
    let Some((seq, frame)) = p.frame() else {
        return Ok(());
    };
    if seq == app.kitty_seq {
        return Ok(());
    }
    let data = crate::camera::kitty_frame(&frame, p.width, p.height, rect.width, rect.height);
    let b = term.backend_mut();
    let _ = execute!(b, crossterm::cursor::MoveTo(rect.x, rect.y));
    let _ = b.write_all(data.as_bytes());
    let _ = Write::flush(b);
    app.kitty_seq = seq;
    app.kitty_visible = true;
    Ok(())
}

struct Palette {
    fg: Color,
    muted: Color,
    accent: Color,
    border: Color,
    good: Color,
    warn: Color,
    bad: Color,
}

fn palette(app: &App) -> Palette {
    let t = &app.theme;
    Palette {
        fg: t.fg(),
        muted: t.muted(),
        accent: t.accent(),
        border: t.border(),
        good: t.good(),
        warn: t.warn(),
        bad: t.bad(),
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    app.hits.clear();
    let p = palette(app);
    let bg = Style::default().bg(app.theme.bg()).fg(p.fg);
    f.buffer_mut().set_style(area, bg);
    if area.height < 8 || area.width < 40 {
        f.render_widget(
            Paragraph::new("NIRUIO needs at least 40x8 cells").style(Style::default().fg(p.warn)),
            area,
        );
        return;
    }
    let header = Rect::new(area.x, area.y, area.width, 1);
    let body = Rect::new(
        area.x,
        area.y + 2,
        area.width,
        area.height.saturating_sub(5),
    );
    let footer = Rect::new(area.x, area.y + area.height - 2, area.width, 2);
    draw_header(f.buffer_mut(), header, app, &p);
    app.preview_rect = None;
    match app.tab {
        Tab::Out | Tab::In | Tab::Apps => draw_devices(f.buffer_mut(), body, app, &p),
        Tab::Cameras => draw_cameras(f.buffer_mut(), body, app, &p),
        Tab::Cards => draw_cards(f.buffer_mut(), body, app, &p),
        Tab::Saved => draw_saved(f.buffer_mut(), body, app, &p),
        Tab::Doctor => draw_doctor(f.buffer_mut(), body, app, &p),
    }
    draw_footer(f.buffer_mut(), footer, app, &p);
    if app.popup.is_some() {
        draw_popup(f, area, app, &p);
    }
    if app.help {
        draw_help(f, area, app, &p);
    }
}

fn draw_header(buf: &mut Buffer, area: Rect, app: &mut App, p: &Palette) {
    let mut x = area.x + 1;
    buf.set_string(
        x,
        area.y,
        "NIRUIO",
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
    );
    x += 8;
    for (i, tab) in Tab::ALL.iter().enumerate() {
        let mut label = format!("{} {}", i + 1, tab.title());
        let mut style = Style::default().fg(p.muted);
        if *tab == Tab::Doctor {
            let n = app.problem_count();
            if n > 0 {
                label.push_str(&format!(" !{n}"));
                style = style.fg(p.warn);
            }
        }
        if *tab == app.tab {
            style = Style::default()
                .fg(p.accent)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        }
        let w = label.chars().count() as u16;
        if x + w >= area.x + area.width {
            break;
        }
        buf.set_string(x, area.y, &label, style);
        app.hits.push((Rect::new(x, area.y, w, 1), Hit::Tab(*tab)));
        x += w + 3;
    }
}

fn hints(app: &App) -> &'static str {
    if app.popup.is_some() {
        return "↑↓ choose  Enter apply  Esc close";
    }
    match app.tab {
        Tab::Out => {
            "←→ volume  m mute  d default  [ ] balance  c centre  p port  P profile  ? keys"
        }
        Tab::In => {
            "←→ gain  m mute  d default  t test  y play test  b/B boost  n niruclip  p port  ? keys"
        }
        Tab::Apps => "←→ volume  m mute  o move to device  [ ] balance  ? keys",
        Tab::Cameras if app.cam_controls => {
            "↑↓ control  ←→ change (Shift: more)  0 default  v preview  Esc list"
        }
        Tab::Cameras => "↑↓ camera  Enter controls  v preview  n use in NIRUCLIP  ? keys",
        Tab::Cards => "↑↓ card  Enter profile  o port  ? keys",
        Tab::Saved => "Enter/r reset to 100 % unmuted  R reset every silencing entry  ? keys",
        Tab::Doctor => "Enter/f apply the fix  F fix all  r check again  ? keys",
    }
}

fn draw_footer(buf: &mut Buffer, area: Rect, app: &App, p: &Palette) {
    buf.set_string(
        area.x + 1,
        area.y,
        clip(hints(app), area.width.saturating_sub(2) as usize),
        Style::default().fg(p.muted),
    );
    let (text, style) = if let Some(b) = &app.busy {
        (b.clone(), Style::default().fg(p.accent))
    } else if let Some((s, _, err)) = &app.status {
        (
            s.clone(),
            Style::default().fg(if *err { p.bad } else { p.good }),
        )
    } else if let Some(e) = &app.model_error {
        (format!("PipeWire: {e}"), Style::default().fg(p.bad))
    } else {
        (String::new(), Style::default())
    };
    buf.set_string(
        area.x + 1,
        area.y + 1,
        clip(&text, area.width.saturating_sub(2) as usize),
        style,
    );
}

fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_string()
    } else if width == 0 {
        String::new()
    } else {
        let mut s: String = text.chars().take(width - 1).collect();
        s.push('…');
        s
    }
}

/// A horizontal bar of `width` cells for `value` in 0..`max`, with a tick
/// at `mark`.
fn bar(value: f32, max: f32, width: usize, mark: Option<f32>) -> (String, String) {
    let filled = ((value / max).clamp(0.0, 1.0) * width as f32).round() as usize;
    let mark_at = mark.map(|m| ((m / max) * width as f32).round() as usize);
    let full: String = "█".repeat(filled);
    let empty: String = (filled..width)
        .map(|i| if Some(i) == mark_at { '│' } else { '░' })
        .collect();
    (full, empty)
}

fn meter_spans(level: Option<&Level>, width: usize, p: &Palette) -> Vec<Span<'static>> {
    let db = level.map(|l| l.max_db()).unwrap_or(-120.0);
    let hold = level
        .map(|l| l.hold.iter().copied().fold(-120.0, f32::max))
        .unwrap_or(-120.0);
    meter_bar(db, hold, level.is_some_and(|l| l.clipped), width, p)
}

/// -60 dB .. 0 dB, coloured by zone.
fn meter_bar(db: f32, hold: f32, clipped: bool, width: usize, p: &Palette) -> Vec<Span<'static>> {
    let pos = |d: f32| (((d + 60.0) / 60.0).clamp(0.0, 1.0) * width as f32).round() as usize;
    let lit = pos(db);
    let hold_at = pos(hold);
    let warn_from = pos(-12.0);
    let bad_from = pos(-3.0);
    let mut spans = Vec::new();
    for i in 0..width {
        let (ch, color) = if i < lit {
            let c = if i >= bad_from {
                p.bad
            } else if i >= warn_from {
                p.warn
            } else {
                p.accent
            };
            ('▮', c)
        } else if i + 1 == hold_at && hold > -59.0 {
            ('▮', if hold > -3.0 { p.bad } else { p.muted })
        } else {
            ('·', p.border)
        };
        spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
    }
    let text = if db <= -100.0 {
        "  silent".to_string()
    } else {
        format!(" {:>5.0} dB", db.round().min(0.0) + 0.0)
    };
    spans.push(Span::styled(
        text,
        Style::default().fg(if clipped { p.bad } else { p.muted }),
    ));
    if clipped {
        spans.push(Span::styled(" clip", Style::default().fg(p.bad)));
    }
    spans
}

struct Block2 {
    lines: Vec<Line<'static>>,
    /// Line index inside the block and x offset + width of its volume bar.
    volume: Option<(usize, u16, u16)>,
}

/// Lay out selectable blocks top to bottom, scrolled so the selected one is
/// visible. Returns the rows actually shown.
fn render_blocks(
    buf: &mut Buffer,
    area: Rect,
    blocks: &[Block2],
    selected: usize,
    hits: &mut Vec<(Rect, Hit)>,
    p: &Palette,
) -> usize {
    let heights: Vec<u16> = blocks.iter().map(|b| b.lines.len() as u16 + 1).collect();
    let mut start = 0;
    loop {
        let used: u16 = heights[start..=selected.min(blocks.len().saturating_sub(1))]
            .iter()
            .sum();
        if used <= area.height || start >= selected {
            break;
        }
        start += 1;
    }
    let mut y = area.y;
    let mut shown = 0;
    for (i, b) in blocks.iter().enumerate().skip(start) {
        if y >= area.y + area.height {
            break;
        }
        let h = (b.lines.len() as u16).min(area.y + area.height - y);
        let rect = Rect::new(area.x, y, area.width, h);
        hits.push((rect, Hit::Row(i)));
        if i == selected {
            for dy in 0..h {
                buf.set_string(area.x, y + dy, "▌", Style::default().fg(p.accent));
            }
        }
        for (j, line) in b.lines.iter().enumerate().take(h as usize) {
            buf.set_line(area.x + 2, y + j as u16, line, area.width.saturating_sub(3));
        }
        if let Some((line, x, w)) = b.volume {
            if (line as u16) < h {
                hits.push((
                    Rect::new(area.x + 2 + x, y + line as u16, w, 1),
                    Hit::Volume(i, area.x + 2 + x, w),
                ));
            }
        }
        y += h + 1;
        shown += 1;
    }
    if blocks.is_empty() {
        return 0;
    }
    if start > 0 {
        buf.set_string(
            area.x + area.width - 2,
            area.y,
            "▲",
            Style::default().fg(p.muted),
        );
    }
    if start + shown < blocks.len() {
        buf.set_string(
            area.x + area.width - 2,
            area.y + area.height - 1,
            "▼",
            Style::default().fg(p.muted),
        );
    }
    shown
}

fn device_block(app: &App, n: &Node, width: u16, p: &Palette) -> Block2 {
    let inner = width.saturating_sub(4) as usize;
    let mut l1: Vec<Span> = Vec::new();
    let is_default = app.model.is_default(n);
    if n.is_stream() {
        let arrow = if n.kind == Kind::Playback {
            "▶"
        } else {
            "●"
        };
        l1.push(Span::styled(
            format!("{arrow} "),
            Style::default().fg(if n.kind == Kind::Playback {
                p.accent
            } else {
                p.bad
            }),
        ));
        l1.push(Span::styled(
            n.program(),
            Style::default().fg(p.fg).add_modifier(Modifier::BOLD),
        ));
        let what = if !n.media_name.is_empty() && n.media_name != n.program() {
            format!("  {}", n.media_name)
        } else {
            String::new()
        };
        let role = if n.media_role.is_empty() {
            String::new()
        } else {
            format!("  role {}", n.media_role)
        };
        l1.push(Span::styled(
            clip(
                &format!("{what}{role}"),
                inner.saturating_sub(n.program().len() + 30),
            ),
            Style::default().fg(p.muted),
        ));
        let target = app
            .model
            .stream_targets(n)
            .first()
            .map(|t| short(&t.description))
            .unwrap_or_else(|| "not connected".into());
        let dir = if n.kind == Kind::Playback {
            "  → "
        } else {
            "  ← "
        };
        l1.push(Span::styled(
            format!("{dir}{target}"),
            Style::default().fg(p.muted),
        ));
    } else {
        l1.push(Span::styled(
            if is_default { "● " } else { "  " },
            Style::default().fg(if is_default { p.good } else { p.border }),
        ));
        let name_w = inner.saturating_sub(24);
        l1.push(Span::styled(
            clip(&n.description, name_w),
            Style::default().fg(p.fg).add_modifier(Modifier::BOLD),
        ));
        let mut tags = vec![n.connection().to_string()];
        if n.positions.len() > 2 {
            tags.push(format!("{} ch", n.positions.len()));
        }
        if is_default {
            tags.push("default".into());
        }
        l1.push(Span::styled(
            format!("  {}", tags.join(" · ")),
            Style::default().fg(p.muted),
        ));
    }
    let barw = inner.saturating_sub(34).clamp(10, 60);
    let v = n.volume();
    let (full, empty) = bar(v, crate::actions::MAX_VOLUME, barw, Some(1.0));
    let vol_color = if n.mute {
        p.muted
    } else if v > 1.0 {
        p.warn
    } else {
        p.accent
    };
    let label = if n.kind == Kind::Source {
        "Gain   "
    } else {
        "Volume "
    };
    let mut l2 = vec![
        Span::styled(format!("  {label}"), Style::default().fg(p.muted)),
        Span::styled(full, Style::default().fg(vol_color)),
        Span::styled(empty, Style::default().fg(p.border)),
        Span::styled(format!(" {:>4.0} %", v * 100.0), Style::default().fg(p.fg)),
    ];
    if n.mute {
        l2.push(Span::styled(
            "  MUTED",
            Style::default().fg(p.bad).add_modifier(Modifier::BOLD),
        ));
    }
    if n.stereo() && n.balance().abs() >= 0.005 {
        l2.push(Span::styled(
            format!("  balance {}", balance_text(n.balance())),
            Style::default().fg(p.muted),
        ));
    }
    let mut l3 = vec![Span::styled("  Level  ", Style::default().fg(p.muted))];
    if app.meters.disabled {
        l3.push(Span::styled("meters off", Style::default().fg(p.border)));
    } else {
        l3.extend(meter_spans(app.levels.get(&n.id), barw, p));
    }
    let mut lines = vec![Line::from(l1), Line::from(l2), Line::from(l3)];
    // Every input of a multichannel interface, for the selected one.
    let selected = app.selected_node().is_some_and(|s| s.id == n.id);
    if selected && n.kind == Kind::Source && n.positions.len() > 2 {
        if let Some(level) = app.levels.get(&n.id) {
            let cell = 15usize;
            let per_line = (inner.saturating_sub(4) / cell).max(1);
            for chunk in (0..level.db.len()).collect::<Vec<_>>().chunks(per_line) {
                let mut spans = vec![Span::raw("  ")];
                for &c in chunk {
                    spans.push(Span::styled(
                        format!("{:>3} ", c + 1),
                        Style::default().fg(p.muted),
                    ));
                    let m = meter_bar(level.db[c], level.hold[c], false, 8, p);
                    spans.extend(m.into_iter().take(8));
                    spans.push(Span::raw("  "));
                }
                lines.push(Line::from(spans));
            }
        }
    }
    Block2 {
        lines,
        volume: Some((1, 2 + label.len() as u16, barw as u16)),
    }
}

fn draw_devices(buf: &mut Buffer, area: Rect, app: &mut App, p: &Palette) {
    let nodes = match app.tab {
        Tab::Out => app.sinks(),
        Tab::In => app.sources(),
        _ => app.streams(),
    };
    let detail_h = if nodes.is_empty() { 0 } else { 4u16 };
    let list = Rect::new(
        area.x,
        area.y,
        area.width,
        area.height.saturating_sub(detail_h),
    );
    if nodes.is_empty() {
        let text = match app.tab {
            Tab::Out => "No outputs. Open the Doctor tab (7) to see why.",
            Tab::In => "No inputs. Open the Doctor tab (7) to see why.",
            _ => "No program plays or records sound right now.",
        };
        buf.set_string(area.x + 2, area.y, text, Style::default().fg(p.muted));
        return;
    }
    let blocks: Vec<Block2> = nodes
        .iter()
        .map(|n| device_block(app, n, area.width, p))
        .collect();
    let sel = app.selected();
    let mut hits = std::mem::take(&mut app.hits);
    let shown = render_blocks(buf, list, &blocks, sel, &mut hits, p);
    app.hits = hits;
    app.page = shown.max(1);
    // Details of the selected one.
    let Some(n) = nodes.get(sel).cloned() else {
        return;
    };
    let d = Rect::new(
        area.x,
        area.y + area.height - detail_h,
        area.width,
        detail_h,
    );
    for x in d.x + 1..d.x + d.width - 1 {
        buf.set_string(x, d.y, "─", Style::default().fg(p.border));
    }
    let mut lines: Vec<Line> = Vec::new();
    let dim = Style::default().fg(p.muted);
    let fg = Style::default().fg(p.fg);
    if n.is_stream() {
        let mut parts = vec![Span::styled("Program ", dim)];
        parts.push(Span::styled(
            format!(
                "{}{}",
                if n.app_binary.is_empty() {
                    n.program()
                } else {
                    n.app_binary.clone()
                },
                n.pid.map(|p| format!(" (pid {p})")).unwrap_or_default()
            ),
            fg,
        ));
        if let Some(key) = n.restore_key(None) {
            parts.push(Span::styled("   remembered as ", dim));
            parts.push(Span::styled(key.clone(), fg));
            if let Some(e) = app.saved.find(&key) {
                if e.silencing() {
                    parts.push(Span::styled(
                        "  (muted or silent: see Saved)",
                        Style::default().fg(p.bad),
                    ));
                }
            }
        }
        lines.push(Line::from(parts));
    } else {
        let mut parts = vec![
            Span::styled("Device ", dim),
            Span::styled(n.name.clone(), fg),
        ];
        if let Some(rate) = n.rate {
            parts.push(Span::styled(format!("  {rate} Hz"), dim));
        }
        if !n.codec.is_empty() {
            parts.push(Span::styled("  codec ", dim));
            parts.push(Span::styled(n.codec.to_uppercase(), fg));
        }
        lines.push(Line::from(parts));
        if let Some(dev) = n.device_id.and_then(|id| app.model.device(id)) {
            let mut parts = vec![
                Span::styled("Card ", dim),
                Span::styled(dev.description.clone(), fg),
            ];
            if let Some(pr) = dev.active_profile() {
                parts.push(Span::styled("  profile ", dim));
                parts.push(Span::styled(crate::pw::profile_label(pr), fg));
            }
            if let Some(r) = n.profile_device.and_then(|cd| dev.route_for(cd)) {
                parts.push(Span::styled("  port ", dim));
                parts.push(Span::styled(r.description.clone(), fg));
                if r.available == "no" {
                    parts.push(Span::styled(" (not connected)", Style::default().fg(p.bad)));
                }
            }
            lines.push(Line::from(parts));
        }
    }
    if app.tab == Tab::In {
        let mut parts: Vec<Span> = Vec::new();
        if let Some(card) = n.alsa_card {
            let controls = app.alsa_controls(card);
            if let Some(b) = crate::alsa::boosts(&controls).first() {
                parts.push(Span::styled("Boost ", dim));
                parts.push(Span::styled(
                    format!(
                        "{} {} of {}{}",
                        b.name,
                        b.value.unwrap_or(0),
                        b.max,
                        b.db.as_ref().map(|d| format!(" ({d})")).unwrap_or_default()
                    ),
                    fg,
                ));
                parts.push(Span::raw("   "));
            }
        }
        if let Some(t) = &app.mictest {
            parts.push(Span::styled(
                format!("Recording… {} s left, speak now", t.remaining()),
                Style::default().fg(p.warn),
            ));
        } else {
            match &app.mic_result {
                Some(Ok(r)) if r.source == n.id => {
                    let (ch, peak) = r.loudest();
                    let clips: usize = r.clipped.iter().sum();
                    parts.push(Span::styled("Test ", dim));
                    parts.push(Span::styled(
                        format!(
                            "{}: peak {peak:.1} dBFS, RMS {:.1} dBFS{}{}  (y plays it)",
                            r.verdict(),
                            r.rms_db.get(ch).copied().unwrap_or(-120.0),
                            if r.channels > 2 {
                                format!(", loudest input {}", ch + 1)
                            } else {
                                String::new()
                            },
                            if clips > 0 {
                                format!(", {clips} clipped samples")
                            } else {
                                String::new()
                            }
                        ),
                        Style::default().fg(if clips > 0 || peak < -50.0 {
                            p.bad
                        } else {
                            p.good
                        }),
                    ));
                }
                Some(Err(e)) => parts.push(Span::styled(
                    format!("Test failed: {e}"),
                    Style::default().fg(p.bad),
                )),
                _ => parts.push(Span::styled("t records a 5 s test, y plays it back", dim)),
            }
        }
        lines.push(Line::from(parts));
    }
    for (i, line) in lines.iter().enumerate().take(3) {
        buf.set_line(d.x + 2, d.y + 1 + i as u16, line, d.width.saturating_sub(4));
    }
}

fn draw_cameras(buf: &mut Buffer, area: Rect, app: &mut App, p: &Palette) {
    let dim = Style::default().fg(p.muted);
    let fg = Style::default().fg(p.fg);
    if app.cameras.is_empty() {
        buf.set_string(
            area.x + 2,
            area.y,
            "No camera found. The Doctor tab (7) explains why.",
            dim,
        );
        return;
    }
    let left_w = (area.width / 3).clamp(24, 40);
    let left = Rect::new(area.x, area.y, left_w, area.height);
    let right = Rect::new(
        area.x + left_w + 1,
        area.y,
        area.width - left_w - 1,
        area.height,
    );
    let clip_cam = crate::camera::niruclip_get("camera");
    let blocks: Vec<Block2> = app
        .cameras
        .iter()
        .map(|c| {
            let mut l1 = vec![Span::styled(
                clip(&c.name, left_w as usize - 4),
                Style::default().fg(p.fg).add_modifier(Modifier::BOLD),
            )];
            if c.infrared {
                l1.push(Span::styled(" IR", dim));
            }
            let mut l2 = vec![Span::styled(c.path.clone(), dim)];
            if clip_cam.as_deref() == Some(c.stable.as_str())
                || clip_cam.as_deref() == Some(c.path.as_str())
            {
                l2.push(Span::styled("  NIRUCLIP", Style::default().fg(p.good)));
            }
            let l3 = if !c.readable {
                Line::from(Span::styled("no access", Style::default().fg(p.bad)))
            } else if c.users.is_empty() {
                Line::from(Span::styled("free", Style::default().fg(p.good)))
            } else {
                Line::from(Span::styled(
                    format!(
                        "in use: {}",
                        c.users
                            .iter()
                            .map(|(_, n)| n.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    Style::default().fg(p.warn),
                ))
            };
            Block2 {
                lines: vec![Line::from(l1), Line::from(l2), l3],
                volume: None,
            }
        })
        .collect();
    let sel = app.sel[Tab::Cameras.index()].min(app.cameras.len() - 1);
    let mut hits = std::mem::take(&mut app.hits);
    render_blocks(buf, left, &blocks, sel, &mut hits, p);
    for y in area.y..area.y + area.height {
        buf.set_string(area.x + left_w, y, "│", Style::default().fg(p.border));
    }
    let cam = app.cameras[sel].clone();
    let mut y = right.y;
    let put = |buf: &mut Buffer, y: &mut u16, line: Line| {
        if *y < right.y + right.height {
            buf.set_line(right.x + 1, *y, &line, right.width.saturating_sub(2));
            *y += 1;
        }
    };
    put(
        buf,
        &mut y,
        Line::from(Span::styled(
            cam.name.clone(),
            Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
        )),
    );
    put(
        buf,
        &mut y,
        Line::from(vec![
            Span::styled("Path ", dim),
            Span::styled(cam.stable.clone(), fg),
        ]),
    );
    let details = app.details.get(&cam.path).cloned();
    if let Some(d) = &details {
        put(
            buf,
            &mut y,
            Line::from(vec![
                Span::styled("Driver ", dim),
                Span::styled(format!("{}  {}", d.driver, d.bus), fg),
            ]),
        );
        for f in d.formats.iter().take(3) {
            put(
                buf,
                &mut y,
                Line::from(vec![
                    Span::styled("Format ", dim),
                    Span::styled(f.summary(), fg),
                ]),
            );
        }
    } else if !cam.readable {
        put(
            buf,
            &mut y,
            Line::from(Span::styled(
                "You have no access to this camera (the video group).",
                Style::default().fg(p.bad),
            )),
        );
    }
    if !cam.users.is_empty() {
        put(
            buf,
            &mut y,
            Line::from(vec![
                Span::styled("Used by ", dim),
                Span::styled(
                    cam.users
                        .iter()
                        .map(|(pid, n)| format!("{n} ({pid})"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    Style::default().fg(p.warn),
                ),
            ]),
        );
    }
    y += 1;
    let rest = Rect::new(
        right.x,
        y,
        right.width,
        (right.y + right.height).saturating_sub(y),
    );
    if rest.height < 3 {
        app.hits = hits;
        return;
    }
    let side = rest.width >= 80;
    let (ctrl_area, prev_area) = if side {
        let cw = 44.min(rest.width / 2);
        (
            Rect::new(rest.x, rest.y, cw, rest.height),
            Rect::new(rest.x + cw + 1, rest.y, rest.width - cw - 1, rest.height),
        )
    } else {
        let ch = rest.height / 2;
        (
            Rect::new(rest.x, rest.y, rest.width, ch),
            Rect::new(rest.x, rest.y + ch, rest.width, rest.height - ch),
        )
    };
    // Controls.
    if let Some(d) = &details {
        let title_style = if app.cam_controls {
            Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
        } else {
            dim
        };
        buf.set_string(ctrl_area.x + 1, ctrl_area.y, "Controls", title_style);
        let rows = ctrl_area.height.saturating_sub(1) as usize;
        let start = app.ctrl_sel.saturating_sub(rows.saturating_sub(1));
        for (i, c) in d.controls.iter().enumerate().skip(start).take(rows) {
            let yy = ctrl_area.y + 1 + (i - start) as u16;
            let selected = app.cam_controls && i == app.ctrl_sel;
            let marker = if selected { "▌" } else { " " };
            buf.set_string(ctrl_area.x, yy, marker, Style::default().fg(p.accent));
            let name_w = (ctrl_area.width as usize).saturating_sub(16).min(28);
            let style = if c.inactive {
                Style::default().fg(p.border)
            } else {
                fg
            };
            buf.set_string(ctrl_area.x + 2, yy, clip(&c.label(), name_w), style);
            let value = if c.kind == "int" && c.max > c.min {
                let at = ((c.value - c.min) as f32 / (c.max - c.min) as f32 * 7.0).round() as usize;
                let slider: String = (0..8)
                    .map(|i| if i == at.min(7) { '●' } else { '─' })
                    .collect();
                format!("{slider} {}", c.value_text())
            } else {
                c.value_text()
            };
            buf.set_string(
                ctrl_area.x + 3 + name_w as u16,
                yy,
                clip(
                    &value,
                    (ctrl_area.width as usize).saturating_sub(name_w + 4),
                ),
                if selected {
                    Style::default().fg(p.accent)
                } else {
                    style
                },
            );
            hits.push((
                Rect::new(ctrl_area.x, yy, ctrl_area.width, 1),
                Hit::Control(i),
            ));
        }
    }
    // Preview.
    let pv = Rect::new(
        prev_area.x + 1,
        prev_area.y + 1,
        prev_area.width.saturating_sub(2),
        prev_area.height.saturating_sub(1),
    );
    buf.set_string(prev_area.x + 1, prev_area.y, "Preview", dim);
    app.hits = hits;
    if !app.preview_wanted {
        buf.set_string(
            pv.x,
            pv.y,
            "v starts a live preview; the camera stays closed until then.",
            dim,
        );
        return;
    }
    if !cam.readable {
        return;
    }
    app.preview_rect = Some(pv);
    // Clear the area: kitty draws the picture on top of it.
    for yy in pv.y..pv.y + pv.height {
        for xx in pv.x..pv.x + pv.width {
            buf[(xx, yy)].set_symbol(" ");
        }
    }
    if app.kitty {
        if app.preview.as_ref().and_then(|p| p.frame()).is_none() {
            buf.set_string(pv.x, pv.y, "Opening the camera…", dim);
        }
        return;
    }
    // Half blocks elsewhere: two pixels per cell.
    if let Some(prev) = &app.preview {
        if let Some((_, frame)) = prev.frame() {
            let (w, h) = (prev.width as usize, prev.height as usize);
            for cy in 0..pv.height as usize {
                for cx in 0..(pv.width as usize).min(w) {
                    let top = 2 * cy;
                    let bot = top + 1;
                    if bot >= h {
                        continue;
                    }
                    let px = |row: usize| {
                        let i = (row * w + cx) * 3;
                        Color::Rgb(frame[i], frame[i + 1], frame[i + 2])
                    };
                    let cell = &mut buf[(pv.x + cx as u16, pv.y + cy as u16)];
                    cell.set_symbol("▀");
                    cell.set_fg(px(top));
                    cell.set_bg(px(bot));
                }
            }
        } else {
            buf.set_string(pv.x, pv.y, "Opening the camera…", dim);
        }
    }
}

fn draw_cards(buf: &mut Buffer, area: Rect, app: &mut App, p: &Palette) {
    let dim = Style::default().fg(p.muted);
    let fg = Style::default().fg(p.fg);
    let devices = app.model.devices.clone();
    if devices.is_empty() {
        buf.set_string(
            area.x + 2,
            area.y,
            "No sound cards. The Doctor tab (7) explains why.",
            dim,
        );
        return;
    }
    let left_w = (area.width / 3).clamp(26, 44);
    let left = Rect::new(area.x, area.y, left_w, area.height);
    let blocks: Vec<Block2> = devices
        .iter()
        .map(|d| {
            let kind = if d.bluetooth() {
                "Bluetooth"
            } else if d.bus == "usb" {
                "USB"
            } else {
                "Built-in"
            };
            let profile = d
                .active_profile()
                .map(|p| p.description.clone())
                .unwrap_or_else(|| "no profile".into());
            Block2 {
                lines: vec![
                    Line::from(Span::styled(
                        clip(&d.description, left_w as usize - 4),
                        Style::default().fg(p.fg).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(Span::styled(
                        clip(&format!("{kind} · {profile}"), left_w as usize - 4),
                        dim,
                    )),
                ],
                volume: None,
            }
        })
        .collect();
    let sel = app.selected();
    let mut hits = std::mem::take(&mut app.hits);
    render_blocks(buf, left, &blocks, sel, &mut hits, p);
    app.hits = hits;
    for y in area.y..area.y + area.height {
        buf.set_string(area.x + left_w, y, "│", Style::default().fg(p.border));
    }
    let d = &devices[sel];
    let rx = area.x + left_w + 2;
    let rw = area.width.saturating_sub(left_w + 3);
    let mut y = area.y;
    let mut put = |line: Line, buf: &mut Buffer| {
        if y < area.y + area.height {
            buf.set_line(rx, y, &line, rw);
            y += 1;
        }
    };
    put(
        Line::from(Span::styled(
            d.description.clone(),
            Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
        )),
        buf,
    );
    put(
        Line::from(vec![
            Span::styled("Name ", dim),
            Span::styled(d.name.clone(), fg),
        ]),
        buf,
    );
    if d.bluetooth() {
        let codec = app
            .model
            .nodes
            .iter()
            .find(|n| n.device_id == Some(d.id) && !n.codec.is_empty())
            .map(|n| n.codec.to_uppercase());
        put(
            Line::from(vec![
                Span::styled("Codec ", dim),
                Span::styled(codec.unwrap_or_else(|| "not reported".into()), fg),
                Span::styled("   pairing and connecting: NIRU Launcher, Bluetooth", dim),
            ]),
            buf,
        );
    }
    put(Line::from(""), buf);
    put(
        Line::from(Span::styled("Profiles (Enter to choose)", dim)),
        buf,
    );
    for pr in &d.profiles {
        let current = Some(pr.index) == d.profile;
        let style = if current {
            Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
        } else if pr.available == "no" {
            Style::default().fg(p.border)
        } else {
            fg
        };
        put(
            Line::from(vec![
                Span::styled(
                    if current { " ● " } else { "   " },
                    Style::default().fg(p.good),
                ),
                Span::styled(crate::pw::profile_label(pr), style),
                Span::styled(
                    if pr.available == "no" {
                        "  unavailable".to_string()
                    } else {
                        String::new()
                    },
                    dim,
                ),
            ]),
            buf,
        );
    }
    put(Line::from(""), buf);
    put(Line::from(Span::styled("Ports (o to choose)", dim)), buf);
    for r in &d.routes {
        let active = d.active_routes.iter().any(|a| a.index == r.index);
        let avail = match r.available.as_str() {
            "yes" => "connected",
            "no" => "not connected",
            _ => "",
        };
        put(
            Line::from(vec![
                Span::styled(
                    if active { " ● " } else { "   " },
                    Style::default().fg(p.good),
                ),
                Span::styled(
                    format!("{} {}", if r.output { "out" } else { "in " }, r.description),
                    if active {
                        Style::default().fg(p.accent)
                    } else {
                        fg
                    },
                ),
                Span::styled(
                    format!("  {avail}"),
                    if r.available == "no" {
                        Style::default().fg(p.border)
                    } else {
                        dim
                    },
                ),
            ]),
            buf,
        );
    }
}

fn draw_saved(buf: &mut Buffer, area: Rect, app: &mut App, p: &Palette) {
    let dim = Style::default().fg(p.muted);
    let fg = Style::default().fg(p.fg);
    let file = app
        .saved
        .stream_file
        .as_ref()
        .map(|f| f.display().to_string())
        .unwrap_or_else(|| format!("nothing stored in {}", app.saved.dir.display()));
    buf.set_line(
        area.x + 2,
        area.y,
        &Line::from(vec![
            Span::styled(
                "WirePlumber remembers volume and mute per role and program in ",
                dim,
            ),
            Span::styled(file, fg),
        ]),
        area.width.saturating_sub(3),
    );
    let defaults: Vec<String> = app
        .saved
        .default_nodes
        .iter()
        .filter(|(k, _)| {
            !k.ends_with(".0") && !k.contains(".audio.sink.") && !k.contains(".audio.source.")
        })
        .map(|(k, v)| {
            format!(
                "{} {}",
                if k.contains("sink") {
                    "output"
                } else if k.contains("source") {
                    "input"
                } else {
                    k
                },
                v
            )
        })
        .collect();
    let extra = if defaults.is_empty() { 0 } else { 2 };
    let list = Rect::new(
        area.x,
        area.y + 2,
        area.width,
        area.height.saturating_sub(2 + extra),
    );
    let entries = app.saved.entries.clone();
    if entries.is_empty() {
        buf.set_string(area.x + 2, area.y + 2, "No remembered entries.", dim);
    }
    let blocks: Vec<Block2> = entries
        .iter()
        .map(|e| {
            let level = e.level();
            let silencing = e.silencing();
            let state_style = if silencing {
                Style::default().fg(p.bad).add_modifier(Modifier::BOLD)
            } else {
                dim
            };
            let mut spans = vec![
                Span::styled(format!("{:<4}", if e.output() { "out" } else { "in" }), dim),
                Span::styled(
                    format!("{:<6}", if e.mute == Some(true) { "muted" } else { "" }),
                    state_style,
                ),
                Span::styled(
                    format!(
                        "{:>6}  ",
                        level
                            .map(|l| format!("{:.0} %", l * 100.0))
                            .unwrap_or_default()
                    ),
                    if level.is_some_and(|l| l < 0.05) {
                        state_style
                    } else {
                        fg
                    },
                ),
                Span::styled(e.label(), if e.is_stream() { fg } else { dim }),
            ];
            if silencing {
                spans.push(Span::styled("  silences it", Style::default().fg(p.bad)));
            }
            if let Some(t) = &e.target {
                spans.push(Span::styled(format!("  → {t}"), dim));
            }
            Block2 {
                lines: vec![Line::from(spans)],
                volume: None,
            }
        })
        .collect();
    // One line per entry: no blank line between them.
    let sel = app.selected();
    let rows = list.height as usize;
    let start = sel.saturating_sub(rows.saturating_sub(1));
    for (i, b) in blocks.iter().enumerate().skip(start).take(rows) {
        let y = list.y + (i - start) as u16;
        if i == sel {
            buf.set_string(list.x, y, "▌", Style::default().fg(p.accent));
        }
        buf.set_line(list.x + 2, y, &b.lines[0], list.width.saturating_sub(3));
        app.hits
            .push((Rect::new(list.x, y, list.width, 1), Hit::Row(i)));
    }
    app.page = rows.max(1);
    if !defaults.is_empty() {
        let y = area.y + area.height - 1;
        buf.set_line(
            area.x + 2,
            y,
            &Line::from(vec![
                Span::styled("Chosen defaults: ", dim),
                Span::styled(defaults.join("   "), fg),
            ]),
            area.width.saturating_sub(3),
        );
    }
}

fn draw_doctor(buf: &mut Buffer, area: Rect, app: &mut App, p: &Palette) {
    let width = area.width.saturating_sub(14) as usize;
    let blocks: Vec<Block2> = app
        .findings
        .iter()
        .map(|f| {
            let (word, color) = match f.severity {
                Severity::Problem => ("PROBLEM", p.bad),
                Severity::Warning => ("WARNING", p.warn),
                Severity::Info => ("INFO", p.muted),
                Severity::Ok => ("OK", p.good),
            };
            let mut lines = vec![Line::from(vec![
                Span::styled(
                    format!("{word:<9}"),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    f.title.clone(),
                    Style::default().fg(p.fg).add_modifier(Modifier::BOLD),
                ),
            ])];
            for chunk in wrap(&f.detail, width) {
                lines.push(Line::from(vec![
                    Span::raw("         "),
                    Span::styled(chunk, Style::default().fg(p.muted)),
                ]));
            }
            if let Some(fix) = &f.fix_text {
                lines.push(Line::from(vec![
                    Span::raw("         "),
                    Span::styled("fix: ", Style::default().fg(p.accent)),
                    Span::styled(fix.clone(), Style::default().fg(p.fg)),
                ]));
            }
            Block2 {
                lines,
                volume: None,
            }
        })
        .collect();
    let sel = app.selected();
    let mut hits = std::mem::take(&mut app.hits);
    let shown = render_blocks(buf, area, &blocks, sel, &mut hits, p);
    app.hits = hits;
    app.page = shown.max(1);
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width.max(10) {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

fn draw_popup(f: &mut Frame, area: Rect, app: &mut App, p: &Palette) {
    let Some(pop) = app.popup.clone() else { return };
    let w = pop
        .items
        .iter()
        .map(|i| i.0.chars().count())
        .max()
        .unwrap_or(10)
        .max(pop.title.chars().count())
        .clamp(20, 90) as u16
        + 8;
    let h = (pop.items.len() as u16 + 2).min(area.height.saturating_sub(4));
    let r = centered(area, w, h);
    f.render_widget(Clear, r);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.accent))
        .style(Style::default().bg(app.theme.bg()).fg(p.fg))
        .title(Span::styled(
            format!(" {} ", pop.title),
            Style::default().fg(p.accent),
        ));
    let inner = block.inner(r);
    f.render_widget(block, r);
    let rows = inner.height as usize;
    let start = pop.sel.saturating_sub(rows.saturating_sub(1));
    for (i, (label, _, current)) in pop.items.iter().enumerate().skip(start).take(rows) {
        let y = inner.y + (i - start) as u16;
        let selected = i == pop.sel;
        let style = if selected {
            Style::default().fg(app.theme.bg()).bg(p.accent)
        } else {
            Style::default().fg(p.fg)
        };
        let text = format!("{} {}", if *current { "●" } else { " " }, label);
        let padded = format!(
            "{:<width$}",
            clip(&text, inner.width as usize),
            width = inner.width as usize
        );
        f.buffer_mut().set_string(inner.x, y, padded, style);
        app.hits
            .push((Rect::new(inner.x, y, inner.width, 1), Hit::PopupItem(i)));
    }
}

const HELP_TEXT: &[(&str, &str)] = &[
    ("Tab / Shift+Tab, 1-7", "switch tab (mouse: click a tab)"),
    ("↑ ↓  j k, PgUp PgDn", "select (mouse: click, wheel)"),
    (
        "← →  h l  - +",
        "volume or gain 5 % (Shift: 1 %), up to 150 %",
    ),
    ("m", "mute or unmute (mouse: right click)"),
    ("d / Enter", "make the default output or input"),
    ("[  ]  c", "balance left, right, centre"),
    ("p / P", "port (speaker, headphones...) / card profile"),
    ("t  y", "In: record a 5 s test / play it back"),
    ("b  B", "In: ALSA microphone boost down / up"),
    (
        "n",
        "In: inputs NIRUCLIP records; Cameras: NIRUCLIP's camera",
    ),
    ("o / Enter", "Apps: move the program to another device"),
    (
        "Enter, v, 0",
        "Cameras: controls, live preview, control default",
    ),
    (
        "Enter / o",
        "Cards: profile (hi-fi or headset for Bluetooth) / port",
    ),
    (
        "Enter, R",
        "Saved: reset to 100 % unmuted, reset all silencing",
    ),
    ("Enter, F, r", "Doctor: apply fix, fix all, check again"),
    ("q / Esc", "quit"),
];

fn draw_help(f: &mut Frame, area: Rect, app: &App, p: &Palette) {
    let r = centered(area, 84, HELP_TEXT.len() as u16 + 4);
    f.render_widget(Clear, r);
    let mut lines: Vec<Line> = HELP_TEXT
        .iter()
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!("{k:<24}"), Style::default().fg(p.accent)),
                Span::styled(v.to_string(), Style::default().fg(p.fg)),
            ])
        })
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "niruio {}  ·  niruio --help for the command line",
            crate::VERSION
        ),
        Style::default().fg(p.muted),
    )));
    let w = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(p.accent))
                .title(Span::styled(" Keys ", Style::default().fg(p.accent))),
        )
        .style(Style::default().bg(app.theme.bg()));
    f.render_widget(w, r);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn screen(app: &mut App, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        let mut s = String::new();
        for y in 0..h {
            for x in 0..w {
                s.push_str(buf[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    fn fixture_app(tab: Tab) -> App {
        // The fixture is the only graph source; meters stay off.
        std::env::set_var(
            "NIRUIO_PW_DUMP",
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/laptop.json"),
        );
        App::new(tab, false)
    }

    #[test]
    fn every_tab_renders() {
        for tab in Tab::ALL {
            let mut app = fixture_app(tab);
            for (w, h) in [(120, 40), (80, 24), (40, 8)] {
                let s = screen(&mut app, w, h);
                assert!(s.contains("NIRUIO"), "{tab:?} {w}x{h}\n{s}");
            }
        }
    }

    #[test]
    fn outputs_and_apps_show_state() {
        let mut app = fixture_app(Tab::Out);
        let s = screen(&mut app, 120, 40);
        assert!(s.contains("Speaker"), "{s}");
        assert!(s.contains("40 %"), "{s}");
        assert!(s.contains("MUTED"), "HDMI is muted\n{s}");
        assert!(s.contains("default"), "{s}");
        let mut app = fixture_app(Tab::Apps);
        let s = screen(&mut app, 120, 40);
        assert!(s.contains("mpv"), "{s}");
        assert!(s.contains("role Movie"), "{s}");
        assert!(s.contains("Microsoft Teams"), "{s}");
    }

    #[test]
    fn keys_record_commands() {
        let mut app = fixture_app(Tab::Out);
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let k = |c| KeyEvent::new(c, KeyModifiers::NONE);
        let lines = crate::actions::record(|| {
            app.key(k(KeyCode::Right));
            app.key(k(KeyCode::Char('m')));
            app.key(k(KeyCode::Down));
            app.key(k(KeyCode::Char('d')));
        });
        assert_eq!(lines[0], "wpctl set-volume 60 0.450");
        assert_eq!(lines[1], "wpctl set-mute 60 1");
        assert_eq!(lines[2], "wpctl set-default 61");
        // Apps: move mpv to HDMI through the popup.
        app.enter_tab(Tab::Apps);
        let lines = crate::actions::record(|| {
            app.key(k(KeyCode::Char('o')));
            assert!(app.popup.is_some());
            app.key(k(KeyCode::Down));
            app.key(k(KeyCode::Enter));
        });
        assert_eq!(
            lines,
            vec!["pw-metadata -n default 71 target.object 61 Spa:Id"]
        );
        let s = screen(&mut app, 100, 30);
        assert!(s.contains("Apps"));
    }

    #[test]
    fn mouse_clicks_tabs() {
        let mut app = fixture_app(Tab::Out);
        let _ = screen(&mut app, 120, 40);
        let (rect, _) = *app
            .hits
            .iter()
            .find(|(_, h)| *h == Hit::Tab(Tab::Cards))
            .unwrap();
        app.mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });
        assert_eq!(app.tab, Tab::Cards);
        let s = screen(&mut app, 120, 40);
        assert!(s.contains("Profiles"), "{s}");
        assert!(s.contains("Analog Stereo Duplex"), "{s}");
    }

    #[test]
    fn meter_bar_zones() {
        let p = Palette {
            fg: Color::White,
            muted: Color::Gray,
            accent: Color::Green,
            border: Color::DarkGray,
            good: Color::Green,
            warn: Color::Yellow,
            bad: Color::Red,
        };
        let spans = meter_bar(0.0, 0.0, true, 20, &p);
        assert_eq!(spans[19].style.fg, Some(Color::Red));
        assert_eq!(spans[0].style.fg, Some(Color::Green));
        assert!(spans.iter().any(|s| s.content.contains("clip")));
        let spans = meter_bar(-120.0, -120.0, false, 10, &p);
        assert!(spans[10].content.contains("silent"));
    }
}
