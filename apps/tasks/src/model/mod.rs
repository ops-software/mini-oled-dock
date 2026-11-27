pub mod classify;
pub mod group;
pub mod order;
pub mod rates;
pub mod tree;
pub mod verdict;
pub mod view;

#[cfg(test)]
mod tests {
    use super::classify::{ImagePath, PathClass, PathMap};
    use super::group::{AppKey, AppWindow, ExeId, Group, IdentityId, ImageId, ProcInfo, Totals};
    use super::order::{HoldInputs, HoldState, OrderState, SortSpec};
    use super::rates::Rates;
    use super::verdict::{GroupStat, Rule, SysState, Verdict};
    use super::view::{ColumnId, Page, Section, ViewMode};
    use oled_sysmon::ProcKey;

    #[test]
    fn frozen_types_compile() {
        let key = ProcKey { pid: 4, created: 9 };
        let app = AppKey::Image(ImageId(1));
        let _ = [AppKey::App(IdentityId(1)), AppKey::Exe(ExeId(2)), app, AppKey::Single(key)];
        let stat = GroupStat {
            key: app,
            section: Section::Apps,
            cpu: 1.0,
            mem: 2,
            io_bps: 3.0,
            hung: false,
            has_windows: true,
        };
        let v = Verdict::Calm { busiest: Some(app), pct: 3 };
        assert_ne!(v, Verdict::Looking);
        let _ = (stat, SysState::default(), Rule::CpuHog, Rates::default(), Totals::default());
        let _ = (
            SortSpec { floor: 0.5, descending: true },
            HoldInputs::default(),
            HoldState::default(),
            OrderState::default(),
        );
        let path = ImagePath::Path("c:\\x\\a.exe".into());
        let map = PathMap {
            entries: vec![(key, path.clone()), (ProcKey::default(), ImagePath::Unknown), (key, ImagePath::NoImage)],
        };
        assert_eq!(map.entries.len(), 3);
        let info = ProcInfo { key, exe: Some(ExeId(2)), image: ImageId(1), class: PathClass::Other };
        let _ = (info, AppWindow { owner: key, identity: IdentityId(1), hung: false });
        let g = Group { key: app, section: Section::Background, members: vec![key], windows: 0, hung_windows: 0 };
        assert_eq!(g.members.len(), 1);
        let _ = (Page::default(), ViewMode::default(), ColumnId::Name);
    }
}
