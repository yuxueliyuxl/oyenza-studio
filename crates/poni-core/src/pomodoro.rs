//! Pomodoro focus timer state machine. Time is a monotonic clock in seconds
//! supplied by the caller.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Focus,
    ShortBreak,
    LongBreak,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Focus => "专注",
            Phase::ShortBreak => "短休息",
            Phase::LongBreak => "长休息",
        }
    }
    pub fn is_break(self) -> bool {
        self != Phase::Focus
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PomodoroConfig {
    pub focus_min: u32,
    pub short_break_min: u32,
    pub long_break_min: u32,
    /// A long break after this many focus sessions.
    pub long_every: u32,
    pub auto_start_breaks: bool,
    pub auto_start_focus: bool,
}

impl Default for PomodoroConfig {
    fn default() -> Self {
        Self {
            focus_min: 25,
            short_break_min: 5,
            long_break_min: 15,
            long_every: 4,
            auto_start_breaks: true,
            auto_start_focus: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RunState {
    Idle,
    Running { ends_at: f64 },
    Paused { remaining: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhaseEnded {
    pub ended: Phase,
    pub next: Phase,
    /// Whether the next phase started automatically.
    pub auto_started: bool,
}

#[derive(Clone, Debug)]
pub struct Pomodoro {
    pub cfg: PomodoroConfig,
    pub phase: Phase,
    pub run: RunState,
    /// Focus sessions completed in the current long-break cycle.
    pub cycle_done: u32,
}

impl Pomodoro {
    pub fn new(cfg: PomodoroConfig) -> Self {
        Self { cfg, phase: Phase::Focus, run: RunState::Idle, cycle_done: 0 }
    }

    pub fn phase_duration(&self, phase: Phase) -> f64 {
        let min = match phase {
            Phase::Focus => self.cfg.focus_min,
            Phase::ShortBreak => self.cfg.short_break_min,
            Phase::LongBreak => self.cfg.long_break_min,
        };
        (min.max(1) as f64) * 60.0
    }

    pub fn remaining(&self, now: f64) -> f64 {
        match self.run {
            RunState::Idle => self.phase_duration(self.phase),
            RunState::Running { ends_at } => (ends_at - now).max(0.0),
            RunState::Paused { remaining } => remaining,
        }
    }

    /// Elapsed fraction 0..=1 of the current phase.
    pub fn progress(&self, now: f64) -> f32 {
        let total = self.phase_duration(self.phase);
        (1.0 - self.remaining(now) / total).clamp(0.0, 1.0) as f32
    }

    pub fn is_running(&self) -> bool {
        matches!(self.run, RunState::Running { .. })
    }

    pub fn is_active(&self) -> bool {
        !matches!(self.run, RunState::Idle)
    }

    pub fn start(&mut self, now: f64) {
        self.run = match self.run {
            RunState::Idle => RunState::Running { ends_at: now + self.phase_duration(self.phase) },
            RunState::Paused { remaining } => RunState::Running { ends_at: now + remaining },
            r @ RunState::Running { .. } => r,
        };
    }

    pub fn pause(&mut self, now: f64) {
        if let RunState::Running { ends_at } = self.run {
            self.run = RunState::Paused { remaining: (ends_at - now).max(0.0) };
        }
    }

    pub fn toggle(&mut self, now: f64) {
        if self.is_running() {
            self.pause(now)
        } else {
            self.start(now)
        }
    }

    /// Stop and return to the start of the current phase.
    pub fn reset(&mut self) {
        self.run = RunState::Idle;
    }

    /// Reset everything back to a fresh focus session.
    pub fn reset_all(&mut self) {
        self.run = RunState::Idle;
        self.phase = Phase::Focus;
        self.cycle_done = 0;
    }

    /// Switch to `phase` (e.g. picked by the user), stopped at its start.
    /// Picking anything but a long break while one is due counts as skipping
    /// it, so the cycle restarts, as it does when a long break finishes.
    pub fn set_phase(&mut self, phase: Phase) {
        self.phase = phase;
        self.run = RunState::Idle;
        if phase != Phase::LongBreak && self.cfg.long_every > 0 && self.cycle_done >= self.cfg.long_every {
            self.cycle_done = 0;
        }
    }

    fn next_phase(&self, ended: Phase, cycle_done_after: u32) -> Phase {
        match ended {
            Phase::Focus => {
                if self.cfg.long_every > 0 && cycle_done_after >= self.cfg.long_every {
                    Phase::LongBreak
                } else {
                    Phase::ShortBreak
                }
            }
            Phase::ShortBreak | Phase::LongBreak => Phase::Focus,
        }
    }

    fn advance(&mut self, now: f64, completed: bool) -> PhaseEnded {
        let ended = self.phase;
        if ended == Phase::Focus && completed {
            self.cycle_done += 1;
        }
        let next = self.next_phase(ended, self.cycle_done);
        if ended == Phase::LongBreak {
            self.cycle_done = 0;
        }
        self.phase = next;
        let auto = if next.is_break() { self.cfg.auto_start_breaks } else { self.cfg.auto_start_focus };
        self.run = if auto { RunState::Running { ends_at: now + self.phase_duration(next) } } else { RunState::Idle };
        PhaseEnded { ended, next, auto_started: auto }
    }

    /// Jump to the next phase. Skipping focus does not count as a completed session.
    pub fn skip(&mut self, now: f64) -> PhaseEnded {
        self.advance(now, false)
    }

    /// Call periodically; returns `Some` when a phase finished.
    pub fn tick(&mut self, now: f64) -> Option<PhaseEnded> {
        if let RunState::Running { ends_at } = self.run {
            if now >= ends_at {
                return Some(self.advance(ends_at, true));
            }
        }
        None
    }

    /// Seconds until the displayed (ceil) countdown value changes, for timer scheduling.
    pub fn next_second_boundary(&self, now: f64) -> Option<f64> {
        match self.run {
            RunState::Running { .. } => {
                let rem = self.remaining(now);
                let frac = rem - rem.floor();
                Some(if frac < 1e-3 { 1.0 } else { frac })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pomo() -> Pomodoro {
        Pomodoro::new(PomodoroConfig { focus_min: 25, short_break_min: 5, long_break_min: 15, long_every: 2, auto_start_breaks: true, auto_start_focus: false })
    }

    #[test]
    fn start_pause_resume() {
        let mut p = pomo();
        assert_eq!(p.remaining(0.0), 1500.0);
        p.start(0.0);
        assert_eq!(p.remaining(100.0), 1400.0);
        p.pause(100.0);
        assert_eq!(p.remaining(500.0), 1400.0);
        p.start(500.0);
        assert_eq!(p.remaining(600.0), 1300.0);
        assert!((p.progress(600.0) - (200.0 / 1500.0) as f32).abs() < 1e-6);
    }

    #[test]
    fn cycle_to_long_break_and_back() {
        let mut p = pomo();
        p.start(0.0);
        let e = p.tick(1500.0).unwrap();
        assert_eq!((e.ended, e.next, e.auto_started), (Phase::Focus, Phase::ShortBreak, true));
        let e = p.tick(1500.0 + 300.0).unwrap();
        assert_eq!(e.next, Phase::Focus);
        assert!(!e.auto_started);
        assert_eq!(p.run, RunState::Idle);
        p.start(2000.0);
        let e = p.tick(3500.0).unwrap();
        assert_eq!(e.next, Phase::LongBreak);
        let e = p.tick(3500.0 + 900.0).unwrap();
        assert_eq!(e.ended, Phase::LongBreak);
        assert_eq!(p.cycle_done, 0);
    }

    #[test]
    fn skip_does_not_count() {
        let mut p = pomo();
        p.start(0.0);
        p.skip(10.0);
        assert_eq!(p.cycle_done, 0);
        assert_eq!(p.phase, Phase::ShortBreak);
    }

    #[test]
    fn set_phase_stops_at_start_of_phase() {
        let mut p = pomo();
        p.start(0.0);
        p.set_phase(Phase::ShortBreak);
        assert_eq!(p.phase, Phase::ShortBreak);
        assert_eq!(p.run, RunState::Idle);
        assert_eq!(p.remaining(50.0), 300.0);
        assert!(p.tick(99999.0).is_none());
        p.start(10.0);
        p.pause(20.0);
        p.set_phase(Phase::Focus);
        assert_eq!(p.run, RunState::Idle);
        assert_eq!(p.remaining(30.0), 1500.0);
    }

    #[test]
    fn set_phase_resets_a_due_cycle_unless_long_break() {
        let mut p = pomo(); // long_every = 2
        p.cycle_done = 2;
        p.set_phase(Phase::LongBreak);
        assert_eq!(p.cycle_done, 2, "taking the due long break keeps the count");
        p.set_phase(Phase::Focus);
        assert_eq!(p.cycle_done, 0, "skipping the due long break restarts the cycle");

        p.cycle_done = 3;
        p.set_phase(Phase::ShortBreak);
        assert_eq!(p.cycle_done, 0);

        p.cycle_done = 1;
        p.set_phase(Phase::ShortBreak);
        assert_eq!(p.cycle_done, 1, "not yet due: keep progress");
        p.set_phase(Phase::Focus);
        assert_eq!(p.cycle_done, 1);
    }

    #[test]
    fn set_phase_with_long_breaks_off_keeps_count() {
        let mut p = pomo();
        p.cfg.long_every = 0;
        p.cycle_done = 5;
        p.set_phase(Phase::Focus);
        assert_eq!(p.cycle_done, 5);
    }

    #[test]
    fn long_break_finishing_after_set_phase_resets_cycle() {
        let mut p = pomo();
        p.cycle_done = 2;
        p.set_phase(Phase::LongBreak);
        p.start(0.0);
        let e = p.tick(900.0).unwrap();
        assert_eq!((e.ended, e.next), (Phase::LongBreak, Phase::Focus));
        assert_eq!(p.cycle_done, 0);
    }

    #[test]
    fn tick_without_running_is_none() {
        let mut p = pomo();
        assert!(p.tick(99999.0).is_none());
    }

    #[test]
    fn second_boundary() {
        let mut p = pomo();
        p.start(0.0);
        // 1499.75 s remain → the ceil display flips from 25:00 to 24:59 in 0.75 s.
        let b = p.next_second_boundary(0.25).unwrap();
        assert!((b - 0.75).abs() < 1e-9, "{b}");
    }
}
