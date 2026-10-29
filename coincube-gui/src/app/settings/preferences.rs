//! Installation-wide display choices. Platform inputs are supplied by the caller;
//! this module does not observe the operating system or change the running GUI.

use coincube_ui::theme::palette::ThemeMode;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreference {
    System,
    Light,
    Dark,
}

impl ThemePreference {
    /// `None` means the platform supplied no usable mode. Keep the current
    /// concrete dark fallback until the GUI has a real system-theme input.
    pub fn resolve(self, system_mode: Option<ThemeMode>) -> ThemeMode {
        match self {
            Self::System => system_mode.unwrap_or(ThemeMode::Dark),
            Self::Light => ThemeMode::Light,
            Self::Dark => ThemeMode::Dark,
        }
    }
}

impl From<ThemeMode> for ThemePreference {
    fn from(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Light => Self::Light,
            ThemeMode::Dark => Self::Dark,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LauncherStyle {
    #[default]
    Cubes,
    List,
}

/// Where the Launcher reads the network activity it shows behind the Cubes.
/// It only ever reads a node that is already running; it never starts one.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkActivitySource {
    /// The managed node when it is running, otherwise COINCUBE | Connect.
    #[default]
    Automatic,
    LocalNode,
    Connect,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotionPreference {
    #[default]
    FollowSystem,
    Reduced,
}

impl MotionPreference {
    /// An unknown system setting does not request reduced motion. A caller
    /// must supply an observed input before FollowSystem can follow the OS.
    pub fn is_reduced(self, system_reduces_motion: Option<bool>) -> bool {
        matches!(self, Self::Reduced)
            || matches!(self, Self::FollowSystem) && system_reduces_motion == Some(true)
    }
}

/// Whole-interface zoom, applied as Iced's scale factor. Saved as a percent;
/// any other saved number snaps to the nearest step, and a saved value that
/// is not a number reads as 100% rather than failing the whole settings file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(into = "u16")]
pub struct Zoom(u16);

impl<'de> Deserialize<'de> for Zoom {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Saved {
            Percent(f64),
            Other(serde::de::IgnoredAny),
        }
        Ok(match Saved::deserialize(deserializer)? {
            // `as` saturates: negatives read as 0 and huge values as u16::MAX.
            Saved::Percent(percent) => Zoom::from(percent.round() as u16),
            Saved::Other(_) => Zoom::default(),
        })
    }
}

impl Zoom {
    /// The steps ⌘+ and ⌘− move between. At 150% a 1440px window lays out at
    /// 960 points; the minimum window grows with the zoom, so no page is ever
    /// laid out narrower than [`Zoom::LAYOUT_MIN_WIDTH`].
    pub const STEPS: [Zoom; 6] = [
        Zoom(80),
        Zoom(90),
        Zoom(100),
        Zoom(110),
        Zoom(125),
        Zoom(150),
    ];
    pub const ACTUAL_SIZE: Zoom = Zoom(100);
    pub const LARGEST: Zoom = Zoom(150);
    /// The narrowest layout pages are designed for, in points at the zoom.
    pub const LAYOUT_MIN_WIDTH: f32 = 1024.0;

    /// The largest step a screen `screen_width` points wide can hold at
    /// [`Zoom::LAYOUT_MIN_WIDTH`]: 125% on a 1440-point laptop, 150% from
    /// 1536 points. Never below 100%, which works on any screen it did before.
    pub fn largest_for_screen(screen_width: f32) -> Zoom {
        Self::STEPS
            .iter()
            .copied()
            .rev()
            .find(|step| Self::LAYOUT_MIN_WIDTH * step.factor() <= screen_width)
            .unwrap_or(Self::ACTUAL_SIZE)
            .max(Self::ACTUAL_SIZE)
    }

    pub fn percent(self) -> u16 {
        self.0
    }

    pub fn factor(self) -> f32 {
        f32::from(self.0) / 100.0
    }

    /// The next step up, or `None` at the largest.
    pub fn zoomed_in(self) -> Option<Zoom> {
        Self::STEPS.iter().copied().find(|step| step.0 > self.0)
    }

    /// The next step down, or `None` at the smallest.
    pub fn zoomed_out(self) -> Option<Zoom> {
        Self::STEPS
            .iter()
            .copied()
            .rev()
            .find(|step| step.0 < self.0)
    }
}

impl Default for Zoom {
    fn default() -> Self {
        Self::ACTUAL_SIZE
    }
}

impl From<u16> for Zoom {
    fn from(percent: u16) -> Self {
        Self::STEPS
            .iter()
            .copied()
            .min_by_key(|step| step.0.abs_diff(percent))
            .unwrap_or_default()
    }
}

impl From<Zoom> for u16 {
    fn from(zoom: Zoom) -> Self {
        zoom.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevicePreferences {
    pub theme: ThemePreference,
    pub launcher_style: LauncherStyle,
    pub hide_balances: bool,
    pub motion: MotionPreference,
    pub zoom: Zoom,
    pub developer_mode: bool,
    pub bitcoin_blake2b_beta: bool,
    /// Recent blocks, mempool and chain facts behind the Launcher's Cubes.
    pub network_activity: bool,
    pub network_activity_source: NetworkActivitySource,
}

impl Default for DevicePreferences {
    fn default() -> Self {
        Self {
            theme: ThemePreference::System,
            launcher_style: LauncherStyle::Cubes,
            hide_balances: false,
            motion: MotionPreference::FollowSystem,
            zoom: Zoom::ACTUAL_SIZE,
            developer_mode: false,
            bitcoin_blake2b_beta: false,
            network_activity: true,
            network_activity_source: NetworkActivitySource::Automatic,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectivePreferences {
    pub theme_mode: ThemeMode,
    pub launcher_style: LauncherStyle,
    pub hide_balances: bool,
    pub reduce_motion: bool,
}

impl DevicePreferences {
    /// Pure resolution of saved choices against explicitly injected platform
    /// inputs. Effective List never overwrites a saved Cubes choice.
    pub fn resolve(
        self,
        system_theme: Option<ThemeMode>,
        system_reduces_motion: Option<bool>,
    ) -> EffectivePreferences {
        let reduce_motion = self.motion.is_reduced(system_reduces_motion);
        EffectivePreferences {
            theme_mode: self.theme.resolve(system_theme),
            launcher_style: if reduce_motion {
                LauncherStyle::List
            } else {
                self.launcher_style
            },
            hide_balances: self.hide_balances,
            reduce_motion,
        }
    }
}

/// A single-field edit, applied to the latest locked global-settings value.
/// Callers never persist a stale complete snapshot over newer settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreferenceChange {
    Theme(ThemePreference),
    LauncherStyle(LauncherStyle),
    HideBalances(bool),
    Motion(MotionPreference),
    Zoom(Zoom),
    DeveloperMode(bool),
    BitcoinBlake2bBeta(bool),
    NetworkActivity(bool),
    NetworkActivitySource(NetworkActivitySource),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_steps_stop_at_both_ends() {
        let mut zoom = Zoom::ACTUAL_SIZE;
        let mut seen = vec![zoom.percent()];
        while let Some(next) = zoom.zoomed_in() {
            zoom = next;
            seen.push(zoom.percent());
        }
        assert_eq!(seen, [100, 110, 125, 150]);
        while let Some(next) = zoom.zoomed_out() {
            zoom = next;
        }
        assert_eq!(zoom.percent(), 80);
    }

    #[test]
    fn a_saved_zoom_off_the_steps_snaps_to_the_nearest() {
        assert_eq!(Zoom::from(118).percent(), 125);
        assert_eq!(Zoom::from(0).percent(), 80);
        assert_eq!(Zoom::from(400).percent(), 150);
        assert_eq!(serde_json::from_str::<Zoom>("112").unwrap().percent(), 110);
        assert_eq!(serde_json::to_string(&Zoom::from(125)).unwrap(), "125");
    }

    #[test]
    fn a_saved_zoom_that_is_not_a_step_never_fails_the_settings_file() {
        let read = |json: &str| serde_json::from_str::<Zoom>(json).unwrap().percent();
        assert_eq!(read("112.5"), 110);
        assert_eq!(read("1.25"), 80);
        assert_eq!(read("-20"), 80);
        assert_eq!(read("70000"), 150);
        assert_eq!(read("\"125\""), 100);
        assert_eq!(read("null"), 100);
    }

    #[test]
    fn a_screen_offers_only_the_zoom_it_can_lay_out_at_full_width() {
        let largest = |width: f32| Zoom::largest_for_screen(width).percent();
        // A 1440 x 900 laptop: 150% would leave 960 points, under 1024.
        assert_eq!(largest(1440.0), 125);
        assert_eq!(largest(1280.0), 125);
        assert_eq!(largest(1535.0), 125);
        assert_eq!(largest(1536.0), 150);
        assert_eq!(largest(2560.0), 150);
        assert_eq!(largest(1200.0), 110);
        // A screen too small for 100% keeps 100%, as before zoom existed.
        assert_eq!(largest(800.0), 100);
    }

    #[test]
    fn theme_resolves_system_and_explicit_overrides() {
        assert_eq!(
            ThemePreference::System.resolve(Some(ThemeMode::Light)),
            ThemeMode::Light
        );
        assert_eq!(
            ThemePreference::System.resolve(Some(ThemeMode::Dark)),
            ThemeMode::Dark
        );
        assert_eq!(ThemePreference::System.resolve(None), ThemeMode::Dark);
        assert_eq!(
            ThemePreference::Light.resolve(Some(ThemeMode::Dark)),
            ThemeMode::Light
        );
        assert_eq!(
            ThemePreference::Dark.resolve(Some(ThemeMode::Light)),
            ThemeMode::Dark
        );
    }

    #[test]
    fn motion_forces_effective_list_without_changing_saved_style() {
        let saved = DevicePreferences::default();
        assert_eq!(saved.launcher_style, LauncherStyle::Cubes);
        for input in [None, Some(false)] {
            let effective = saved.resolve(None, input);
            assert!(!effective.reduce_motion);
            assert_eq!(effective.launcher_style, LauncherStyle::Cubes);
        }
        let reduced = saved.resolve(None, Some(true));
        assert!(reduced.reduce_motion);
        assert_eq!(reduced.launcher_style, LauncherStyle::List);
        assert_eq!(
            saved.resolve(None, Some(false)).launcher_style,
            LauncherStyle::Cubes
        );

        let explicit = DevicePreferences {
            motion: MotionPreference::Reduced,
            ..saved
        };
        assert!(explicit.resolve(None, None).reduce_motion);
        assert_eq!(
            explicit.resolve(None, Some(false)).launcher_style,
            LauncherStyle::List
        );
    }
}
