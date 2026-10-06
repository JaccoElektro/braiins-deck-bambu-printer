//! Bambu Lab widget — print progress, temperatures and every AMS spool, read
//! from the local read-only bridge. The printer's access code never reaches
//! the Deck.

mod manifest_params;
mod model;

#[cfg(target_arch = "wasm32")]
mod wasm_glue {
    use super::manifest_params;
    use super::model::{self, Alert, Phase, Watch};
    use std::cell::{Cell, RefCell};

    #[expect(
        clippy::wildcard_imports,
        reason = "widget render code uses many SDK exports and macros in one file"
    )]
    use bmc_wasm_sdk::*;

    const ACTIVE_MS: u32 = 10_000;
    const IDLE_MS: u32 = 60_000;

    const BAMBU_GREEN: Color = Color::from_hex(0x00_AE_42);
    const ORANGE: Color = Color::from_hex(0xFF_9F_0A);
    const RED: Color = Color::from_hex(0xFF_45_3A);
    const BLUE: Color = Color::from_hex(0x0A_84_FF);

    const DONE_SOUND: Audio = include_audio!("assets/confirmation.mp3");
    const TROUBLE_SOUND: Audio = include_audio!("assets/error_sound.mp3");

    struct Theme {
        background: Color,
        card: Color,
        rim: Color,
        text: Color,
        secondary: Color,
        tertiary: Color,
        track: Color,
    }

    const DARK: Theme = Theme {
        background: BLACK,
        card: Color::from_hex(0x1C_1C_1E),
        rim: Color::from_hex(0x2C_2C_2E),
        text: WHITE,
        secondary: Color::from_hex(0x98_98_9F),
        tertiary: Color::from_hex(0x63_63_66),
        track: Color::from_hex(0x2C_2C_2E),
    };

    const LIGHT: Theme = Theme {
        background: Color::from_hex(0xF2_F2_F7),
        card: WHITE,
        rim: Color::from_hex(0xE5_E5_EA),
        text: BLACK,
        secondary: Color::from_hex(0x6C_6C_70),
        tertiary: Color::from_hex(0xAE_AE_B2),
        track: Color::from_hex(0xE5_E5_EA),
    };

    #[derive(Clone, Default)]
    struct Spool {
        slot: i64,
        kind: String,
        name: String,
        color: Option<(u8, u8, u8)>,
        remain: Option<i64>,
        empty: bool,
        active: bool,
    }

    #[derive(Clone, Default)]
    struct Status {
        online: bool,
        state: String,
        state_label: String,
        stage_label: String,
        job: String,
        percent: f64,
        remaining_min: i64,
        eta: Option<i64>,
        layer: i64,
        layers: i64,
        nozzle: f64,
        nozzle_target: f64,
        bed: f64,
        bed_target: f64,
        nozzle_diameter: String,
        nozzle_type: String,
        speed: String,
        wifi_dbm: i64,
        print_error: i64,
        hms_count: i64,
        humidity: Option<f64>,
        ams_temp: Option<f64>,
        spools: Vec<Spool>,
    }

    thread_local! {
        static STATUS: RefCell<Option<Status>> = const { RefCell::new(None) };
        static WATCH: RefCell<Watch> = RefCell::new(Watch::default());
        static TROUBLE: Cell<bool> = const { Cell::new(false) };
        static POLL: Cell<Option<PollHandle>> = const { Cell::new(None) };
    }

    fn params() -> manifest_params::Params {
        manifest_params::Params::current()
    }

    fn lang() -> deckfx::Lang {
        match params().language {
            Some(manifest_params::Language::Nl) => deckfx::Lang::Nl,
            _ => deckfx::Lang::En,
        }
    }

    /// The English or Dutch wording, following the widget's language setting.
    fn tr(en: &'static str, nl: &'static str) -> &'static str {
        match lang() {
            deckfx::Lang::En => en,
            deckfx::Lang::Nl => nl,
        }
    }

    fn th() -> &'static Theme {
        if params().light_theme { &LIGHT } else { &DARK }
    }

    // ── Data ─────────────────────────────────────────────────────────────

        fn build(_h: PollHandle) -> Option<FetchSpec> {
        let base = params().bridge_url;
        if base.trim().is_empty() {
            return None;
        }
        // The bridge words printer states in the widget's language.
        let code = if lang() == deckfx::Lang::Nl { "nl" } else { "en" };
        Some(FetchSpec::get(fmt!("{}/bambu.json?lang={}", base.trim_end_matches('/'), code)).timeout(core::time::Duration::from_secs(5)))
    }

    fn spool(json: &JsonDoc, at: &str) -> Spool {
        let s = |k: &str| json.str(&fmt!("{}/{}", at, k)).unwrap_or_default();
        Spool {
            slot: json.i64(&fmt!("{}/slot", at)).unwrap_or(0),
            kind: s("type"),
            name: s("name"),
            color: model::hex_rgb(&s("color")),
            remain: json.i64(&fmt!("{}/remain", at)),
            empty: json.bool(&fmt!("{}/empty", at)).unwrap_or(true),
            active: json.bool(&fmt!("{}/active", at)).unwrap_or(false),
        }
    }

    fn parse(json: &JsonDoc) -> Status {
        let f = |k: &str| json.f64(k).unwrap_or(0.0);
        let i = |k: &str| json.i64(k).unwrap_or(0);
        let s = |k: &str| json.str(k).unwrap_or_default();
        let mut spools = Vec::new();
        let units = json.len("/ams").unwrap_or(0);
        for u in 0..units {
            for t in 0..json.len(&fmt!("/ams/{}/trays", u)).unwrap_or(0) {
                spools.push(spool(json, &fmt!("/ams/{}/trays/{}", u, t)));
            }
        }
        if json.kind("/external") == Some(JsonKind::Object) {
            spools.push(spool(json, "/external"));
        }
        Status {
            online: json.bool("/online").unwrap_or(false),
            state: s("/state"),
            state_label: s("/state_label"),
            stage_label: s("/stage_label"),
            job: s("/job"),
            percent: f("/percent"),
            remaining_min: i("/remaining_min"),
            eta: json.i64("/eta"),
            layer: i("/layer"),
            layers: i("/layers"),
            nozzle: f("/nozzle"),
            nozzle_target: f("/nozzle_target"),
            bed: f("/bed"),
            bed_target: f("/bed_target"),
            nozzle_diameter: s("/nozzle_diameter"),
            nozzle_type: s("/nozzle_type"),
            speed: s("/speed"),
            wifi_dbm: i("/wifi_dbm"),
            print_error: i("/print_error"),
            hms_count: i("/hms_count"),
            humidity: json.f64("/ams/0/humidity"),
            ams_temp: json.f64("/ams/0/temp"),
            spools,
        }
    }

    fn on_reply(handle: PollHandle, r: &FetchResponse) {
        renew_hold();
        let status = if r.ok() { Some(parse(&r.json())) } else { None };
        let (phase, active_remain) = status.as_ref().map_or((Phase::Offline, None), |s| {
            (model::phase(&s.state, s.online, s.print_error), s.spools.iter().find(|x| x.active).and_then(|x| x.remain))
        });
        handle.set_interval(if matches!(phase, Phase::Printing | Phase::Paused) { ACTIVE_MS } else { IDLE_MS });
        if let Some(alert) = WATCH.with(|w| w.borrow_mut().update(phase, active_remain)) {
            fire(alert);
        }
        if TROUBLE.get() && matches!(phase, Phase::Printing | Phase::Idle) {
            // Resumed or cleared at the printer: stop the red light.
            TROUBLE.set(false);
            hold_stop();
        }
        if status.is_some() || STATUS.with(|s| s.borrow().is_none()) {
            STATUS.with(|s| *s.borrow_mut() = status);
        } else {
            STATUS.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut() {
                    st.online = false;
                }
            });
        }
        request_frame();
    }

    fn fire(alert: Alert) {
        if !params().alerts {
            return;
        }
        let night = system::current().night_mode().unwrap_or(false);
        let sound = |a: &Audio| {
            if !night {
                audio_play(ensure_audio_registered(a), Volume::new(85));
            }
        };
        match alert {
            Alert::Done => {
                led::set_effect_global(LedEffect::Chase, BAMBU_GREEN, 2_000, Some(12_000));
                sound(&DONE_SOUND);
            }
            Alert::Trouble => {
                TROUBLE.set(true);
                hold_start(LedEffect::Breathe, RED, 2_500);
                sound(&TROUBLE_SOUND);
            }
            Alert::LowSpool => led::set_effect_global(LedEffect::Breathe, deckfx::wasm::led_orange(), 2_000, Some(10_000)),
        }
    }

    // ── Light strip (deckfx): alerts as renewed temporaries, never endless ──

    thread_local! {
        static HOLD: RefCell<deckfx::Hold> = RefCell::new(deckfx::Hold::default());
        static HELD: Cell<Option<(LedEffect, Color, u32)>> = const { Cell::new(None) };
        static SCENE_FX: RefCell<deckfx::SceneFx> = RefCell::new(deckfx::SceneFx::default());
    }

    /// Start a global alert that survives as long as `renew_hold` keeps being called.
    fn hold_start(effect: LedEffect, color: Color, period_ms: u32) {
        HELD.set(Some((effect, color, period_ms)));
        HOLD.with(|h| h.borrow_mut().clear());
        renew_hold();
    }

    fn renew_hold() {
        if let Some((effect, color, period)) = HELD.get() {
            HOLD.with(|h| deckfx::wasm::hold(&mut h.borrow_mut(), SystemTime::now().unix_secs, effect, color, period));
        }
    }

    /// End the alert; the ambient glow comes back on its own.
    fn hold_stop() {
        HELD.set(None);
        HOLD.with(|h| h.borrow_mut().clear());
        led::stop();
    }

    fn scene_entry(delta_ms: u32, enabled: bool, color: Color) {
        if enabled && SCENE_FX.with(|f| f.borrow_mut().on_render(delta_ms, SystemTime::now().unix_secs)) {
            deckfx::wasm::scene_effect(color);
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn init() {
        let h = register_poll(build, on_reply, PollConfig { interval_ms: Some(IDLE_MS), debounce_ms: 0, ..Default::default() });
        POLL.set(Some(h));
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn on_params_update() {
        if let Some(h) = POLL.get() {
            h.invalidate();
        }
        request_frame();
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn on_system_update() {
        request_frame();
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn on_touch() {
        request_frame();
    }

    // ── View ─────────────────────────────────────────────────────────────

    struct Scale {
        hero: u32,
        title: u32,
        body: u32,
        caption: u32,
        pad: f32,
        gap: f32,
        spool: f32,
    }

    fn scale(v: SizeVariant) -> Scale {
        match v {
            SizeVariant::Full => Scale { hero: 84, title: 30, body: 20, caption: 16, pad: 28.0, gap: 12.0, spool: 96.0 },
            SizeVariant::Large => Scale { hero: 54, title: 22, body: 16, caption: 13, pad: 18.0, gap: 8.0, spool: 44.0 },
            SizeVariant::Medium => Scale { hero: 48, title: 18, body: 14, caption: 12, pad: 14.0, gap: 8.0, spool: 34.0 },
            SizeVariant::Small => Scale { hero: 44, title: 16, body: 13, caption: 12, pad: 14.0, gap: 6.0, spool: 0.0 },
        }
    }

    fn phase_color(p: Phase) -> Color {
        match p {
            Phase::Printing => BLUE,
            Phase::Finished => BAMBU_GREEN,
            Phase::Paused => ORANGE,
            Phase::Failed | Phase::Offline => RED,
            Phase::Idle => th().secondary,
        }
    }

    fn pill(label: &str, color: Color, s: &Scale) -> Node {
        let dot = s.caption as f32 * 0.6;
        row(
            props!(gap: 6.0, cross_align: CrossAlign::Center, padding: 6.0, border_radius: 999.0, background: th().card),
            [
                canvas(props!(width: dot, height: dot), [Draw::circle(dot / 2.0, dot / 2.0, dot / 2.0, color)]),
                text(label, style!(size: s.caption, weight: FontWeight::BOLD, color: color)),
            ],
        )
    }

    fn bar(width: f32, height: f32, fraction: f64, color: Color) -> Node {
        #[expect(clippy::cast_possible_truncation, reason = "pixel width")]
        let filled = fraction.clamp(0.0, 1.0) as f32 * width;
        canvas(
            props!(width: width, height: height),
            [Draw::rect(0.0, 0.0, width, height, th().track), Draw::rect(0.0, 0.0, filled, height, color)],
        )
    }

    fn clock(unix: i64) -> String {
        let tz = system::current().timezone().map(Tz::from_runtime);
        format_time(SystemTime { unix_secs: unix }, FormatTimeOpts { timezone: tz, ..FormatTimeOpts::default() })
    }

    fn progress_block(st: &Status, p: Phase, s: &Scale, width: f32, compact: bool) -> Node {
        let t = th();
        let mut lines = vec![];
        if !st.job.is_empty() {
            lines.push(text(st.job.clone(), style!(size: s.body, weight: FontWeight::SEMIBOLD, color: t.text, text_overflow: TextOverflow::Ellipsis)));
        }
        let hero = if matches!(p, Phase::Idle | Phase::Offline) { st.state_label.clone() } else { fmt!("{} %", format_number!(st.percent, 0)) };
        lines.push(text(hero, style!(size: s.hero, weight: FontWeight::BOLD, color: t.text, family: FontFamily::DeckSans, line_height: 1.05, text_overflow: TextOverflow::Ellipsis)));
        if !matches!(p, Phase::Idle | Phase::Offline) {
            lines.push(bar(width, if compact { 6.0 } else { 10.0 }, st.percent / 100.0, phase_color(p)));
        }
        let mut detail = String::new();
        if st.layers > 0 && !matches!(p, Phase::Idle) {
            detail.push_str(&fmt!("{} {}/{}", tr("layer", "laag"), st.layer, st.layers));
        }
        if p == Phase::Printing {
            let left = model::duration(st.remaining_min, lang());
            detail.push_str(&match lang() {
                deckfx::Lang::En => fmt!(" · {} left", left),
                deckfx::Lang::Nl => fmt!(" · nog {}", left),
            });
            if let Some(eta) = st.eta {
                detail.push_str(&fmt!(" · {} {}", tr("done", "klaar"), clock(eta)));
            }
        }
        if !detail.is_empty() {
            lines.push(text(detail, style!(size: s.body, color: t.secondary, text_overflow: TextOverflow::Ellipsis)));
        }
        if !compact && !st.stage_label.is_empty() {
            lines.push(text(st.stage_label.clone(), style!(size: s.caption, weight: FontWeight::SEMIBOLD, color: phase_color(p))));
        }
        col(props!(gap: s.gap * 0.5), lines)
    }

    fn temp_card(label: &str, now: f64, target: f64, s: &Scale) -> Node {
        let t = th();
        let heating = target > 0.0;
        let mut children = vec![
            text(label, style!(size: s.caption, color: t.secondary)),
            text(fmt!("{}°", format_number!(now, 0)), style!(size: s.title, weight: FontWeight::BOLD, color: if heating { ORANGE } else { t.text }, family: FontFamily::DeckSans)),
        ];
        if heating {
            children.push(text(fmt!("→ {}°", format_number!(target, 0)), style!(size: s.caption, color: t.tertiary)));
        }
        col(props!(flex: 1.0, gap: 2.0, padding: s.gap, background: t.card, border_radius: 14.0, border_width: 1.0, border_color: t.rim), children)
    }

    fn info_card(label: &str, value: String, detail: &str, s: &Scale) -> Node {
        let t = th();
        col(
            props!(flex: 1.0, gap: 2.0, padding: s.gap, background: t.card, border_radius: 14.0, border_width: 1.0, border_color: t.rim),
            [
                text(label, style!(size: s.caption, color: t.secondary)),
                text(value, style!(size: s.title, weight: FontWeight::BOLD, color: t.text, family: FontFamily::DeckSans)),
                text(detail, style!(size: s.caption, color: t.tertiary, text_overflow: TextOverflow::Ellipsis)),
            ],
        )
    }

    /// A spool seen from the side: coloured filament ring around a hub.
    fn spool_icon(sp: &Spool, size: f32) -> Node {
        let t = th();
        let r = size / 2.0;
        let rgb = sp.color.unwrap_or((60, 60, 64));
        let fill = Color::from_rgb(rgb.0, rgb.1, rgb.2);
        let mut draws = vec![];
        if sp.active {
            draws.push(Draw::circle(r, r, r, BAMBU_GREEN));
        }
        let rim = if model::luminance(rgb) < 0.15 { Color::from_hex(0x48_48_4A) } else { t.rim };
        draws.push(Draw::circle(r, r, r * 0.9, rim));
        if !sp.empty {
            draws.push(Draw::circle(r, r, r * 0.84, fill));
        }
        draws.push(Draw::circle(r, r, r * 0.34, t.card));
        draws.push(Draw::circle(r, r, r * 0.14, t.rim));
        canvas(props!(width: size, height: size), draws)
    }

    fn spool_card(sp: &Spool, s: &Scale, compact: bool) -> Node {
        let t = th();
        let label = if sp.slot == 0 { tr("Ext.", "Extern").to_string() } else { fmt!("A{}", sp.slot) };
        let remain = sp.remain.map_or_else(|| "?".to_string(), |r| fmt!("{} %", r));
        let low = sp.remain.is_some_and(|r| r < model::LOW_SPOOL);
        let mut children = vec![spool_icon(sp, s.spool)];
        if sp.empty {
            children.push(text(fmt!("{} · {}", label, tr("empty", "leeg")), style!(size: s.caption, color: t.tertiary)));
        } else if compact {
            children.push(text(remain, style!(size: s.caption, weight: FontWeight::BOLD, color: if low { ORANGE } else { t.text })));
        } else {
            children.push(text(sp.kind.clone(), style!(size: s.body, weight: FontWeight::BOLD, color: t.text, text_overflow: TextOverflow::Ellipsis)));
            children.push(text(fmt!("{} · {}", label, if sp.name != sp.kind { sp.name.as_str() } else { "" }).trim_end_matches(" · ").to_string(), style!(size: s.caption, color: t.secondary, text_overflow: TextOverflow::Ellipsis)));
            #[expect(clippy::cast_precision_loss, reason = "percentage")]
            let frac = sp.remain.map_or(0.0, |r| r as f64 / 100.0);
            children.push(bar(s.spool + 30.0, 5.0, frac, if low { ORANGE } else { BAMBU_GREEN }));
            children.push(text(remain, style!(size: s.caption, weight: FontWeight::SEMIBOLD, color: if low { ORANGE } else { t.secondary })));
        }
        col(props!(flex: 1.0, gap: 4.0, cross_align: CrossAlign::Center), children)
    }

    fn ams_panel(st: &Status, s: &Scale, compact: bool) -> Node {
        let t = th();
        let cards: Vec<Node> = st.spools.iter().map(|sp| spool_card(sp, s, compact)).collect();
        let mut caption = String::from("AMS");
        if let Some(h) = st.humidity {
            caption.push_str(&fmt!(" · {} % {}", format_number!(h, 0), tr("RH", "RV")));
        }
        if let Some(tmp) = st.ams_temp {
            caption.push_str(&fmt!(" · {}°", format_number!(tmp, 0)));
        }
        let humid = st.humidity.is_some_and(|h| h > 40.0);
        let mut children = vec![row(
            props!(justify_content: Justify::SpaceBetween),
            [
                text(caption, style!(size: s.caption, weight: FontWeight::BOLD, color: t.secondary)),
                text(if humid { tr("replace desiccant", "droogmiddel vervangen") } else { "" }, style!(size: s.caption, weight: FontWeight::BOLD, color: ORANGE)),
            ],
        )];
        children.push(row(props!(gap: s.gap, flex: 1.0, cross_align: CrossAlign::Center), cards));
        col(props!(flex: 1.0, gap: s.gap, padding: s.gap, background: t.card, border_radius: 18.0, border_width: 1.0, border_color: t.rim), children)
    }

    fn header(st: &Status, p: Phase, s: &Scale) -> Node {
        let t = th();
        let name = params().printer_name;
        let label = if p == Phase::Offline { "Offline".to_string() } else { st.state_label.clone() };
        let mut right = vec![];
        if st.hms_count > 0 || st.print_error != 0 {
            right.push(text(fmt!("{} {}", st.hms_count.max(1), tr("alert(s)", "melding(en)")), style!(size: s.caption, weight: FontWeight::BOLD, color: RED)));
        }
        if s.title >= 24 {
            right.push(text(fmt!("{} · wifi {} dBm", st.speed, st.wifi_dbm), style!(size: s.caption, color: t.tertiary)));
        }
        right.push(pill(&label, phase_color(p), s));
        row(
            props!(cross_align: CrossAlign::Center, gap: s.gap),
            [
                text(name, style!(size: s.title, weight: FontWeight::BOLD, color: t.text, flex: 1.0, text_overflow: TextOverflow::Ellipsis)),
                row(props!(gap: s.gap, cross_align: CrossAlign::Center), right),
            ],
        )
    }

    fn view(st: &Status, variant: SizeVariant, w: f32) -> Node {
        let t = th();
        let s = scale(variant);
        let p = model::phase(&st.state, st.online, st.print_error);
        let temps = |s: &Scale| {
            row(
                props!(gap: s.gap),
                [
                    temp_card("Nozzle", st.nozzle, st.nozzle_target, s),
                    temp_card("Bed", st.bed, st.bed_target, s),
                    // The P1S has no chamber sensor; the fitted nozzle is what matters (CF needs hardened steel).
                    info_card(tr("Nozzle", "Tip"), if st.nozzle_diameter.is_empty() { "--".into() } else { fmt!("{} mm", st.nozzle_diameter.replace('.', ",")) }, &st.nozzle_type, s),
                ],
            )
        };
        let body = match variant {
            SizeVariant::Full => {
                let left_w = w * 0.42;
                row(
                    props!(flex: 1.0, gap: s.pad),
                    [
                        col(
                            props!(width: left_w, gap: s.gap, justify_content: Justify::SpaceBetween),
                            [progress_block(st, p, &s, left_w, false), temps(&s)],
                        ),
                        ams_panel(st, &s, false),
                    ],
                )
            }
            SizeVariant::Large => col(
                props!(flex: 1.0, gap: s.gap),
                [progress_block(st, p, &s, w - 2.0 * s.pad, false), ams_panel(st, &s, false), temps(&s)],
            ),
            SizeVariant::Medium => row(
                props!(flex: 1.0, gap: s.gap),
                [
                    col(props!(width: w * 0.45, justify_content: Justify::Center), [progress_block(st, p, &s, w * 0.45, true)]),
                    ams_panel(st, &s, true),
                ],
            ),
            SizeVariant::Small => col(props!(flex: 1.0, justify_content: Justify::Center), [progress_block(st, p, &s, w - 2.0 * s.pad, true)]),
        };
        col(props!(flex: 1.0, background: t.background, padding: s.pad, gap: s.gap), [header(st, p, &s), body])
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn render(delta_ms: u32) {
        renew_hold();
        scene_entry(delta_ms, manifest_params::Params::current().scene_fx.unwrap_or(true), BAMBU_GREEN);
        let ws = widget_size();
        #[expect(clippy::cast_precision_loss, reason = "viewport sizes are small integers")]
        let w = ws.width as f32;
        let face = STATUS.with(|st| match st.borrow().as_ref() {
            Some(st) => view(st, ws.variant, w),
            None => center(
                props!(flex: 1.0, background: th().background),
                [text(
                    if params().bridge_url.trim().is_empty() { tr("Set the bridge URL in the widget settings", "Vul de bridge-URL in bij de widget") } else { tr("Connecting to the printer…", "Verbinden met de printer…") },
                    style!(size: 18, color: th().secondary),
                )],
            ),
        });
        let mut layers = vec![face];
        // A whole-face touch area only while trouble waits for a tap (it costs CPU).
        if TROUBLE.get() {
            layers.push(touchable("tap", props!(inset_top: 0.0, inset_left: 0.0, inset_right: 0.0, inset_bottom: 0.0), []));
        }
        layers.extend(POLL.get().and_then(|h| deckfx::wasm::offline_badge(&[h], lang())));
        let root = col(props!(background: th().background), layers);
        let result = render_ui(ws.width, ws.height, root);
        if result.clicks.contains_key("tap") && TROUBLE.get() {
            TROUBLE.set(false);
            hold_stop();
            request_frame();
        }
        // Countdown and ETA keep moving between polls.
        request_frame_after(30_000);
    }
}
