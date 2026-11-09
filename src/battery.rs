//! Battery status for the BATTERY module of the bar (`render::ItemId::Battery`) and its popup.
//!
//! COST: one kernel32 call (`GetSystemPowerStatus`) per sample, no allocation, no thread, no timer
//! (the UI arms its own 30 s timer, only while the bar is visible and a battery is present).
//! `powrprof` is touched only when the battery popup opens (`power_mode`).
//!
//! THREADING: UI thread. `BatteryStatus` is plain data.

use windows::core::GUID;
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::System::Power::{GetSystemPowerStatus, PowerGetActiveScheme, SYSTEM_POWER_STATUS};

use crate::config::BatteryShow;

/// One reading of the system battery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatteryStatus {
    /// A system battery exists and Windows knows its charge.
    pub present: bool,
    /// Charge, 0..=100 (0 when not `present`).
    pub percent: u8,
    /// Plugged in.
    pub ac: bool,
    /// The battery is being charged right now.
    pub charging: bool,
    /// Estimated seconds of charge left; only while on battery and when Windows has an estimate.
    pub seconds_left: Option<u32>,
}

impl BatteryStatus {
    /// No battery (a desktop, or an unreadable status).
    pub const NONE: BatteryStatus =
        BatteryStatus { present: false, percent: 0, ac: false, charging: false, seconds_left: None };

    /// Builds a status from the raw `SYSTEM_POWER_STATUS` fields. Windows' "unknown" markers
    /// (flag 255, percent 255, life 0xFFFFFFFF, flag bit 128 = no system battery) never leak out.
    pub fn from_raw(ac_line: u8, flag: u8, percent: u8, life_secs: u32) -> BatteryStatus {
        let ac = ac_line == 1;
        if flag & 128 != 0 || flag == 255 || percent > 100 {
            return BatteryStatus { ac, ..BatteryStatus::NONE };
        }
        BatteryStatus {
            present: true,
            percent,
            ac,
            charging: flag & 8 != 0,
            seconds_left: if !ac && life_secs != u32::MAX { Some(life_secs) } else { None },
        }
    }

    /// What the battery is doing, for the popup title.
    pub fn state(&self) -> ChargeState {
        if self.charging {
            ChargeState::Charging
        } else if self.ac {
            if self.percent >= 100 {
                ChargeState::Full
            } else {
                ChargeState::PluggedIn
            }
        } else {
            ChargeState::OnBattery
        }
    }

    /// Colour class of the fill: plugged in or charging is never alarming; on battery 10 % and
    /// below is `Critical`, below 20 % is `Low`.
    pub fn tint(&self) -> Tint {
        if self.ac || self.charging {
            Tint::Normal
        } else if self.percent <= 10 {
            Tint::Critical
        } else if self.percent < 20 {
            Tint::Low
        } else {
            Tint::Normal
        }
    }
}

impl Default for BatteryStatus {
    fn default() -> Self {
        BatteryStatus::NONE
    }
}

/// Charge state shown by the popup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChargeState {
    OnBattery,
    Charging,
    /// Plugged in at 100 %.
    Full,
    /// Plugged in but not charging below 100 % (battery at its charge limit).
    PluggedIn,
}

/// Colour class of the battery fill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tint {
    Normal,
    Low,
    Critical,
}

/// Reads the system power status. `None` only if the call fails.
pub fn query() -> Option<BatteryStatus> {
    let mut s = SYSTEM_POWER_STATUS::default();
    // SAFETY: `s` is a valid, writable SYSTEM_POWER_STATUS for the duration of the call.
    unsafe { GetSystemPowerStatus(&mut s) }.ok()?;
    Some(BatteryStatus::from_raw(s.ACLineStatus, s.BatteryFlag, s.BatteryLifePercent, s.BatteryLifeTime))
}

/// Whether the 30 s timer should run: bar visible, a battery to read, module not switched off.
pub fn polling_wanted(show: BatteryShow, present: bool, visible: bool) -> bool {
    visible && present && show != BatteryShow::Off
}

/// "3 h 12 min", "1 h", "45 min" (minutes rounded down, at least 1).
pub fn format_remaining(secs: u32) -> String {
    let mins = (secs / 60).max(1);
    let (h, m) = (mins / 60, mins % 60);
    match (h, m) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// The active Windows power plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerMode {
    PowerSaver,
    Balanced,
    HighPerformance,
}

fn mode_from_guid(g: &GUID) -> Option<PowerMode> {
    match g.to_u128() {
        0x381b4222_f694_41f0_9685_ff5bb260df2e => Some(PowerMode::Balanced),
        0x8c5e7fda_e8bf_4a96_9a85_a6e23a8c635c => Some(PowerMode::HighPerformance),
        0xa1841308_3541_4fab_bc81_f71556f20b4a => Some(PowerMode::PowerSaver),
        _ => None,
    }
}

/// The active power plan, `None` when it cannot be read or is a custom plan.
pub fn power_mode() -> Option<PowerMode> {
    let mut p: *mut GUID = std::ptr::null_mut();
    // SAFETY: `p` is a valid out pointer; on success Windows allocates the GUID with LocalAlloc.
    let err = unsafe { PowerGetActiveScheme(None, &mut p) };
    if err.0 != 0 || p.is_null() {
        return None;
    }
    // SAFETY: `p` points to a GUID allocated by the call above; it is copied, then freed once.
    let guid = unsafe { *p };
    // SAFETY: `p` came from LocalAlloc inside PowerGetActiveScheme and is not used afterwards.
    unsafe { LocalFree(Some(HLOCAL(p.cast()))) };
    mode_from_guid(&guid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_raw_unknown_and_absent_values() {
        assert!(!BatteryStatus::from_raw(1, 128, 255, u32::MAX).present); // desktop: "no system battery"
        assert!(!BatteryStatus::from_raw(0, 255, 255, u32::MAX).present); // status unknown
        assert!(!BatteryStatus::from_raw(0, 1, 255, u32::MAX).present); // percent unknown
        assert!(BatteryStatus::from_raw(1, 128, 255, u32::MAX).ac); // AC is still reported
        let b = BatteryStatus::from_raw(0, 1, 64, 11520);
        assert_eq!((b.present, b.percent, b.ac, b.charging, b.seconds_left), (true, 64, false, false, Some(11520)));
        assert_eq!(BatteryStatus::from_raw(0, 1, 64, u32::MAX).seconds_left, None); // time unknown
        assert_eq!(BatteryStatus::from_raw(1, 9, 80, 5000).seconds_left, None); // no estimate while plugged in
    }

    #[test]
    fn state_plugged_in_cases() {
        assert_eq!(BatteryStatus::from_raw(0, 1, 64, 100).state(), ChargeState::OnBattery);
        assert_eq!(BatteryStatus::from_raw(1, 9, 80, u32::MAX).state(), ChargeState::Charging); // flag bit 8
        assert_eq!(BatteryStatus::from_raw(1, 1, 100, u32::MAX).state(), ChargeState::Full);
        assert_eq!(BatteryStatus::from_raw(1, 1, 90, u32::MAX).state(), ChargeState::PluggedIn); // charge limit
    }

    #[test]
    fn tint_thresholds() {
        let on = |p| BatteryStatus::from_raw(0, 1, p, u32::MAX).tint();
        assert_eq!(
            (on(100), on(20), on(19), on(11), on(10), on(1)),
            (Tint::Normal, Tint::Normal, Tint::Low, Tint::Low, Tint::Critical, Tint::Critical)
        );
        assert_eq!(BatteryStatus::from_raw(1, 9, 5, u32::MAX).tint(), Tint::Normal); // charging: never alarming
    }

    #[test]
    fn polling_only_when_there_is_something_to_poll() {
        use crate::config::BatteryShow::*;
        assert!(polling_wanted(Auto, true, true));
        assert!(!polling_wanted(Auto, false, true)); // desktop: no timer
        assert!(!polling_wanted(On, false, true)); // shown without a battery: nothing to refresh
        assert!(!polling_wanted(Off, true, true) && !polling_wanted(Auto, true, false)); // off, or bar hidden
    }

    #[test]
    fn remaining_time_text() {
        assert_eq!(format_remaining(11520), "3 h 12 min");
        assert_eq!(format_remaining(2700), "45 min");
        assert_eq!(format_remaining(3600), "1 h");
        assert_eq!(format_remaining(30), "1 min");
    }

    #[test]
    fn power_plan_guids() {
        let g = |v: u128| GUID::from_u128(v);
        assert_eq!(mode_from_guid(&g(0x381b4222_f694_41f0_9685_ff5bb260df2e)), Some(PowerMode::Balanced));
        assert_eq!(mode_from_guid(&g(0x8c5e7fda_e8bf_4a96_9a85_a6e23a8c635c)), Some(PowerMode::HighPerformance));
        assert_eq!(mode_from_guid(&g(0xa1841308_3541_4fab_bc81_f71556f20b4a)), Some(PowerMode::PowerSaver));
        assert_eq!(mode_from_guid(&g(1)), None);
    }
}
