//! Mouse action map, SPEC-dock 5.2.1: what a gesture on a part of the bar does. Pure data and one pure function; the UI
//! thread runs the result (`ui::run_action`). No per-app rules before the Input page: `resolve` is the default map only.

use crate::popup::PopupKind;

/// The part of the bar a gesture lands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    App,
    Empty,
    Start,
    Volume,
    Network,
    Battery,
    Notifications,
    Clock,
    Power,
    Metrics,
    ShowDesktop,
    Tray,
    TrayOverflow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gesture {
    Click,
    Middle,
    Right,
    ShiftClick,
    ShiftMiddle,
    WheelUp,
    WheelDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettingsLink {
    AppsVolume,
    Network,
    BatterySaver,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrayButton {
    Left,
    Middle,
    Right,
}

/// SPEC-dock 5.3.6's Action, cut to what 1.6 does. Copy, so no variant can carry a path or a command (5.3.6 rule).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    None,
    Smart,
    NewInstance,
    Cycle { skip_min: bool, forward: bool },
    CloseGroup,
    Menu,
    Popup(PopupKind),
    DockMenu,
    ShowDesktop,
    Volume(i8),
    Mute,
    OpenTasks,
    OpenSettings(SettingsLink),
    Tray(TrayButton),
}

/// The 1.6 defaults of SPEC-dock 5.2.1 (no per-app rules before the Input page). Shift changes only the App target: on
/// every other target ShiftClick is Click and ShiftMiddle is Middle, as it is today.
pub(crate) fn resolve(t: Target, g: Gesture) -> Action {
    use Action as A;
    use Gesture as G;
    use Target as T;
    let g = match (t, g) {
        (T::App, _) => g,
        (_, G::ShiftClick) => G::Click,
        (_, G::ShiftMiddle) => G::Middle,
        _ => g,
    };
    match (t, g) {
        (T::App, G::Click) => A::Smart,
        (T::App, G::Middle | G::ShiftClick) => A::NewInstance,
        (T::App, G::Right) => A::Menu,
        (T::App, G::ShiftMiddle) => A::CloseGroup,
        (T::App, G::WheelUp) => A::Cycle { skip_min: true, forward: false },
        (T::App, G::WheelDown) => A::Cycle { skip_min: true, forward: true },
        (T::Empty, G::Right) => A::DockMenu,
        (T::Empty, G::WheelUp) => A::Volume(2),
        (T::Empty, G::WheelDown) => A::Volume(-2),
        (T::Volume, G::Click) => A::Popup(PopupKind::Audio),
        (T::Volume, G::Middle) => A::Mute,
        (T::Volume, G::Right) => A::OpenSettings(SettingsLink::AppsVolume),
        (T::Volume, G::WheelUp) => A::Volume(2),
        (T::Volume, G::WheelDown) => A::Volume(-2),
        (T::Network, G::Click) => A::Popup(PopupKind::Network),
        (T::Network, G::Right) => A::OpenSettings(SettingsLink::Network),
        (T::Battery, G::Click) => A::Popup(PopupKind::Battery),
        (T::Battery, G::Right) => A::OpenSettings(SettingsLink::BatterySaver),
        (T::Notifications, G::Click) => A::Popup(PopupKind::Notifications),
        (T::Clock, G::Click) => A::Popup(PopupKind::Calendar),
        (T::Power, G::Click) => A::Popup(PopupKind::Power),
        (T::Metrics, G::Click) => A::OpenTasks,
        (T::Metrics, G::Right) => A::Popup(PopupKind::MetricsMenu),
        (T::ShowDesktop, G::Click) => A::ShowDesktop,
        (T::Start, G::Click) => A::Popup(PopupKind::StartMenu),
        (T::Start, G::Right) => A::Popup(PopupKind::Start),
        (T::Tray, G::Click) => A::Tray(TrayButton::Left),
        (T::Tray, G::Middle) => A::Tray(TrayButton::Middle),
        (T::Tray, G::Right) => A::Tray(TrayButton::Right),
        (T::TrayOverflow, G::Click) => A::Popup(PopupKind::TrayOverflow),
        _ => A::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Action as A;
    use Gesture as G;
    use Target as T;

    const TARGETS: [Target; 13] = [
        T::App,
        T::Empty,
        T::Start,
        T::Volume,
        T::Network,
        T::Battery,
        T::Notifications,
        T::Clock,
        T::Power,
        T::Metrics,
        T::ShowDesktop,
        T::Tray,
        T::TrayOverflow,
    ];
    /// The columns of the literal table below.
    const GESTURES: [Gesture; 7] =
        [G::Click, G::Middle, G::Right, G::ShiftClick, G::ShiftMiddle, G::WheelUp, G::WheelDown];

    fn pop(k: PopupKind) -> Action {
        A::Popup(k)
    }

    /// One row per target, one column per gesture (order of `GESTURES`), written out by hand.
    fn table() -> [[Action; 7]; 13] {
        let cyc_up = A::Cycle { skip_min: true, forward: false };
        let cyc_dn = A::Cycle { skip_min: true, forward: true };
        let n = A::None;
        [
            // App
            [A::Smart, A::NewInstance, A::Menu, A::NewInstance, A::CloseGroup, cyc_up, cyc_dn],
            // Empty
            [n, n, A::DockMenu, n, n, A::Volume(2), A::Volume(-2)],
            // Start
            [pop(PopupKind::StartMenu), n, pop(PopupKind::Start), pop(PopupKind::StartMenu), n, n, n],
            // Volume
            [
                pop(PopupKind::Audio),
                A::Mute,
                A::OpenSettings(SettingsLink::AppsVolume),
                pop(PopupKind::Audio),
                A::Mute,
                A::Volume(2),
                A::Volume(-2),
            ],
            // Network
            [pop(PopupKind::Network), n, A::OpenSettings(SettingsLink::Network), pop(PopupKind::Network), n, n, n],
            // Battery
            [pop(PopupKind::Battery), n, A::OpenSettings(SettingsLink::BatterySaver), pop(PopupKind::Battery), n, n, n],
            // Notifications
            [pop(PopupKind::Notifications), n, n, pop(PopupKind::Notifications), n, n, n],
            // Clock
            [pop(PopupKind::Calendar), n, n, pop(PopupKind::Calendar), n, n, n],
            // Power
            [pop(PopupKind::Power), n, n, pop(PopupKind::Power), n, n, n],
            // Metrics
            [A::OpenTasks, n, pop(PopupKind::MetricsMenu), A::OpenTasks, n, n, n],
            // ShowDesktop
            [A::ShowDesktop, n, n, A::ShowDesktop, n, n, n],
            // Tray
            [
                A::Tray(TrayButton::Left),
                A::Tray(TrayButton::Middle),
                A::Tray(TrayButton::Right),
                A::Tray(TrayButton::Left),
                A::Tray(TrayButton::Middle),
                n,
                n,
            ],
            // TrayOverflow
            [pop(PopupKind::TrayOverflow), n, n, pop(PopupKind::TrayOverflow), n, n, n],
        ]
    }

    #[test]
    fn action_table_covers_every_cell() {
        let t = table();
        for (r, target) in TARGETS.iter().enumerate() {
            for (c, gesture) in GESTURES.iter().enumerate() {
                assert_eq!(resolve(*target, *gesture), t[r][c], "{target:?} x {gesture:?}");
            }
        }
    }

    /// What `click_left`, `on_mbutton_up` and `on_rbutton_up` did before the table (18f2762), cell by cell.
    #[test]
    fn today_clicks_are_kept() {
        let click = |t| match t {
            T::App => A::Smart,
            T::Notifications => pop(PopupKind::Notifications),
            T::Volume => pop(PopupKind::Audio),
            T::Clock => pop(PopupKind::Calendar),
            T::Power => pop(PopupKind::Power),
            T::TrayOverflow => pop(PopupKind::TrayOverflow),
            T::Tray => A::Tray(TrayButton::Left),
            T::ShowDesktop => A::ShowDesktop,
            T::Start => pop(PopupKind::StartMenu),
            T::Network => pop(PopupKind::Network),
            T::Metrics => A::OpenTasks,
            T::Battery => pop(PopupKind::Battery),
            T::Empty => A::None,
        };
        let middle = |t| match t {
            T::App => A::NewInstance,
            T::Volume => A::Mute,
            T::Tray => A::Tray(TrayButton::Middle),
            _ => A::None,
        };
        let right = |t| match t {
            T::App => A::Menu,
            T::Volume => A::OpenSettings(SettingsLink::AppsVolume),
            T::Network => A::OpenSettings(SettingsLink::Network),
            T::Battery => A::OpenSettings(SettingsLink::BatterySaver),
            T::Start => pop(PopupKind::Start),
            T::Metrics => pop(PopupKind::MetricsMenu),
            T::Tray => A::Tray(TrayButton::Right),
            // Empty space gets the dock menu (G2b); the rest did nothing.
            T::Empty => A::DockMenu,
            _ => A::None,
        };
        for t in TARGETS {
            assert_eq!(resolve(t, G::Click), click(t), "click {t:?}");
            assert_eq!(resolve(t, G::Middle), middle(t), "middle {t:?}");
            assert_eq!(resolve(t, G::Right), right(t), "right {t:?}");
        }
    }

    #[test]
    fn action_carries_no_text() {
        fn is_copy<T: Copy>() {}
        is_copy::<Action>();
    }
}
