//! Opening on the Cube (canvas boards O-Launcher, O-Glow-B, O-Finale-B,
//! O-Stall and O-List).
//!
//! After the PIN or passkey the Cube tumbles to its bottom face, the one face
//! nothing used yet, and grows to the 700px loading face. Its edge glows in
//! slowly, then breathes, faster and brighter with each loader stage. Once the
//! Cube is ready the face rushes toward the viewer while the screen washes in
//! the edge colour, and the App shows as the wash fades.
//!
//! Without the Launcher floor behind it (List view, which reduced motion
//! forces) the same content fills the screen inside a glowing frame, with no
//! tumble and no finale; under reduced motion the frame does not flicker or
//! breathe either.
//!
//! This is presentation only. The loader's own `Step` decides the stage, and
//! nothing here starts, stops or retries a load.

use std::time::{Duration, Instant};

use super::launcher_catalog::LauncherSceneContext;
use super::launcher_gpu::Glow;
use super::launcher_scene::{self, Pose};

/// `.cube { transition: transform 820ms }`: unlock face to loading face.
const TUMBLE: Duration = launcher_scene::TURN;
/// `.fc.load { animation: loadin 700ms var(--spin) 500ms }`.
const CONTENT_DELAY: Duration = Duration::from_millis(500);
const CONTENT_FADE: Duration = Duration::from_millis(700);
/// `.lload { animation: loadin 500ms var(--spin) }`.
const FRAME_FADE: Duration = Duration::from_millis(500);
/// The breath fades in over this long once its delay is up, so the light
/// starts slow and soft rather than with a flicker.
const GLOW_IN: Duration = Duration::from_millis(1600);
/// One breath at the first stage, and at the last before Ready. Each stage
/// in between breathes faster, so the light quickens as the load goes on.
const BREATHE_SLOW: Duration = Duration::from_millis(2600);
const BREATHE_FAST: Duration = Duration::from_millis(1100);
/// `::before { transition: opacity 900ms var(--spin) }`.
const LEVEL_FADE: Duration = Duration::from_millis(900);
/// "Ready" holds this long at full light before the finale.
const READY_BEAT: Duration = Duration::from_millis(600);
/// `.cb.finale .cube { transition: transform 950ms }`.
const RUSH: Duration = Duration::from_millis(950);
/// `.flash.flat { animation: flat 1700ms ease-out }`.
const WASH: Duration = Duration::from_millis(1700);
/// The App replaces the stage under the wash at its peak.
const APP_AT: Duration = Duration::from_millis(900);
/// A stage held this long offers Cancel.
pub(crate) const STALL: Duration = Duration::from_secs(15);
/// Waiting on Spark this long offers to go on without it, so a Spark or
/// Breez outage never keeps a user from the rest of their Cube.
pub(crate) const SPARK_WAIT: Duration = Duration::from_secs(16);

/// `--lvl` for each stage index.
const LEVELS: [f32; 6] = [0.35, 0.5, 0.65, 0.8, 0.9, 1.0];

/// `@keyframes flat`, as (offset, opacity), each segment `ease-out`.
const WASH_KEYS: [(f32, f32); 4] = [(0.0, 0.0), (0.42, 0.92), (0.55, 0.92), (1.0, 0.0)];
const EASE_OUT: (f32, f32, f32, f32) = (0.0, 0.0, 0.58, 1.0);
const EASE_IN_OUT: (f32, f32, f32, f32) = (0.42, 0.0, 0.58, 1.0);

/// The stage line, quoted from the loader's own view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    Connecting,
    StartingDaemon,
    Scanning,
    Syncing,
    AlmostThere,
    /// The App is up and its Spark wallet has not returned a synced
    /// balance yet.
    Spark,
    Ready,
}

impl Stage {
    pub fn text(self) -> &'static str {
        match self {
            Stage::Connecting => "Connecting to daemon...",
            Stage::StartingDaemon => "Starting daemon...",
            Stage::Scanning => "Scanning the blockchain...",
            Stage::Syncing => "Syncing blockchain...",
            Stage::AlmostThere => "Almost there...",
            Stage::Spark => "Loading your Spark wallet...",
            Stage::Ready => "Ready",
        }
    }

    fn index(self) -> usize {
        match self {
            Stage::Connecting => 0,
            Stage::StartingDaemon | Stage::Scanning => 1,
            Stage::Syncing => 2,
            Stage::AlmostThere => 3,
            Stage::Spark => 4,
            Stage::Ready => 5,
        }
    }

    fn level(self) -> f32 {
        LEVELS[self.index()]
    }

    /// How far along the breath is, 0 at the first stage to 1 at the last
    /// before Ready: it sets the breath's speed and depth.
    fn intensity(self) -> f32 {
        (self.index() as f32 / 4.0).min(1.0)
    }

    /// How long this stage runs before the face offers a way out.
    fn wait(self) -> Duration {
        if self == Stage::Spark {
            SPARK_WAIT
        } else {
            STALL
        }
    }

    fn breath(self) -> Duration {
        BREATHE_SLOW.mul_f32(1.0 - self.intensity()) + BREATHE_FAST.mul_f32(self.intensity())
    }
}

/// One Cube's opening, from the end of the unlock to the App.
#[derive(Debug, Clone)]
pub(crate) struct Opening {
    /// The Launcher page the Cube was unlocked on. `None` opens full screen.
    floor: Option<LauncherSceneContext>,
    reduced: bool,
    started: Instant,
    stage: Stage,
    stage_since: Instant,
    level_from: f32,
    level_since: Instant,
    intensity_from: f32,
    /// Breaths taken up to `breath_since`, at the previous stages' speeds,
    /// so a quicker stage carries on from the same point in the breath.
    breaths: f32,
    breath_since: Instant,
    ready_since: Option<Instant>,
    /// An error page held the stall wait; the next stage set, even the same
    /// one after a Retry, starts it afresh.
    held: bool,
}

impl Opening {
    pub fn new(
        floor: Option<LauncherSceneContext>,
        reduced: bool,
        stage: Stage,
        now: Instant,
    ) -> Self {
        let mut opening = Self {
            floor,
            reduced,
            started: now,
            stage,
            stage_since: now,
            level_from: 0.0,
            level_since: now,
            intensity_from: stage.intensity(),
            breaths: 0.0,
            breath_since: now,
            ready_since: None,
            held: false,
        };
        if stage == Stage::Ready {
            opening.ready_since = Some(now);
        }
        opening
    }

    pub fn floor(&self) -> Option<&LauncherSceneContext> {
        self.floor.as_ref()
    }

    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// The tumble and the finale run only on the floor, with motion allowed.
    fn moving(&self) -> bool {
        self.floor.is_some() && !self.reduced
    }

    pub fn set_stage(&mut self, stage: Stage, now: Instant) {
        let held = std::mem::take(&mut self.held);
        if stage == self.stage {
            // Back from an error page, however long ago its last hold was
            // (a background tab gets no ticks), the wait starts now.
            if held {
                self.stage_since = now;
            }
            return;
        }
        self.level_from = self.level(now);
        self.intensity_from = self.intensity(now);
        self.breaths = self.breaths_at(now);
        self.breath_since = now;
        self.level_since = now;
        self.stage = stage;
        self.stage_since = now;
        if stage == Stage::Ready {
            self.ready_since.get_or_insert(now);
        } else {
            // A Retry after an error starts the stages over.
            self.ready_since = None;
        }
    }

    /// Restart the stall wait without changing the stage: while the loader
    /// shows an error, nothing is stalled.
    pub fn hold(&mut self, now: Instant) {
        self.stage_since = now;
        self.held = true;
    }

    fn since(&self, at: Instant, now: Instant) -> Duration {
        now.saturating_duration_since(at)
    }

    /// The tumble's eased progress.
    pub fn tumble(&self, now: Instant) -> f32 {
        if !self.moving() {
            return 1.0;
        }
        launcher_scene::ease(self.since(self.started, now).as_secs_f32() / TUMBLE.as_secs_f32())
    }

    /// The loading content's opacity as it fades in.
    pub fn content(&self, now: Instant) -> f32 {
        if self.reduced {
            return 1.0;
        }
        let t = self.since(self.started, now);
        let (delay, fade) = if self.floor.is_some() {
            (CONTENT_DELAY, CONTENT_FADE)
        } else {
            (Duration::ZERO, FRAME_FADE)
        };
        let shown =
            launcher_scene::ease(t.saturating_sub(delay).as_secs_f32() / fade.as_secs_f32());
        // The face's content does not scale with the rush; it gives way to
        // the wash instead.
        shown * (1.0 - (self.rush(now) * 6.0).min(1.0))
    }

    /// `::before`: the steady light, easing to each stage's `--lvl`.
    fn level(&self, now: Instant) -> f32 {
        let to = self.stage.level();
        if self.reduced {
            return to;
        }
        let t = launcher_scene::ease(
            self.since(self.level_since, now).as_secs_f32() / LEVEL_FADE.as_secs_f32(),
        );
        self.level_from + (to - self.level_from) * t
    }

    /// The stage's intensity, easing from the last stage's like the level.
    fn intensity(&self, now: Instant) -> f32 {
        let to = self.stage.intensity();
        let t = launcher_scene::ease(
            self.since(self.level_since, now).as_secs_f32() / LEVEL_FADE.as_secs_f32(),
        );
        self.intensity_from + (to - self.intensity_from) * t
    }

    /// When the breath starts: after the tumble has begun on the floor.
    fn breath_start(&self) -> Instant {
        self.started
            + if self.floor.is_some() {
                Duration::from_millis(700)
            } else {
                Duration::from_millis(300)
            }
    }

    /// Breaths taken by `now`, each stage at its own speed.
    fn breaths_at(&self, now: Instant) -> f32 {
        let from = self.breath_since.max(self.breath_start());
        self.breaths + self.since(from, now).as_secs_f32() / self.stage.breath().as_secs_f32()
    }

    /// `::after`: dark until the breath starts, then a slow glow in and a
    /// breath that quickens and deepens with each stage; full on at Ready
    /// (`.peak`). Reduced motion has no animation, so no `::after`.
    fn pulse(&self, now: Instant) -> f32 {
        if self.reduced {
            return 0.0;
        }
        if self.stage == Stage::Ready {
            return 1.0;
        }
        let start = self.breath_start();
        if now <= start {
            return 0.0;
        }
        let glow_in =
            launcher_scene::ease(self.since(start, now).as_secs_f32() / GLOW_IN.as_secs_f32());
        let phase = self.breaths_at(now).fract();
        // Low at the start of each breath, full at its middle, each half
        // ease-in-out.
        let half = launcher_scene::bezier((phase * 2.0).min(2.0 - phase * 2.0), EASE_IN_OUT);
        let intensity = self.intensity(now);
        let low = 0.2 + 0.2 * intensity;
        let high = 0.6 + 0.4 * intensity;
        glow_in * (low + (high - low) * half)
    }

    pub(super) fn glow(&self, color: [u8; 3], now: Instant) -> Glow {
        Glow {
            color,
            level: self.level(now),
            pulse: self.pulse(now),
        }
    }

    /// A way out is offered once one stage has run past its wait: Cancel
    /// after [`STALL`], or going on without Spark after [`SPARK_WAIT`].
    pub fn stalled(&self, now: Instant) -> bool {
        self.stage != Stage::Ready && self.since(self.stage_since, now) >= self.stage.wait()
    }

    /// Time into the finale, once Ready has held for its beat.
    fn finale(&self, now: Instant) -> Option<Duration> {
        let ready = self.ready_since.filter(|_| self.moving())?;
        let start = ready + READY_BEAT;
        (now >= start).then(|| now - start)
    }

    /// The face's rush toward the viewer, eased in.
    pub fn rush(&self, now: Instant) -> f32 {
        self.finale(now).map_or(0.0, |t| {
            launcher_scene::bezier(
                t.as_secs_f32() / RUSH.as_secs_f32(),
                launcher_scene::RUSH_EASE,
            )
        })
    }

    /// The edge-colour wash over the whole window, `.flash.flat`.
    pub fn wash(&self, now: Instant) -> f32 {
        self.finale(now).filter(|t| *t < WASH).map_or(0.0, |t| {
            keyframes(
                &WASH_KEYS,
                t.as_secs_f32() / WASH.as_secs_f32(),
                Some(EASE_OUT),
            )
        })
    }

    /// The opening Cube's pose.
    pub(super) fn pose(&self, now: Instant) -> Pose {
        Pose::opening(self.tumble(now), self.rush(now))
    }

    /// The other Cubes on the page, stepping further back.
    pub(super) fn peer_pose(&self, now: Instant) -> Pose {
        Pose::deep(self.tumble(now))
    }

    /// Whether the App, once it exists, is drawn instead of the stage.
    pub fn shows_app(&self, now: Instant) -> bool {
        self.ready_since.is_some()
            && (!self.moving() || self.finale(now).is_some_and(|t| t >= APP_AT))
    }

    /// The finale is over, or there is none: the opening can be dropped.
    pub fn done(&self, now: Instant) -> bool {
        self.ready_since.is_some()
            && (!self.moving() || self.finale(now).is_some_and(|t| t >= WASH))
    }

    /// Whether the next frame changes position or size, rather than only
    /// light: the tumble, the Ready beat and the finale need every frame.
    pub fn needs_frames(&self, now: Instant) -> bool {
        self.moving()
            && (self.since(self.started, now) < TUMBLE + CONTENT_DELAY + CONTENT_FADE
                || self.ready_since.is_some())
    }

    /// Whether the light still changes on its own: the flicker and breath.
    pub fn breathes(&self) -> bool {
        !self.reduced
    }
}

/// Linear (or `ease`d per segment) interpolation of CSS keyframes at `t`.
fn keyframes(keys: &[(f32, f32)], t: f32, ease: Option<(f32, f32, f32, f32)>) -> f32 {
    let t = t.clamp(0.0, 1.0);
    for pair in keys.windows(2) {
        let ((t0, v0), (t1, v1)) = (pair[0], pair[1]);
        if t <= t1 {
            let u = if t1 > t0 { (t - t0) / (t1 - t0) } else { 1.0 };
            let u = ease.map_or(u, |curve| launcher_scene::bezier(u, curve));
            return v0 + (v1 - v0) * u;
        }
    }
    keys.last().map_or(0.0, |(_, v)| *v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::ChainId;

    fn floor() -> LauncherSceneContext {
        LauncherSceneContext {
            selected: super::super::CubeCatalogKey {
                chain: ChainId::Bitcoin,
                cube_id: "everyday".into(),
            },
            page: 0,
            selected_slot: 0,
            backup: "Status unknown",
            connect: String::new(),
            wallets: "Not yet known".into(),
            peers: Vec::new(),
            pages: 1,
            chrome: Default::default(),
        }
    }

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    #[test]
    fn the_cube_tumbles_then_its_content_fades_in_and_its_edge_glows_in_slowly() {
        let t0 = Instant::now();
        let opening = Opening::new(Some(floor()), false, Stage::Connecting, t0);
        assert_eq!(opening.tumble(t0), 0.0);
        assert_eq!(opening.content(t0 + ms(500)), 0.0);
        assert!(opening.content(t0 + ms(900)) > 0.0 && opening.content(t0 + ms(900)) < 1.0);
        assert!((opening.tumble(t0 + TUMBLE) - 1.0).abs() < 1e-6);
        assert!((opening.content(t0 + ms(1200)) - 1.0).abs() < 1e-6);
        // `::after` waits 700ms, then glows in with no flicker: it only
        // rises over its first 400ms.
        assert_eq!(opening.pulse(t0 + ms(700)), 0.0);
        let first: Vec<f32> = (1..=40)
            .map(|i| opening.pulse(t0 + ms(700 + i * 10)))
            .collect();
        assert!(
            first.windows(2).all(|pair| pair[1] >= pair[0]) && first[0] < 0.01 && first[39] < 0.25,
            "{:?}",
            first
        );
        // Then it breathes softly at the first stage: never dark, never full.
        let breaths: Vec<f32> = (0..60)
            .map(|i| opening.pulse(t0 + ms(2400 + i * 50)))
            .collect();
        assert!(breaths
            .iter()
            .all(|p| (0.2 - 1e-4..=0.6 + 1e-4).contains(p)));
        assert!(breaths.iter().any(|p| *p > 0.59), "{:?}", breaths);
        // `::before` eases from 0 to the first stage's level.
        assert!((opening.level(t0 + ms(2000)) - 0.35).abs() < 1e-6);
    }

    #[test]
    fn each_stage_brightens_the_edge_and_a_held_stage_offers_cancel() {
        let t0 = Instant::now();
        let mut opening = Opening::new(Some(floor()), false, Stage::Connecting, t0);
        let mut levels = Vec::new();
        for (i, stage) in [
            Stage::StartingDaemon,
            Stage::Syncing,
            Stage::AlmostThere,
            Stage::Ready,
        ]
        .iter()
        .copied()
        .enumerate()
        {
            let at = t0 + ms(2000 * (i as u64 + 1));
            opening.set_stage(stage, at);
            levels.push(opening.level(at + LEVEL_FADE));
        }
        assert_eq!(levels, vec![0.5, 0.65, 0.8, 1.0]);
        assert_eq!(opening.pulse(t0 + ms(9000)), 1.0, "Ready is .peak");

        let mut held = Opening::new(Some(floor()), false, Stage::Connecting, t0);
        held.set_stage(Stage::Syncing, t0 + ms(1000));
        assert!(!held.stalled(t0 + ms(1000) + STALL - ms(1)));
        assert!(held.stalled(t0 + ms(1000) + STALL));
        held.set_stage(Stage::AlmostThere, t0 + ms(20_000));
        assert!(
            !held.stalled(t0 + ms(20_000)),
            "a new stage restarts the wait"
        );
    }

    #[test]
    fn ready_holds_a_beat_then_the_face_rushes_and_the_wash_hands_over_to_the_app() {
        let t0 = Instant::now();
        let mut opening = Opening::new(Some(floor()), false, Stage::Syncing, t0);
        let ready = t0 + ms(3000);
        opening.set_stage(Stage::Ready, ready);
        assert!(!opening.shows_app(ready));
        assert_eq!(opening.rush(ready + READY_BEAT), 0.0);
        assert_eq!(opening.wash(ready + READY_BEAT), 0.0);
        let finale = ready + READY_BEAT;
        // Ease-in: barely moving at first, then fast.
        assert!(opening.rush(finale + ms(300)) < 0.1);
        assert!((opening.rush(finale + RUSH) - 1.0).abs() < 1e-6);
        assert!((opening.wash(finale + ms(714)) - 0.92).abs() < 1e-3);
        assert!(!opening.shows_app(finale + APP_AT - ms(1)));
        assert!(opening.shows_app(finale + APP_AT));
        assert!(!opening.done(finale + WASH - ms(1)));
        assert!(opening.done(finale + WASH));
        assert_eq!(opening.wash(finale + WASH), 0.0);
        assert!(
            opening.pose(finale + RUSH).rush > 0.99 && opening.pose(finale).open > 0.99,
            "the finale runs on the loading face"
        );
    }

    #[test]
    fn without_the_floor_the_content_opens_full_screen_with_no_tumble_or_finale() {
        let t0 = Instant::now();
        let mut opening = Opening::new(None, false, Stage::Connecting, t0);
        assert_eq!(opening.tumble(t0), 1.0);
        assert!((opening.content(t0 + FRAME_FADE) - 1.0).abs() < 1e-6);
        assert_eq!(opening.pulse(t0 + ms(300)), 0.0);
        assert!(opening.pulse(t0 + ms(1000)) > 0.0);
        opening.set_stage(Stage::Ready, t0 + ms(4000));
        assert!(opening.shows_app(t0 + ms(4000)) && opening.done(t0 + ms(4000)));
        assert_eq!(opening.wash(t0 + ms(5000)), 0.0);
        assert!(!opening.needs_frames(t0));

        let mut reduced = Opening::new(None, true, Stage::Connecting, t0);
        assert_eq!(reduced.content(t0), 1.0);
        assert_eq!(reduced.level(t0), 0.35, "no fade under reduced motion");
        assert_eq!(reduced.pulse(t0 + ms(800)), 0.0, "no flicker or breath");
        assert!(!reduced.breathes());
        reduced.set_stage(Stage::Syncing, t0 + ms(10));
        assert_eq!(reduced.level(t0 + ms(10)), 0.65);
    }

    #[test]
    fn a_held_error_restarts_the_stall_wait() {
        let t0 = Instant::now();
        let mut opening = Opening::new(Some(floor()), false, Stage::Connecting, t0);
        // An error page shows from 3s to 40s; Retry comes back to Connecting.
        opening.hold(t0 + ms(40_000));
        opening.set_stage(Stage::Connecting, t0 + ms(40_000));
        assert!(!opening.stalled(t0 + ms(40_000)));
        assert!(opening.stalled(t0 + ms(40_000) + STALL));
        // A last hold long before the Retry (no ticks in the background)
        // still restarts the wait at the Retry itself.
        opening.hold(t0 + ms(60_000));
        opening.set_stage(Stage::Connecting, t0 + ms(200_000));
        assert!(!opening.stalled(t0 + ms(200_000)));
        assert!(opening.stalled(t0 + ms(200_000) + STALL));
        // Without a hold, the same stage keeps its wait.
        opening.set_stage(Stage::Connecting, t0 + ms(210_000));
        assert!(opening.stalled(t0 + ms(200_000) + STALL));
    }

    #[test]
    fn waiting_on_spark_offers_to_go_on_without_it_after_its_own_wait() {
        let t0 = Instant::now();
        let mut opening = Opening::new(Some(floor()), false, Stage::AlmostThere, t0);
        let spark = t0 + ms(2000);
        opening.set_stage(Stage::Spark, spark);
        // Spark gets its own, longer wait than the other stages' Cancel.
        assert!(!opening.stalled(spark + STALL));
        assert!(!opening.stalled(spark + SPARK_WAIT - ms(1)));
        assert!(opening.stalled(spark + SPARK_WAIT));
        assert!(
            !opening.shows_app(spark + ms(60_000)),
            "the face waits for Spark"
        );
        assert!(
            opening.level(spark + LEVEL_FADE) > Stage::AlmostThere.level(),
            "Spark is the brightest stage before Ready"
        );
        let ready = spark + SPARK_WAIT + ms(1000);
        opening.set_stage(Stage::Ready, ready);
        assert!(!opening.stalled(ready));
    }

    #[test]
    fn a_retry_after_ready_starts_the_stages_over() {
        let t0 = Instant::now();
        let mut opening = Opening::new(Some(floor()), false, Stage::AlmostThere, t0);
        opening.set_stage(Stage::Ready, t0 + ms(100));
        opening.set_stage(Stage::Connecting, t0 + ms(200));
        assert!(!opening.shows_app(t0 + ms(10_000)));
        assert_eq!(opening.wash(t0 + ms(1500)), 0.0);
    }

    /// The time between the first two rises of the breath through its middle
    /// after `from`.
    fn breath_period(opening: &Opening, from: Instant) -> Duration {
        let samples: Vec<(Instant, f32)> = (0..800)
            .map(|i| {
                let at = from + ms(i * 10);
                (at, opening.pulse(at))
            })
            .collect();
        let (low, high) = samples
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), (_, p)| {
                (lo.min(*p), hi.max(*p))
            });
        let middle = (low + high) / 2.0;
        let rises: Vec<Instant> = samples
            .windows(2)
            .filter(|w| w[0].1 < middle && w[1].1 >= middle)
            .map(|w| w[1].0)
            .collect();
        rises[1] - rises[0]
    }

    #[test]
    fn the_breath_quickens_and_deepens_as_the_load_goes_on() {
        let t0 = Instant::now();
        let mut opening = Opening::new(Some(floor()), false, Stage::Connecting, t0);
        let slow = breath_period(&opening, t0 + ms(3000));
        let soft = (0..300)
            .map(|i| opening.pulse(t0 + ms(3000 + i * 10)))
            .fold(0.0_f32, f32::max);

        let at = t0 + ms(12_000);
        let before = opening.pulse(at);
        opening.set_stage(Stage::AlmostThere, at);
        assert!(
            (opening.pulse(at) - before).abs() < 1e-3,
            "the breath carries on from where it was"
        );
        let settled = at + LEVEL_FADE;
        let fast = breath_period(&opening, settled);
        let bright = (0..200)
            .map(|i| opening.pulse(settled + ms(i * 10)))
            .fold(0.0_f32, f32::max);

        assert!(slow > ms(2500) && slow < ms(2700), "{:?}", slow);
        // Three of the four steps toward the fastest breath, Spark's.
        assert!(fast > ms(1400) && fast < ms(1550), "{:?}", fast);
        assert!(bright > soft + 0.25, "{} vs {}", bright, soft);

        opening.set_stage(Stage::Spark, t0 + ms(30_000));
        let fastest = breath_period(&opening, t0 + ms(30_000) + LEVEL_FADE);
        assert!(fastest > ms(1000) && fastest < ms(1200), "{:?}", fastest);
    }
}
