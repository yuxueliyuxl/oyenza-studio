//! Clipboard page: recent copied text (memory only), newest first.
//!
//! * Click a row to put that text back on the clipboard ("Copied" flash).
//! * Hover a row for pin / remove actions; pinned rows survive "Clear" and
//!   eviction.
//! * "Clear" and a row's remove can be undone for a few seconds.
//! * Mouse wheel scrolls (smoothly) with soft fades at the clipped edges.

use crate::commands::Command;
use crate::gfx::{TextStyle, Weight};
use crate::icons::Icon;
use crate::state::AppState;
use crate::ui::{wid, wid_n, PillStyle, Resp, Ui, PILL_GAP, PILL_REGULAR, PILL_SMALL};
use poni_core::cliphist::{preview, ClipHistory, ClipItem};
use poni_core::geom::{palette, Color, Point, Rect};
use poni_core::svgpath::Seg;
use std::cell::{Cell, RefCell};

/// Summary row: item count on the left, Clear / Undo on the right.
const HEAD_H: f32 = PILL_SMALL;
const HEAD_GAP: f32 = 8.0;
const ROW_H: f32 = 32.0;
const ROW_GAP: f32 = 2.0;
const PITCH: f32 = ROW_H + ROW_GAP;
const ROW_RADIUS: f32 = 10.0;
/// Row backgrounds bleed this far into the page padding, so the row text
/// shares the content column's left edge with the summary row.
const ROW_BLEED: f32 = 10.0;
/// Rows visible before the list scrolls.
const MAX_VISIBLE: usize = 5;
/// Tallest page: summary row + five rows (= 204).
const MAX_H: f32 = HEAD_H + HEAD_GAP + MAX_VISIBLE as f32 * PITCH - ROW_GAP;
/// Shortest page (empty / disabled / one to three rows).
const MIN_H: f32 = 150.0;
/// Right-hand zone of a row: age, pin badge, hover actions or "Copied".
const TRAIL_W: f32 = 72.0;
const ACTION: f32 = 26.0;
/// Height of the fade drawn where the list is clipped.
const FADE_H: f32 = 16.0;
/// DIPs scrolled per wheel notch.
const WHEEL_STEP: f32 = PITCH * 1.5;
const FLASH_SECS: f64 = 0.8;
/// How long "Clear" or a removal can be undone.
const UNDO_SECS: f64 = 8.0;
/// Characters scanned / kept for the one-line preview.
const PREVIEW_SCAN: usize = 600;
const PREVIEW_CHARS: usize = 160;

pub fn height(st: &AppState) -> f32 {
    if !st.settings.clipboard_history || st.clip.is_empty() {
        return MIN_H;
    }
    let n = st.clip.len().min(MAX_VISIBLE) as f32;
    (HEAD_H + HEAD_GAP + n * PITCH - ROW_GAP).clamp(MIN_H, MAX_H)
}

/// When the Undo pill expires (the page needs a frame then), if one is up.
pub fn next_wake(st: &AppState) -> Option<f64> {
    UNDO.with(|u| u.borrow().as_ref().map(|u| u.at + UNDO_SECS)).filter(|&t| t > st.now)
}

pub fn draw(ui: &mut Ui, st: &mut AppState, area: Rect) {
    if !st.settings.clipboard_history {
        st.ui.clip_flash = None;
        forget_undo();
        draw_disabled(ui, st, area);
        return;
    }

    let undo = undo_live(ui.now);
    let (head, rest) = area.split_top(HEAD_H);
    if !st.clip.is_empty() || undo {
        draw_summary(ui, st, head, undo);
    }
    if st.clip.is_empty() {
        st.ui.clip_scroll = 0.0;
        st.ui.clip_flash = None;
        super::empty_state(ui, area, Icon::Clipboard, "剪贴板暂无内容", "复制的文本将显示在此处，仅保存在内存中，安全不落盘");
        return;
    }
    let list = Rect::new(rest.x, rest.y + HEAD_GAP, rest.w, (rest.h - HEAD_GAP).max(0.0));
    draw_list(ui, st, list, pill_card(st, area));
}

// ------------------------------------------------------------------ summary

/// "12 items · 2 pinned" and a Small utility pill: Clear, or Undo right after
/// something was removed.
fn draw_summary(ui: &mut Ui, st: &mut AppState, r: Rect, undo: bool) {
    let scale = ui.c.scale().max(0.5);
    let now = ui.now;
    let n = st.clip.len();
    let pinned = st.clip.iter().filter(|it| it.pinned).count();
    let pill_y = snap(r.y + (r.h - PILL_SMALL) / 2.0, scale);
    let mut text_right = r.right();

    if undo {
        let w = snap(ui.pill_width("撤销", true, true), scale);
        let pr = Rect::new(snap(r.right() - w, scale), pill_y, w, PILL_SMALL);
        if ui.pill(wid("clip.undo"), pr, "撤销", Some(Icon::Undo), PillStyle::UtilityAccent(st.accent)).clicked {
            if let Some(u) = take_undo() {
                restore(&mut st.clip, &u);
            }
            st.ui.clip_flash = None;
        }
        text_right = pr.x - PILL_GAP;
    } else if pinned < n {
        // Only when something unpinned is left to clear.
        let w = snap(ui.pill_width("清空", true, true), scale);
        let pr = Rect::new(snap(r.right() - w, scale), pill_y, w, PILL_SMALL);
        if ui.pill(wid("clip.clear"), pr, "清空", Some(Icon::Trash), PillStyle::Utility).clicked {
            let before: Vec<ClipItem> = st.clip.iter().cloned().collect();
            st.clip.clear();
            let removed = before.iter().filter(|it| !it.pinned).cloned().collect();
            remember(before, removed, now);
            st.ui.clip_scroll = 0.0;
            st.ui.clip_flash = None;
        }
        text_right = pr.x - PILL_GAP;
    }

    let n = st.clip.len();
    if n == 0 {
        return;
    }
    let pinned = st.clip.iter().filter(|it| it.pinned).count();
    let mut count = format!("{n} 项");
    if pinned > 0 {
        count.push_str(&format!(" \u{b7} {pinned} 项已固定"));
    }
    let count_st = TextStyle::body().weight(Weight::Semibold).color(palette::TEXT_DIM);
    ui.c.text(&count, Rect::new(r.x, r.y, (text_right - r.x).max(0.0), r.h), &count_st);
}

// ------------------------------------------------------------------ list

enum RowAction {
    Copy(usize),
    TogglePin(usize),
    Remove(usize),
}

fn draw_list(ui: &mut Ui, st: &mut AppState, list: Rect, card: Option<(Rect, f32)>) {
    let n = st.clip.len();
    let content_h = n as f32 * PITCH - ROW_GAP;
    let max_scroll = (content_h - list.h).max(0.0);

    // Only claim the wheel when there is something to scroll; otherwise the
    // app uses it to switch pages. The whole page body (summary included)
    // scrolls; the tab header above it still switches pages.
    if max_scroll > 0.0 {
        let page = Rect::new(list.x - ROW_BLEED, list.y - HEAD_H - HEAD_GAP, list.w + 2.0 * ROW_BLEED, list.h + HEAD_H + HEAD_GAP);
        let w = ui.wheel_in(page);
        if w != 0.0 {
            st.ui.clip_scroll -= w * WHEEL_STEP;
        }
    }
    st.ui.clip_scroll = st.ui.clip_scroll.clamp(0.0, max_scroll);
    let scroll = smooth_scroll(ui, st.ui.clip_scroll, max_scroll);

    // Copied flash: active for FLASH_SECS, then cleared (and the animation stops).
    let now = ui.now;
    let flash = st.ui.clip_flash.filter(|&(i, t0)| i < n && now >= t0 && now - t0 < FLASH_SECS);
    st.ui.clip_flash = flash;
    if flash.is_some() {
        ui.request_anim();
    }

    let first = (scroll / PITCH).floor().max(0.0) as usize;
    let last = (((scroll + list.h) / PITCH).ceil().max(0.0) as usize).min(n);
    let mut action: Option<RowAction> = None;

    // The clip and the fades include the rows' bleed into the padding.
    let wide = Rect::new(list.x - ROW_BLEED, list.y, list.w + 2.0 * ROW_BLEED, list.h);
    ui.c.clip(wide);
    // While the list glides its rows move between pixels; at rest they sit on whole pixels.
    let snap_was = ui.c.set_text_snap(scroll == st.ui.clip_scroll);
    for i in first..last {
        let Some(item) = st.clip.get(i) else { break };
        let row = Rect::new(list.x, list.y + i as f32 * PITCH - scroll, list.w, ROW_H);
        let flash_t = match flash {
            Some((fi, t0)) if fi == i => Some((now - t0) as f32),
            _ => None,
        };
        if let Some(a) = draw_row(ui, st.accent, st.unix, i, item, row, list, flash_t, card) {
            action = Some(a);
        }
    }
    ui.c.set_text_snap(snap_was);
    // Fades where rows are cut off, stronger the more is hidden.
    fade_edge(ui, Rect::new(wide.x, wide.y, wide.w, FADE_H), true, (scroll / FADE_H).min(1.0));
    fade_edge(ui, Rect::new(wide.x, wide.bottom() - FADE_H, wide.w, FADE_H), false, ((max_scroll - scroll) / FADE_H).min(1.0));
    ui.c.unclip();

    if n <= 2 {
        draw_hint(ui, Rect::new(list.x, list.y + content_h, list.w, list.bottom() - list.y - content_h));
    }

    match action {
        Some(RowAction::Copy(i)) => {
            if let Some(it) = st.clip.get(i) {
                let text = it.text.clone();
                st.cmd(Command::CopyText(text));
                st.ui.clip_flash = Some((i, st.now));
                ui.request_anim();
            }
        }
        Some(RowAction::TogglePin(i)) => st.clip.toggle_pin(i),
        Some(RowAction::Remove(i)) => {
            let before: Vec<ClipItem> = st.clip.iter().cloned().collect();
            if let Some(removed) = st.clip.remove(i) {
                remember(before, vec![removed], now);
                st.ui.clip_flash = match st.ui.clip_flash {
                    Some((fi, _)) if fi == i => None,
                    Some((fi, t)) if fi > i => Some((fi - 1, t)),
                    f => f,
                };
            }
        }
        None => {}
    }
}

/// One history row: `row` is the text column; its background bleeds
/// `ROW_BLEED` into the padding on both sides. `flash_t` = seconds since this
/// row was copied (while flashing). `card`: the Pill card ([`pill_card`]).
#[allow(clippy::too_many_arguments)]
fn draw_row(
    ui: &mut Ui,
    accent: Color,
    unix: i64,
    i: usize,
    item: &ClipItem,
    row: Rect,
    list: Rect,
    flash_t: Option<f32>,
    card: Option<(Rect, f32)>,
) -> Option<RowAction> {
    let key = item_key(item);
    let scale = ui.c.scale().max(0.5);
    let (bx0, bx1) = (snap(row.x - ROW_BLEED, scale), snap(row.right() + ROW_BLEED, scale));
    let bg_r = Rect::new(bx0, row.y, bx1 - bx0, row.h);
    // Hit-test only the visible part so rows hidden under the fades/summary
    // can't be clicked.
    let top = bg_r.y.max(list.y);
    let bottom = bg_r.bottom().min(list.bottom());
    let vis = Rect::new(bg_r.x, top, bg_r.w, (bottom - top).max(0.0));
    let resp = if vis.h >= 8.0 { ui.button(wid_n("clip.row", key), vis) } else { Resp::default() };
    let mut action = if resp.clicked { Some(RowAction::Copy(i)) } else { None };

    let bg = if resp.pressed {
        Color::white(0.13)
    } else if resp.hovered {
        palette::SURFACE
    } else {
        Color::white(0.0)
    };
    // The bottom row's background meets the card's rounded corners in Pill.
    if bg.a > 0.0 {
        fill_box(ui, card, bg_r, ROW_RADIUS, bg);
    }
    // Flash strength: full for most of the flash, then a quick fade-out.
    let flash_a = flash_t.map(|t| {
        let fade_from = FLASH_SECS as f32 - 0.25;
        if t <= fade_from {
            1.0
        } else {
            (1.0 - (t - fade_from) / 0.25).clamp(0.0, 1.0)
        }
    });
    if let Some(a) = flash_a {
        fill_box(ui, card, bg_r, ROW_RADIUS, palette::GREEN.with_alpha(0.16 * a));
    }

    // Preview text, on the content column's left edge.
    let text_r = Rect::new(row.x, row.y, (row.w - TRAIL_W - 8.0).max(0.0), row.h);
    ui.c.text(&row_preview(&item.text), text_r, &TextStyle::body());

    // Trailing zone, ending on the content column's right edge.
    let cy = row.center().y;
    let trail_right = row.right();
    if let Some(a) = flash_a {
        ui.c.with_alpha(a, |c| {
            let st = TextStyle::body().weight(Weight::Semibold).color(palette::GREEN).right();
            let label_w = c.measure("已复制", &st, TRAIL_W).0 + 2.0;
            c.text("已复制", Rect::new(trail_right - label_w, row.y, label_w, row.h), &st);
            c.icon(Icon::Check, Rect::new(trail_right - label_w - 18.0, cy - 7.0, 14.0, 14.0), palette::GREEN);
        });
    } else if resp.hovered && row.y >= list.y - 0.5 && row.bottom() <= list.bottom() + 0.5 {
        // Hover actions, registered after the row so they win the hit test.
        let del = Rect::new(bg_r.right() - 4.0 - ACTION, cy - ACTION / 2.0, ACTION, ACTION);
        let pin = Rect::new(del.x - 2.0 - ACTION, del.y, ACTION, ACTION);
        let (pin_icon, pin_col) = if item.pinned { (Icon::PinFill, accent) } else { (Icon::Pin, palette::TEXT_DIM) };
        if ui.icon_button(wid_n("clip.pin", key), pin, pin_icon, pin_col, 14.0).clicked {
            action = Some(RowAction::TogglePin(i));
        }
        if ui.icon_button(wid_n("clip.del", key), del, Icon::Close, palette::TEXT_DIM, 13.0).clicked {
            action = Some(RowAction::Remove(i));
        }
    } else {
        let age = fmt_age(unix.saturating_sub(item.at_unix));
        let age_st = TextStyle::footnote().color(palette::TEXT_FAINT).tabular().right();
        let age_w = ui.c.measure(&age, &age_st, TRAIL_W).0;
        ui.c.text(&age, Rect::new(trail_right - age_w - 2.0, row.y, age_w + 2.0, row.h), &age_st);
        if item.pinned {
            ui.c.icon(Icon::PinFill, Rect::new(trail_right - age_w - 6.0 - 13.0, cy - 6.5, 13.0, 13.0), accent);
        }
    }
    action
}

/// Faint usage hint in the empty space below a short list.
fn draw_hint(ui: &mut Ui, r: Rect) {
    if r.h < 36.0 {
        return;
    }
    ui.c.text(
        "点击任意条目即可重新复制",
        Rect::new(r.x, r.y + (r.h - 20.0) / 2.0, r.w, 20.0),
        &TextStyle::footnote().color(palette::TEXT_FAINT).center(),
    );
}

// ------------------------------------------------------------------ disabled

fn draw_disabled(ui: &mut Ui, st: &mut AppState, area: Rect) {
    // Icon, title, hint and a pill as one vertically centred group.
    let group_h = 26.0 + 10.0 + 20.0 + 2.0 + 18.0 + 14.0 + PILL_REGULAR;
    let mut y = area.y + ((area.h - group_h) / 2.0).max(0.0);
    let cx = area.center().x;
    ui.c.icon(Icon::Clipboard, Rect::from_center(Point::new(cx, y + 13.0), 26.0, 26.0), Color::white(0.35));
    y += 36.0;
    ui.c.text("剪贴板历史记录已关闭", Rect::new(area.x, y, area.w, 20.0), &TextStyle::headline().center());
    y += 22.0;
    ui.c.text(
        "在此记录您最近复制的文本。数据仅存放在内存中，不会写入磁盘。",
        Rect::new(area.x, y, area.w, 18.0),
        &TextStyle::footnote().color(palette::TEXT_DIM).center(),
    );
    y += 32.0;
    let scale = ui.c.scale().max(0.5);
    let w = snap(ui.pill_width("开启", false, false), scale);
    let pill = Rect::new(snap(cx - w / 2.0, scale), snap(y, scale), w, PILL_REGULAR);
    if ui.pill(wid("clip.enable"), pill, "开启", None, PillStyle::Primary(st.accent)).clicked {
        st.settings.clipboard_history = true;
        st.cmd(Command::SaveSettings);
    }
}

// ------------------------------------------------------------------ undo

/// What the last Clear / remove took away, for a few seconds.
struct Undo {
    /// The whole list before the action (gives the original order).
    before: Vec<ClipItem>,
    removed: Vec<ClipItem>,
    at: f64,
}

thread_local! {
    // Pages only ever run on the UI thread. Memory only, like the history.
    static UNDO: RefCell<Option<Undo>> = const { RefCell::new(None) };
}

fn remember(before: Vec<ClipItem>, removed: Vec<ClipItem>, now: f64) {
    if removed.is_empty() {
        return;
    }
    UNDO.with(|u| *u.borrow_mut() = Some(Undo { before, removed, at: now }));
}

/// Whether an Undo is still on offer; drops an expired snapshot.
fn undo_live(now: f64) -> bool {
    UNDO.with(|u| {
        let mut u = u.borrow_mut();
        let live = u.as_ref().is_some_and(|x| now >= x.at && now - x.at < UNDO_SECS);
        if !live {
            *u = None;
        }
        live
    })
}

fn take_undo() -> Option<Undo> {
    UNDO.with(|u| u.borrow_mut().take())
}

fn forget_undo() {
    let _ = UNDO.try_with(|u| u.borrow_mut().take());
}

/// Put the removed items back where they were. Anything copied since stays
/// on top; a removed text that has been copied again is not duplicated.
fn restore(clip: &mut ClipHistory, u: &Undo) {
    let current: Vec<ClipItem> = clip.iter().cloned().collect();
    let same = |a: &ClipItem, b: &ClipItem| a.text == b.text && a.at_unix == b.at_unix;
    let mut out: Vec<ClipItem> = current.iter().filter(|c| !u.before.iter().any(|b| same(b, c))).cloned().collect();
    for b in &u.before {
        if let Some(c) = current.iter().find(|c| same(c, b)) {
            out.push(c.clone());
        } else if u.removed.iter().any(|r| same(r, b)) && !current.iter().any(|c| c.text == b.text) {
            out.push(b.clone());
        }
    }
    // Rebuild oldest first (push adds to the front), keeping each item's time and pin.
    while clip.remove(0).is_some() {}
    for it in out.iter().rev() {
        if clip.push(&it.text, it.at_unix) && it.pinned {
            clip.toggle_pin(0);
        }
    }
}

// ------------------------------------------------------------------ helpers

thread_local! {
    /// Displayed (animated) scroll offset and the time it was last drawn.
    static SCROLL_ANIM: Cell<(f32, f64)> = const { Cell::new((0.0, f64::NEG_INFINITY)) };
}

/// Ease the drawn scroll offset towards `target`; frames are requested only
/// while it is still moving.
fn smooth_scroll(ui: &mut Ui, target: f32, max_scroll: f32) -> f32 {
    let now = ui.now;
    SCROLL_ANIM.with(|cell| {
        let (prev, last) = cell.get();
        // After an idle gap (the frame clock was parked) step as if one frame passed.
        let dt = if now > last { (now - last).min(1.0 / 30.0) as f32 } else { 0.0 };
        let mut pos = prev.clamp(0.0, max_scroll);
        pos += (target - pos) * (1.0 - (-dt * 18.0).exp());
        if (target - pos).abs() < 0.5 {
            pos = target;
        } else {
            ui.request_anim();
        }
        cell.set((pos, now));
        pos
    })
}

/// Black gradient over `r` (the island is pure black, so this reads as content
/// fading out). Opaque-ish at the outer edge, transparent at the inner one.
fn fade_edge(ui: &mut Ui, r: Rect, outer_is_top: bool, strength: f32) {
    if strength <= 0.01 || r.h <= 0.0 {
        return;
    }
    const STEPS: usize = 8;
    const EDGE: f32 = 0.94;
    // Nested bands that all touch the outer edge (no seams at fractional
    // scales). Band k covers the outer (k+1)/STEPS of the fade; per-band alphas
    // make the composite fall off linearly towards the inner edge.
    let target = |j: usize| EDGE * strength * (STEPS - j) as f32 / STEPS as f32;
    for k in (0..STEPS).rev() {
        let below = if k + 1 < STEPS { target(k + 1) } else { 0.0 };
        let a = 1.0 - (1.0 - target(k)) / (1.0 - below);
        let depth = r.h * (k + 1) as f32 / STEPS as f32;
        let band = if outer_is_top { Rect::new(r.x, r.y, r.w, depth) } else { Rect::new(r.x, r.bottom() - depth, r.w, depth) };
        ui.c.fill_rect(band, Color::black(a));
    }
}

fn snap(v: f32, scale: f32) -> f32 {
    (v * scale).round() / scale
}

/// One-line preview; only the head of very long copies is scanned.
fn row_preview(text: &str) -> String {
    let head = match text.char_indices().nth(PREVIEW_SCAN) {
        Some((b, _)) => &text[..b],
        None => text,
    };
    let mut p = preview(head, PREVIEW_CHARS);
    if head.len() < text.len() {
        if p.is_empty() {
            p = preview(text, PREVIEW_CHARS);
        } else if !p.ends_with('…') {
            p.push('…');
        }
    }
    p.retain(|c| !c.is_control());
    p
}

/// Stable per-item id component (content head + length + capture time), so a
/// press that straddles a list change can't hit a different row.
fn item_key(it: &ClipItem) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in it.text.as_bytes().iter().take(256) {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h ^ (it.text.len() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (it.at_unix as u64).rotate_left(29)
}

/// "now", "2m", "1h", "3d".
fn fmt_age(secs: i64) -> String {
    match secs {
        s if s < 60 => "now".to_string(),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

// ------------------------------------------------------------------ Pill corners
//
// Shared with the other pages that put a box or a row's background near the
// open card's bottom corners (Shelf, Today, Add-ons, Prayer). The Pill card's
// corners are rounder than Classic's, so a box that keeps its Classic radius
// there pinches the band between it and the card's edge.

/// Pill: the open card (`area`, the page's content rect, grown back by the
/// padding and the header) and its corner radius, read from the island's own
/// geometry so the pages follow whatever radius the card is given. `None` in
/// the other themes: their pages draw exactly as before.
pub(crate) fn pill_card(st: &AppState, area: Rect) -> Option<(Rect, f32)> {
    if !crate::pill::on(st.settings.theme) {
        return None;
    }
    let r = crate::activities::geom(&crate::activities::Mode::Expanded(st.page), st).r;
    let (pad, head) = (super::PAD, super::HEADER_H);
    Some((Rect::new(area.x - pad, area.y - head, area.w + 2.0 * pad, area.h + head + pad), r))
}

/// The corner radii (top-left, top-right, bottom-right, bottom-left) for a box
/// `r` with corner `radius` inside `card` (corner radius `card_r`). A bottom
/// corner that sits in the card's rounded corner takes the card's radius less
/// its larger distance to the card's edges, so the band between the two keeps
/// about its width round the curve (concentric corners); never sharper than
/// `radius`, and no rounder than the box leaves room for. The top corners sit
/// under the header, where the card's sides are straight.
pub(crate) fn pill_corners(card: Rect, card_r: f32, r: Rect, radius: f32) -> [f32; 4] {
    let below = card.bottom() - r.bottom();
    let cap = (r.h - radius).min(r.w / 2.0).max(radius);
    let corner = |side: f32| if side < card_r && below < card_r { (card_r - side.max(below)).clamp(radius, cap) } else { radius };
    [radius, radius, corner(card.right() - r.right()), corner(r.x - card.x)]
}

/// Fill `r` with corner `radius`; on the Pill card (`card` from [`pill_card`])
/// with the corners [`pill_corners`] gives it.
pub(crate) fn fill_box(ui: &mut Ui, card: Option<(Rect, f32)>, r: Rect, radius: f32, fill: Color) {
    match card.map(|(c, cr)| pill_corners(c, cr, r, radius)).filter(|k| *k != [radius; 4]) {
        Some(k) => ui.c.path(&rrect_segments(r, k), Some(fill), None),
        None => ui.c.fill_rrect(r, radius, fill),
    }
}

/// [`fill_box`]'s outline: a `width` line just inside `r`'s edge.
pub(crate) fn stroke_box(ui: &mut Ui, card: Option<(Rect, f32)>, r: Rect, radius: f32, color: Color, width: f32) {
    let half = width / 2.0;
    match card.map(|(c, cr)| pill_corners(c, cr, r, radius)).filter(|k| *k != [radius; 4]) {
        Some(k) => ui.c.path(&rrect_segments(r.inset(half, half), k.map(|x| (x - half).max(0.0))), None, Some((color, width))),
        None => ui.c.stroke_rrect(r, (radius - half).max(0.0), color, width),
    }
}

/// A rounded rectangle with its own radius at each corner, as path segments.
fn rrect_segments(r: Rect, [tl, tr, br, bl]: [f32; 4]) -> Vec<Seg> {
    let (x0, y0, x1, y1) = (r.x, r.y, r.right(), r.bottom());
    vec![
        Seg::MoveTo(x0 + tl, y0),
        Seg::LineTo(x1 - tr, y0),
        Seg::ArcTo(tr, tr, 0.0, false, true, x1, y0 + tr),
        Seg::LineTo(x1, y1 - br),
        Seg::ArcTo(br, br, 0.0, false, true, x1 - br, y1),
        Seg::LineTo(x0 + bl, y1),
        Seg::ArcTo(bl, bl, 0.0, false, true, x0, y1 - bl),
        Seg::LineTo(x0, y0 + tl),
        Seg::ArcTo(tl, tl, 0.0, false, true, x0 + tl, y0),
        Seg::Close,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages() {
        assert_eq!(fmt_age(-5), "now");
        assert_eq!(fmt_age(59), "now");
        assert_eq!(fmt_age(150), "2m");
        assert_eq!(fmt_age(3600), "1h");
        assert_eq!(fmt_age(3 * 86_400 + 10), "3d");
    }

    #[test]
    fn previews() {
        assert_eq!(row_preview("  first line\n\tsecond  "), "first line second");
        let long = "word ".repeat(1000);
        let p = row_preview(&long);
        assert!(p.ends_with('…') && p.chars().count() <= PREVIEW_CHARS);
        let late = format!("{}tail", " ".repeat(PREVIEW_SCAN + 10));
        assert_eq!(row_preview(&late), "tail");
        assert_eq!(row_preview("a\u{0}b"), "ab");
    }

    #[test]
    fn page_heights() {
        assert_eq!(MAX_H, 204.0);
        assert!(MIN_H <= MAX_H);
    }

    fn texts(c: &ClipHistory) -> Vec<(String, bool)> {
        c.iter().map(|it| (it.text.clone(), it.pinned)).collect()
    }

    #[test]
    fn undo_restores_order_pins_and_newer_copies() {
        let mut c = ClipHistory::new(30);
        for (i, t) in ["d", "c", "b", "a"].iter().enumerate() {
            c.push(t, 100 + i as i64);
        }
        c.toggle_pin(1); // "b"
        let before: Vec<ClipItem> = c.iter().cloned().collect();
        c.clear();
        let removed = before.iter().filter(|it| !it.pinned).cloned().collect();
        let u = Undo { before, removed, at: 0.0 };
        // Copied after the clear: "e" is new, "c" was cleared and copied again.
        c.push("e", 200);
        c.push("c", 201);
        restore(&mut c, &u);
        let want: Vec<(String, bool)> =
            [("c", false), ("e", false), ("a", false), ("b", true), ("d", false)].iter().map(|(t, p)| (t.to_string(), *p)).collect();
        assert_eq!(texts(&c), want);
    }

    #[test]
    fn undo_single_remove_goes_back_in_place() {
        let mut c = ClipHistory::new(30);
        for (i, t) in ["c", "b", "a"].iter().enumerate() {
            c.push(t, 10 + i as i64);
        }
        let before: Vec<ClipItem> = c.iter().cloned().collect();
        let removed = c.remove(1).unwrap();
        restore(&mut c, &Undo { before, removed: vec![removed], at: 0.0 });
        assert_eq!(texts(&c).into_iter().map(|(t, _)| t).collect::<Vec<_>>(), ["a", "b", "c"]);
    }

    #[test]
    fn undo_respects_capacity_and_keeps_pins() {
        let mut c = ClipHistory::new(3);
        for (i, t) in ["c", "b", "a"].iter().enumerate() {
            c.push(t, 10 + i as i64);
        }
        c.toggle_pin(2); // "c" (oldest) pinned
        let before: Vec<ClipItem> = c.iter().cloned().collect();
        c.clear();
        let removed = before.iter().filter(|it| !it.pinned).cloned().collect();
        let u = Undo { before, removed, at: 0.0 };
        c.push("x", 50);
        c.push("y", 51);
        restore(&mut c, &u);
        // Cap 3: the newer copies and the pinned item stay; the oldest unpinned go.
        assert_eq!(texts(&c), vec![("y".to_string(), false), ("x".to_string(), false), ("c".to_string(), true)]);
    }

    /// The Pill card the pages see at the default width: 620 wide, r 40.
    fn card(h: f32) -> Rect {
        Rect::new(0.0, 0.0, 620.0, h)
    }

    /// Closest approach (DIPs) of the box `r` with corner radii `k` to the
    /// edge of the card `c` (radius `cr`), over its two bottom corners.
    fn bottom_gap(c: Rect, cr: f32, r: Rect, k: [f32; 4]) -> f32 {
        // Distance from `p` to the card's outline, positive inside.
        let inside = |x: f32, y: f32| {
            let (cx, cy) = (x.clamp(c.x + cr, c.right() - cr), y.clamp(c.y + cr, c.bottom() - cr));
            let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
            if d > 0.0 { cr - d } else { (x - c.x).min(c.right() - x).min(y - c.y).min(c.bottom() - y) }
        };
        let mut gap = f32::MAX;
        for i in 0..=90 {
            let a = (i as f32).to_radians();
            let (br, bl) = (k[2], k[3]);
            gap = gap.min(inside(r.right() - br + br * a.cos(), r.bottom() - br + br * a.sin()));
            gap = gap.min(inside(r.x + bl - bl * a.cos(), r.bottom() - bl + bl * a.sin()));
        }
        gap
    }

    #[test]
    fn pill_boxes_follow_the_card_corners() {
        let c = card(272.0);
        // Clipboard's bottom row: bleeds to 8 from the sides, 18 above the bottom.
        let row = Rect::new(8.0, c.bottom() - 18.0 - ROW_H, 604.0, ROW_H);
        let k = pill_corners(c, 40.0, row, ROW_RADIUS);
        assert_eq!(k, [ROW_RADIUS, ROW_RADIUS, 22.0, 22.0], "bottom corners as round as the row leaves room for");
        assert!(bottom_gap(c, 40.0, row, k) >= 7.9, "keeps its 8 DIP side gap round the corner");
        assert!(bottom_gap(c, 40.0, row, [ROW_RADIUS; 4]) < 5.5, "Classic's radius pinches it");
        // A row higher up, or any box on Classic's 30 corner that is already fine, keeps its radius.
        let up = Rect::new(8.0, row.y - 2.0 * PITCH, 604.0, ROW_H);
        assert_eq!(pill_corners(c, 40.0, up, ROW_RADIUS), [ROW_RADIUS; 4]);
        // Today's bottom-right tile (18 in from the side and the bottom): concentric.
        let tile = Rect::new(456.0, c.bottom() - 18.0 - 78.0, 146.0, 78.0);
        let k = pill_corners(c, 40.0, tile, 16.0);
        assert_eq!(k, [16.0, 16.0, 22.0, 16.0]);
        assert!(bottom_gap(c, 40.0, tile, k) >= 17.9);
        assert_eq!(pill_corners(c, 30.0, tile, 16.0), [16.0; 4], "never sharper than its own radius");
        // Shelf's drop zone sits 22 above the bottom: its 18 already keeps the band even.
        let zone = Rect::new(18.0, 50.0, 380.0, c.bottom() - 22.0 - 50.0);
        assert_eq!(pill_corners(c, 40.0, zone, 18.0), [18.0; 4]);
    }
}
