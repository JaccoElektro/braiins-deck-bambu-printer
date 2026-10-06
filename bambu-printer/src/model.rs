//! Pure printer logic: status transitions → alerts, and formatting. Host-testable.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Printing,
    Paused,
    Finished,
    Failed,
    Offline,
}

#[must_use]
pub fn phase(state: &str, online: bool, print_error: i64) -> Phase {
    if !online {
        return Phase::Offline;
    }
    match state {
        "RUNNING" | "PREPARE" | "SLICING" => {
            if print_error != 0 { Phase::Paused } else { Phase::Printing }
        }
        "PAUSE" => Phase::Paused,
        "FINISH" => Phase::Finished,
        "FAILED" => Phase::Failed,
        _ => Phase::Idle,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alert {
    Done,
    Trouble,
    LowSpool,
}

/// Remaining-percentage under which the active spool counts as nearly empty.
pub const LOW_SPOOL: i64 = 10;

#[derive(Debug, Default)]
pub struct Watch {
    last: Option<Phase>,
    low_warned: bool,
}

impl Watch {
    /// Feed a reading; returns an alert on a meaningful transition. The first
    /// reading only seeds the state, so a restart never replays old news.
    pub fn update(&mut self, phase: Phase, active_remain: Option<i64>) -> Option<Alert> {
        let before = self.last.replace(phase);
        let Some(before) = before else {
            return None;
        };
        if phase != Phase::Printing {
            self.low_warned = false;
        }
        match (before, phase) {
            (Phase::Printing | Phase::Paused, Phase::Finished) => Some(Alert::Done),
            (b, Phase::Failed | Phase::Paused) if b != phase => Some(Alert::Trouble),
            (_, Phase::Printing) => {
                if !self.low_warned && active_remain.is_some_and(|r| (0..LOW_SPOOL).contains(&r)) {
                    self.low_warned = true;
                    Some(Alert::LowSpool)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

/// "1 h 23 min" (Dutch "1 u 23 min"), "45 min", "< 1 min".
#[must_use]
pub fn duration(minutes: i64, lang: deckfx::Lang) -> String {
    let hour = if lang == deckfx::Lang::Nl { "u" } else { "h" };
    match minutes {
        m if m <= 0 => "< 1 min".to_string(),
        m if m < 60 => format!("{m} min"),
        m => format!("{} {hour} {:02} min", m / 60, m % 60),
    }
}

/// Parse "RRGGBB" into RGB; `None` for anything else.
#[must_use]
pub fn hex_rgb(hex: &str) -> Option<(u8, u8, u8)> {
    if hex.len() != 6 {
        return None;
    }
    let c = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some((c(0)?, c(2)?, c(4)?))
}

/// Perceived brightness 0..1 — to pick a contrasting ring around a spool.
#[must_use]
pub fn luminance((r, g, b): (u8, u8, u8)) -> f32 {
    (0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)) / 255.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases() {
        assert_eq!(phase("RUNNING", true, 0), Phase::Printing);
        assert_eq!(phase("RUNNING", true, 50_348_044), Phase::Paused);
        assert_eq!(phase("FINISH", true, 0), Phase::Finished);
        assert_eq!(phase("FINISH", false, 0), Phase::Offline);
        assert_eq!(phase("IDLE", true, 0), Phase::Idle);
    }

    #[test]
    fn finishing_a_print_alerts_once() {
        let mut w = Watch::default();
        assert_eq!(w.update(Phase::Printing, Some(50)), None, "seed");
        assert_eq!(w.update(Phase::Finished, None), Some(Alert::Done));
        assert_eq!(w.update(Phase::Finished, None), None);
    }

    #[test]
    fn restart_on_finished_printer_is_quiet() {
        let mut w = Watch::default();
        assert_eq!(w.update(Phase::Finished, None), None);
        assert_eq!(w.update(Phase::Idle, None), None);
    }

    #[test]
    fn trouble_and_low_spool() {
        let mut w = Watch::default();
        w.update(Phase::Printing, Some(40));
        assert_eq!(w.update(Phase::Printing, Some(8)), Some(Alert::LowSpool));
        assert_eq!(w.update(Phase::Printing, Some(7)), None, "warn once per print");
        assert_eq!(w.update(Phase::Paused, Some(7)), Some(Alert::Trouble));
        assert_eq!(w.update(Phase::Failed, None), Some(Alert::Trouble));
    }

    #[test]
    fn formatting() {
        assert_eq!(duration(83, deckfx::Lang::Nl), "1 u 23 min");
        assert_eq!(duration(83, deckfx::Lang::En), "1 h 23 min");
        assert_eq!(duration(45, deckfx::Lang::En), "45 min");
        assert_eq!(duration(0, deckfx::Lang::En), "< 1 min");
        assert_eq!(hex_rgb("0086D6"), Some((0, 0x86, 0xD6)));
        assert_eq!(hex_rgb("zz"), None);
        assert!(luminance((255, 255, 255)) > 0.9 && luminance((0, 0, 0)) < 0.1);
    }
}
