//! Small helpers shared by Deck widgets: light-strip conventions and an
//! "offline" badge.
//!
//! The light strip has one *endless* slot per tier and it is last-write-wins,
//! with no notice to the loser. So:
//!
//! - **Alerts** are global *temporaries*, re-queued while the condition holds
//!   ([`Hold`]). When they end, the host falls back to whatever ambient light
//!   another widget keeps on its own.
//! - **Scene effects** are *local* temporaries: the host keeps them queued
//!   until their scene is on screen, so queueing one on the first render after
//!   a dormant stretch plays it exactly when the scene slides in ([`SceneFx`]).

/// A render this long after the previous one means the widget was off screen.
pub const DORMANT_GAP_MS: u32 = 35_000;
/// Never queue two scene effects within this window (pre-render + entry).
pub const SCENE_FX_COOLDOWN_S: i64 = 90;

/// Decides when a scene effect is due. Pure; host-testable.
#[derive(Debug, Default)]
pub struct SceneFx {
    rendered: bool,
    last_fired: Option<i64>,
}

impl SceneFx {
    /// Feed every `render(delta_ms)`; true when an entry effect should queue.
    pub fn on_render(&mut self, delta_ms: u32, now_s: i64) -> bool {
        let first = !self.rendered;
        self.rendered = true;
        let returning = first || delta_ms > DORMANT_GAP_MS;
        let cooled = self.last_fired.is_none_or(|t| now_s - t >= SCENE_FX_COOLDOWN_S);
        if returning && cooled {
            self.last_fired = Some(now_s);
            true
        } else {
            false
        }
    }
}

/// Keeps a global alert alive as a chain of temporaries while its condition
/// holds. Pure scheduling; the wasm side issues the effect when told to.
#[derive(Debug, Default)]
pub struct Hold {
    until: Option<i64>,
}

impl Hold {
    /// Chunk length of one queued temporary.
    pub const CHUNK_S: i64 = 120;

    /// Call while the condition holds; true when a new chunk must be queued.
    pub fn due(&mut self, now_s: i64) -> bool {
        if self.until.is_some_and(|u| now_s < u - 3) {
            return false;
        }
        self.until = Some(now_s + Self::CHUNK_S);
        true
    }

    /// Condition cleared (the caller stops its LED requests).
    pub fn clear(&mut self) {
        self.until = None;
    }

    #[must_use]
    pub fn active(&self) -> bool {
        self.until.is_some()
    }
}

/// Orange as the strip renders it: deeper than the screen's #F7931A, which
/// the LEDs show yellow.
pub const LED_ORANGE: (u8, u8, u8) = (0xFF, 0x46, 0x00);

/// Language of the text a widget shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Lang {
    #[default]
    En,
    Nl,
}

/// "12 min", "3 h", "5 days" (or Dutch): how old shown data is, in the
/// largest whole unit.
#[must_use]
pub fn age_text(secs: u64, lang: Lang) -> String {
    let (n, unit) = match (secs, lang) {
        (0..3_600, _) => ((secs / 60).max(1), "min"),
        (3_600..172_800, Lang::En) => (secs / 3_600, "h"),
        (3_600..172_800, Lang::Nl) => (secs / 3_600, "uur"),
        (_, Lang::En) => (secs / 86_400, "days"),
        (_, Lang::Nl) => (secs / 86_400, "dagen"),
    };
    format!("{n} {unit}")
}

#[cfg(target_arch = "wasm32")]
pub mod wasm {
    use super::{Hold, Lang};
    #[expect(clippy::wildcard_imports, reason = "UI builders and macros come as one SDK surface")]
    use bmc_wasm_sdk::*;
    use bmc_wasm_sdk::led::{self, LedEffect};

    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "chunk is a small positive constant")]
    const CHUNK_MS: u32 = (Hold::CHUNK_S * 1_000) as u32;

    /// Entry cue for a scene: one slow, soft breath in the widget's accent colour.
    pub fn scene_effect(color: Color) {
        led::set_effect(LedEffect::Breathe, color, 2_400, Some(2_400));
    }

    /// Keep `effect` running globally while the caller's condition holds.
    pub fn hold(h: &mut Hold, now_s: i64, effect: LedEffect, color: Color, period_ms: u32) {
        if h.due(now_s) {
            led::set_effect_global(effect, color, period_ms, Some(CHUNK_MS));
        }
    }

    /// [`super::LED_ORANGE`] as a colour.
    #[must_use]
    pub fn led_orange() -> Color {
        let (r, g, b) = super::LED_ORANGE;
        Color::from_rgb(r, g, b)
    }

    /// A quiet pill in the top-right corner while any of `polls` is stale (or
    /// has never loaded and keeps failing): the widget keeps its last values,
    /// but never pretends they are live. Push it as the last child of the root.
    #[must_use]
    pub fn offline_badge(polls: &[PollHandle], lang: Lang) -> Option<Node> {
        let stale: Vec<PollHandle> = polls.iter().copied().filter(|h| h.is_stale() || h.is_offline()).collect();
        if stale.is_empty() {
            return None;
        }
        let age = stale.iter().filter_map(|h| h.last_success_age()).map(|d| d.as_secs()).max();
        let (offline, old) = match lang {
            Lang::En => ("No connection", "old"),
            Lang::Nl => ("Geen verbinding", "oud"),
        };
        let label = match age {
            Some(secs) => fmt!("{} · {} {}", offline, super::age_text(secs, lang), old),
            None => offline.into(),
        };
        let amber = Color::from_hex(0xFF_B0_2E);
        Some(row(
            props!(
                inset_top: 10.0,
                inset_right: 12.0,
                gap: 8.0,
                padding: 7.0,
                background: Color::from_rgba(0x1C, 0x1A, 0x18, 0xE6),
                border_radius: 14.0,
                border_width: 1.0,
                border_color: Color::from_rgba(0xFF, 0xB0, 0x2E, 0x55),
                cross_align: CrossAlign::Center,
            ),
            [
                canvas(props!(width: 10.0, height: 10.0), [Draw::circle(5.0, 5.0, 4.0, amber)]),
                text(label, style!(size: 13, weight: FontWeight::SEMIBOLD, color: amber)),
            ],
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_fx_fires_on_first_render_and_after_dormancy() {
        let mut fx = SceneFx::default();
        assert!(fx.on_render(0, 1_000), "first render");
        assert!(!fx.on_render(2_000, 1_002), "steady while visible");
        assert!(!fx.on_render(40_000, 1_042), "pre-render then entry within cooldown");
        assert!(fx.on_render(300_000, 1_400), "back after a full cycle");
    }

    #[test]
    fn hold_requeues_per_chunk() {
        let mut h = Hold::default();
        assert!(h.due(0));
        assert!(!h.due(60));
        assert!(h.due(118), "renew just before the chunk ends");
        h.clear();
        assert!(!h.active());
        assert!(h.due(130));
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(age_text(20, Lang::En), "1 min");
        assert_eq!(age_text(12 * 60 + 5, Lang::Nl), "12 min");
        assert_eq!(age_text(3 * 3_600 + 100, Lang::En), "3 h");
        assert_eq!(age_text(3 * 3_600 + 100, Lang::Nl), "3 uur");
        assert_eq!(age_text(5 * 86_400, Lang::En), "5 days");
    }
}
