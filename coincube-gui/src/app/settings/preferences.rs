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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevicePreferences {
    pub theme: ThemePreference,
    pub launcher_style: LauncherStyle,
    pub hide_balances: bool,
    pub motion: MotionPreference,
    pub developer_mode: bool,
    pub bitcoin_blake2b_beta: bool,
}

impl Default for DevicePreferences {
    fn default() -> Self {
        Self {
            theme: ThemePreference::System,
            launcher_style: LauncherStyle::Cubes,
            hide_balances: false,
            motion: MotionPreference::FollowSystem,
            developer_mode: false,
            bitcoin_blake2b_beta: false,
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
    DeveloperMode(bool),
    BitcoinBlake2bBeta(bool),
}

#[cfg(test)]
mod tests {
    use super::*;

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
