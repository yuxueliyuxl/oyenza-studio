//! Focus page: a Pomodoro timer.
//!
//! Left: a large progress ring in the phase colour with the countdown inside
//! (and the wall-clock end time while running). Right: a neutral segmented
//! control to pick the phase while idle (a plain status line while a session
//! runs), Start/Pause/Resume as the page's one primary action plus Reset and
//! Skip, and today's completed sessions with the long-break cycle as dots.
//!
//! The countdown repaints once per second through the app's wake timer; the
//! only continuous animations here are a short ring "rewind" when the phase is
//! reset, skipped or switched, and the segmented control's selection gliding.

use crate::gfx::{ts, TextStyle, Weight};
use crate::icons::Icon;
use crate::state::{AppState, InlineAction, NoticeAction};
use crate::ui::{wid, wid_n, PillStyle, Ui, PILL_GAP, PILL_REGULAR, PILL_SMALL};
use poni_core::datetime::{fmt_clock, fmt_countdown, LocalDateTime};
use poni_core::geom::{lerp, palette, Color, Point, Rect};
use poni_core::pomodoro::{Phase, PhaseEnded, RunState};
use std::cell::Cell;

const HEIGHT: f32 = 144.0;
const RING_R: f32 = 54.0;
const RING_W: f32 = 7.0;
/// The neutral track under every progress ring.
const RING_TRACK: Color = Color::white(0.14);
/// Gap between the ring and the controls column.
const COLUMN_GAP: f32 = 30.0;
/// Phase picker / status line, then the buttons, then the stats line.
const CHIP_H: f32 = PILL_SMALL;
const BUTTON_H: f32 = PILL_REGULAR;
const STATS_H: f32 = 16.0;
/// Vertical gap between the rows of the controls column.
const ROW_GAP: f32 = 14.0;
/// The primary capsule never gets narrower than this (it doesn't resize between Start/Pause/Resume).
const TOGGLE_MIN_W: f32 = 112.0;
/// Segmented control: selection inset inside the track, label padding, name→minutes gap.
const SEG_INSET: f32 = 2.0;
const SEG_PAD: f32 = 12.0;
const SEG_NUM_GAP: f32 = 5.0;
/// Duration of the selection glide in the segmented control.
const SEG_GLIDE_S: f64 = 0.22;
/// Duration of the ring's rewind / colour change after a reset, skip or phase switch.
const RING_TWEEN_S: f64 = 0.45;
/// When the page hasn't been drawn for this long, show the current state without tweening.
const STALE_S: f64 = 1.0;
/// Keep this many days of focus statistics.
const KEEP_STATS_DAYS: i64 = 366;

pub fn phase_color(p: Phase) -> Color {
    match p {
        Phase::Focus => palette::ORANGE,
        Phase::ShortBreak | Phase::LongBreak => palette::GREEN,
    }
}

/// What the ring showed last frame, so a backwards jump (reset, skip, a
/// finished phase) or a colour change can be tweened instead of snapping.
#[derive(Clone, Copy)]
struct RingTween {
    last_draw: f64,
    last_target: f32,
    last_color: Color,
    shown: f32,
    shown_color: Color,
    from: f32,
    from_color: Color,
    start: f64,
}

impl RingTween {
    const fn new() -> Self {
        Self {
            last_draw: f64::NEG_INFINITY,
            last_target: 0.0,
            last_color: palette::ORANGE,
            shown: 0.0,
            shown_color: palette::ORANGE,
            from: 0.0,
            from_color: palette::ORANGE,
            start: f64::NEG_INFINITY,
        }
    }
}

/// The segmented control's selection (x, width), gliding between segments.
#[derive(Clone, Copy)]
struct SegTween {
    last_draw: f64,
    target: (f32, f32),
    from: (f32, f32),
    shown: (f32, f32),
    start: f64,
}

impl SegTween {
    const fn new() -> Self {
        Self { last_draw: f64::NEG_INFINITY, target: (0.0, 0.0), from: (0.0, 0.0), shown: (0.0, 0.0), start: f64::NEG_INFINITY }
    }
}

thread_local! {
    static RING: Cell<RingTween> = const { Cell::new(RingTween::new()) };
    static SEG: Cell<SegTween> = const { Cell::new(SegTween::new()) };
    /// (end of the running phase, its wall-clock minute of the day) for "until HH:MM".
    static END_MINUTE: Cell<(f64, i64)> = const { Cell::new((f64::NAN, 0)) };
}

fn ease_out_cubic(t: f32) -> f32 {
    let u = 1.0 - t.clamp(0.0, 1.0);
    1.0 - u * u * u
}

/// Progress and colour to draw this frame, plus whether a tween is running.
fn ring_state(now: f64, target: f32, color: Color) -> (f32, Color, bool) {
    let mut a = RING.with(Cell::get);
    if now - a.last_draw > STALE_S {
        a.start = f64::NEG_INFINITY;
    } else if target + 0.01 < a.last_target || color != a.last_color {
        a.from = a.shown;
        a.from_color = a.shown_color;
        a.start = now;
    }
    let t = ((now - a.start) / RING_TWEEN_S).clamp(0.0, 1.0) as f32;
    let (p, c, moving) = if t < 1.0 {
        let e = ease_out_cubic(t);
        (lerp(a.from, target, e), Color::lerp(a.from_color, color, e), true)
    } else {
        (target, color, false)
    };
    a.last_draw = now;
    a.last_target = target;
    a.last_color = color;
    a.shown = p;
    a.shown_color = c;
    RING.with(|r| r.set(a));
    (p, c, moving)
}

/// Selection (x, width) to draw this frame, plus whether it is still gliding.
fn seg_state(now: f64, target: (f32, f32)) -> ((f32, f32), bool) {
    let mut s = SEG.with(Cell::get);
    if now - s.last_draw > STALE_S {
        s.start = f64::NEG_INFINITY;
    } else if target != s.target {
        s.from = s.shown;
        s.start = now;
    }
    let t = ((now - s.start) / SEG_GLIDE_S).clamp(0.0, 1.0) as f32;
    let (shown, moving) = if t < 1.0 {
        let e = ease_out_cubic(t);
        ((lerp(s.from.0, target.0, e), lerp(s.from.1, target.1, e)), true)
    } else {
        (target, false)
    };
    s.last_draw = now;
    s.target = target;
    s.shown = shown;
    SEG.with(|c| c.set(s));
    (shown, moving)
}

pub fn height(_st: &AppState) -> f32 {
    HEIGHT
}

pub fn draw(ui: &mut Ui, st: &mut AppState, area: Rect) {
    let now = st.now;
    let phase = st.pomodoro.phase;
    let color = phase_color(phase);
    let running = st.pomodoro.is_running();
    let active = st.pomodoro.is_active();
    let paused = active && !running;

    // ---- Ring with the countdown (its outer edge sits on the page's left edge).
    let rc = Point::new(area.x + RING_R + RING_W / 2.0, area.y + HEIGHT / 2.0);
    let (progress, ring_color, tweening) = ring_state(now, st.pomodoro.progress(now), color);
    if tweening {
        ui.request_anim();
    }
    let arc_color = if paused { ring_color.mul_alpha(0.55) } else { ring_color };
    ui.c.ring(rc, RING_R, RING_W, progress, RING_TRACK, arc_color);

    let remaining = st.pomodoro.remaining(now);
    let countdown = fmt_countdown(remaining);
    // Over 99 minutes the countdown gets a third digit; step down so it fits
    // the ring. Both sizes keep the digits' baseline about 2 DIP below the centre.
    let (size, digits_cy) = if countdown.len() > 5 { (24.0, rc.y - 8.0) } else { (ts::DISPLAY, rc.y - 10.0) };
    let digits = TextStyle::new(size).weight(Weight::Semibold).tabular().center().color(if paused { palette::TEXT_DIM } else { palette::TEXT });
    ui.c.text(&countdown, Rect::new(rc.x - 50.0, digits_cy - 20.0, 100.0, 40.0), &digits);
    let (label, label_color) = if paused {
        ("已暂停".to_string(), ring_color)
    } else if running {
        (format!("结束时间 {}", end_clock(st, remaining)), palette::TEXT_DIM)
    } else {
        (phase.label().to_string(), palette::TEXT_DIM)
    };
    ui.c.text(&label, Rect::new(rc.x - 48.0, rc.y + 6.0, 96.0, 16.0), &TextStyle::footnote().color(label_color).center());

    // ---- Controls column, centred on the ring.
    let x0 = rc.x + RING_R + RING_W / 2.0 + COLUMN_GAP;
    let col_w = (area.right() - x0).max(0.0);
    let col_h = CHIP_H + ROW_GAP + BUTTON_H + ROW_GAP + STATS_H;
    let chips_y = (rc.y - col_h / 2.0).round();
    let buttons_y = chips_y + CHIP_H + ROW_GAP;
    let stats_y = buttons_y + BUTTON_H + ROW_GAP;

    if active {
        // Switching is only possible while idle, so a running session shows
        // what it is instead of a row of dead chips.
        let minutes = (st.pomodoro.phase_duration(phase) / 60.0).round() as u32;
        let status = format!("{} · {minutes} 分钟", phase.label());
        let style = TextStyle::body().weight(Weight::Semibold).color(palette::TEXT_DIM);
        ui.c.text(&status, Rect::new(x0, chips_y, col_w, CHIP_H), &style);
    } else {
        draw_segments(ui, st, x0, chips_y);
    }

    let (toggle_label, toggle_icon) = if running {
        ("暂停", Icon::Pause)
    } else if paused {
        ("继续", Icon::Play)
    } else {
        ("开始", Icon::Play)
    };
    // The primary keeps one width for all three labels; squeeze everything on narrow islands.
    let mut toggle_w = ["开始", "暂停", "继续"].iter().map(|l| ui.pill_width(l, true, false)).fold(TOGGLE_MIN_W, f32::max);
    let mut reset_w = ui.pill_width("重置", true, false);
    let mut skip_w = ui.pill_width("跳过", true, false);
    let need = toggle_w + reset_w + skip_w + 2.0 * PILL_GAP;
    if need > col_w && need > 0.0 {
        let k = ((col_w - 2.0 * PILL_GAP) / (need - 2.0 * PILL_GAP)).clamp(0.6, 1.0);
        toggle_w = (toggle_w * k).floor();
        reset_w = (reset_w * k).floor();
        skip_w = (skip_w * k).floor();
    }
    let toggle_r = Rect::new(x0, buttons_y, toggle_w, BUTTON_H);
    if ui.pill(wid("focus.toggle"), toggle_r, toggle_label, Some(toggle_icon), PillStyle::Primary(color)).clicked {
        st.pomodoro.toggle(now);
    }
    let reset_r = Rect::new(toggle_r.right() + PILL_GAP, buttons_y, reset_w, BUTTON_H);
    if active {
        if ui.pill(wid("focus.reset"), reset_r, "重置", Some(Icon::Reset), PillStyle::Neutral).clicked {
            st.pomodoro.reset();
        }
    } else {
        disabled_pill(ui, reset_r, "重置", Icon::Reset);
    }
    let skip_r = Rect::new(reset_r.right() + PILL_GAP, buttons_y, skip_w, BUTTON_H);
    if ui.pill(wid("focus.skip"), skip_r, "跳过", Some(Icon::Next), PillStyle::Neutral).clicked {
        // A skipped phase is not a completed session, so no stats or notice.
        let _ = st.pomodoro.skip(now);
    }

    draw_stats(ui, st, Rect::new(x0, stats_y, col_w, STATS_H));
}

/// Wall-clock time when the running phase ends ("13:20" / "1:20 PM"). The
/// minute is worked out once per run (keyed on its monotonic end), so a
/// whole-second wall clock can't make it flicker near a minute boundary.
fn end_clock(st: &AppState, remaining: f64) -> String {
    let key = match st.pomodoro.run {
        RunState::Running { ends_at } => ends_at,
        _ => f64::NAN,
    };
    let (cached_key, cached_min) = END_MINUTE.with(Cell::get);
    let m = if cached_key == key {
        cached_min
    } else {
        let l = &st.local;
        let secs = l.hour as f64 * 3600.0 + l.minute as f64 * 60.0 + l.second as f64 + remaining.max(0.0);
        let m = ((secs / 60.0).floor() as i64).rem_euclid(24 * 60);
        END_MINUTE.with(|c| c.set((key, m)));
        m
    };
    let t = LocalDateTime { hour: (m / 60) as u8, minute: (m % 60) as u8, second: 0, ..st.local };
    fmt_clock(&t, st.settings.use_24h)
}

/// Focus / Short / Long as a neutral segmented control (idle only): one
/// track, the selected segment lifted in white, no phase colour (the
/// Start capsule is the page's only tinted control).
fn draw_segments(ui: &mut Ui, st: &mut AppState, x0: f32, y: f32) {
    let cfg = &st.settings.pomodoro;
    let segs = [
        (Phase::Focus, "专注", cfg.focus_min),
        (Phase::ShortBreak, "短休", cfg.short_break_min),
        (Phase::LongBreak, "长休", cfg.long_break_min),
    ];
    let name_st = TextStyle::body().weight(Weight::Semibold);
    let num_st = TextStyle::footnote().tabular();
    // Share the baseline: ascent is ~81 % of the line height for Segoe UI,
    // so a centred line's baseline sits 0.31 line-heights below the centre.
    let name_lh = ui.c.measure("F", &name_st, 100.0).1;
    let num_lh = ui.c.measure("2", &num_st, 100.0).1;
    let num_dy = 0.31 * (name_lh - num_lh);

    let inner_h = CHIP_H - 2.0 * SEG_INSET;
    let mut x = x0 + SEG_INSET;
    let mut cells: Vec<(Rect, f32, String)> = Vec::with_capacity(segs.len());
    for (_, name, minutes) in segs.iter() {
        let num = minutes.to_string();
        let nw = ui.c.measure(name, &name_st, 200.0).0;
        let mw = ui.c.measure(&num, &num_st, 100.0).0;
        let w = (SEG_PAD + nw + SEG_NUM_GAP + mw + SEG_PAD).ceil();
        cells.push((Rect::new(x, y + SEG_INSET, w, inner_h), nw, num));
        x += w;
    }
    let track = Rect::new(x0, y, x + SEG_INSET - x0, CHIP_H);
    ui.c.fill_rrect(track, CHIP_H / 2.0, palette::SURFACE);

    let sel = segs.iter().position(|(ph, _, _)| *ph == st.pomodoro.phase).unwrap_or(0);
    let ((sx, sw), gliding) = seg_state(st.now, (cells[sel].0.x, cells[sel].0.w));
    if gliding {
        ui.request_anim();
    }
    ui.c.fill_rrect(Rect::new(sx, y + SEG_INSET, sw, inner_h), inner_h / 2.0, Color::white(0.16));

    let mut picked = None;
    for (i, ((ph, name, _), (r, nw, num))) in segs.iter().zip(cells.iter()).enumerate() {
        let (name_c, num_c) = if i == sel {
            (palette::TEXT, palette::TEXT_DIM)
        } else {
            let resp = ui.button(wid_n("focus.chip", i as u64), Rect::new(r.x, y, r.w, CHIP_H));
            if resp.clicked {
                picked = Some(*ph);
            }
            if resp.pressed || resp.hovered {
                ui.c.fill_rrect(*r, inner_h / 2.0, Color::white(if resp.pressed { 0.10 } else { 0.06 }));
                (palette::TEXT, palette::TEXT_DIM)
            } else {
                (palette::TEXT_DIM, palette::TEXT_DIM)
            }
        };
        let tx = r.x + SEG_PAD;
        ui.c.text(name, Rect::new(tx, y, nw + 2.0, CHIP_H), &name_st.color(name_c));
        let nx = tx + nw + SEG_NUM_GAP;
        ui.c.text(num, Rect::new(nx, y + num_dy, (r.right() - nx).max(0.0), CHIP_H), &num_st.color(num_c));
    }
    if let Some(ph) = picked {
        // set_phase also closes a finished cycle (like ending a long break does).
        st.pomodoro.set_phase(ph);
    }
}

/// Cycle dots and "N focus sessions today".
fn draw_stats(ui: &mut Ui, st: &AppState, r: Rect) {
    let cy = r.center().y;
    let mut x = r.x + 1.0;
    let every = st.pomodoro.cfg.long_every;
    if (1..=12).contains(&every) {
        let done = st.pomodoro.cycle_done.min(every);
        let in_progress = st.pomodoro.phase == Phase::Focus && st.pomodoro.is_active();
        for i in 0..every {
            let c = if i < done {
                palette::ORANGE
            } else if i == done && in_progress {
                palette::ORANGE.mul_alpha(0.45)
            } else {
                palette::TRACK
            };
            ui.c.fill_circle(Point::new(x + 3.5, cy), 3.5, c);
            x += 12.0;
        }
        x += 6.0;
    }
    let n = st.store.focus_sessions.get(&st.day_key()).copied().unwrap_or(0);
    let text = match n {
        0 => "今日尚未开始专注".to_string(),
        1 => "今日已完成 1 次专注".to_string(),
        n => format!("今日已完成 {n} 次专注"),
    };
    ui.c.text(&text, Rect::new(x, r.y, (r.right() - x).max(0.0), r.h), &TextStyle::footnote().color(palette::TEXT_DIM));
}

/// A Regular capsule drawn inert (same layout as [`Ui::pill`], no hit registered).
fn disabled_pill(ui: &mut Ui, r: Rect, label: &str, icon: Icon) {
    ui.c.fill_rrect(r, r.h / 2.0, palette::SURFACE.mul_alpha(0.6));
    let st = TextStyle::body().weight(Weight::Semibold).color(palette::DISABLED);
    let tw = ui.c.measure(label, &st, r.w).0;
    let (isz, gap) = (14.0, 6.0);
    let x = r.x + (r.w - isz - gap - tw) / 2.0;
    ui.c.icon(icon, Rect::new(x, r.center().y - isz / 2.0, isz, isz), palette::DISABLED);
    ui.c.text(label, Rect::new(x + isz + gap, r.y, tw + 2.0, r.h), &st);
}

fn sessions_text(n: u32) -> String {
    format!("{n} 次专注")
}

/// Called by the app when a Pomodoro phase finishes on its own.
pub fn on_phase_ended(st: &mut AppState, e: PhaseEnded) {
    let key = st.day_key();
    if e.ended == Phase::Focus {
        let n = st.store.focus_sessions.entry(key.clone()).or_insert(0);
        *n = n.saturating_add(1);
        let cutoff = st.local.add_days(-KEEP_STATS_DAYS).day_key();
        st.store.focus_sessions.retain(|k, _| k.as_str() >= cutoff.as_str());
        st.save();
    }
    let today = st.store.focus_sessions.get(&key).copied().unwrap_or(0);
    let minutes = (st.pomodoro.phase_duration(e.next) / 60.0).round() as u32;
    let (icon, color, title, subtitle) = match e.next {
        Phase::Focus => (
            Icon::Timer,
            palette::ORANGE,
            "休息时间结束".to_string(),
            if e.auto_started { format!("已开始 {minutes} 分钟专注") } else { "点击开始下一次专注".to_string() },
        ),
        Phase::ShortBreak | Phase::LongBreak => {
            let title = if e.next == Phase::LongBreak {
                format!("周期完成 — {minutes} 分钟长休息")
            } else {
                format!("专注完成 — {minutes} 分钟短休息")
            };
            let action = if e.auto_started { "已自动开始休息" } else { "点击开始休息" };
            (Icon::Coffee, palette::GREEN, title, format!("今日累计完成 {} · {action}", sessions_text(today)))
        }
    };
    // When the next phase didn't start by itself, clicking the notice (or its
    // Start pill, which doesn't expand the island) starts it.
    let action = if e.auto_started { NoticeAction::OpenPage(super::PageKind::Focus) } else { NoticeAction::StartFocus };
    st.notify(icon, color, title, subtitle, 6.0, Some(action));
    if !e.auto_started {
        st.set_notice_inline(InlineAction::StartFocus);
    }
    st.chime();
}
