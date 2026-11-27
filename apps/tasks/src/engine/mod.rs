//! The sampling engine: needs, cadence, the sampler host and the readers behind it. No window.
pub mod cadence;
pub mod frame;
pub mod host;
pub mod need;
pub mod paths;
pub mod services;
pub mod sys;
pub mod trace;
pub mod windows;

#[cfg(test)]
mod tests {
    use super::cadence::{idle, period_ms, Control, Refresh, Slow};
    use super::frame::{sys_state, Frame, Sampler, ServiceEntry, ServiceMap, SysSample, WindowFact};
    use super::host::{default_samplers, FrameSink, HostStats, SamplerHost};
    use super::need::{need_set, Need, NeedSet, Strip, ViewNeeds};
    use super::paths::{PathSampler, PathSource, Win32Paths};
    use super::services::{ScmServices, ServiceSampler};
    use super::sys::{CoreSampler, ProcSampler};
    use super::trace::{
        flaps, parse_trace, replay, token_key, write_trace, Tick, Trace, TraceError, TraceGroup, TraceHeader,
    };
    use super::windows::{RawWindow, Styles, Win32Windows, WindowSampler};
    use crate::model::classify::PathMap;
    use crate::model::group::AppKey;
    use crate::model::verdict::{SysState, Verdict};
    use crate::model::view::{ColumnId, Page, Section, ViewMode};
    use oled_sysmon::percore::CoreTimes;
    use oled_sysmon::{ProcKey, SysmonError};
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    type Step = (u64, Verdict, u8);
    type StartFn = fn(Box<dyn FrameSink>, Vec<Box<dyn Sampler>>, Control) -> Result<SamplerHost, SysmonError>;

    #[test]
    fn frozen_engine_types_compile() {
        let key = ProcKey { pid: 4, created: 9 };
        let need = NeedSet::EMPTY.with(Need::Procs);
        let columns = [ColumnId::Name];
        let strips = [Strip::Cpu];
        let _ = ViewNeeds {
            page: Page::default(),
            mode: ViewMode::default(),
            columns: &columns,
            strips: &strips,
            expanded: &[],
            windows_rows_visible: true,
            verdict_on: false,
        };
        let _ = Control {
            visible: true,
            foreground: false,
            minimised_or_cloaked: false,
            display_off: false,
            refresh: Refresh::default(),
            idle: false,
            need,
        };
        let _ = (Slow::default(), HostStats::default());
        let mk = |pid: u32, name: &str| ServiceEntry { pid, name: name.into(), display: "d".into() };
        let map = ServiceMap { entries: vec![mk(1, "a"), mk(4, "b"), mk(4, "c"), mk(7, "d")] };
        assert_eq!(map.for_pid(4), &[mk(4, "b"), mk(4, "c")]);
        assert_eq!(map.for_pid(1), &[mk(1, "a")]);
        assert_eq!(map.for_pid(7), &[mk(7, "d")]);
        assert!(map.for_pid(0).is_empty());
        assert!(map.for_pid(5).is_empty());
        assert!(map.for_pid(8).is_empty());
        let f = Frame {
            seq: 1,
            t_us: 2,
            dt_ms: 3,
            need,
            procs: Default::default(),
            sys: SysSample::default(),
            added: vec![key],
            gone: vec![key],
            paths: Arc::new(PathMap::default()),
            paths_fresh: false,
            paths_us: 0,
            windows: vec![WindowFact { owner: key, hwnd: 1, hung: false }],
            windows_fresh: false,
            services: Arc::new(ServiceMap::default()),
            services_fresh: false,
            services_error: None,
            error: None,
            tick_us: 0,
        };
        assert_eq!(f.windows.len(), 1);
        let _ = (
            RawWindow { hwnd: 1, pid: 4, styles: Styles::default(), excluded_class: false, core_pid: None },
            TraceHeader { version: 1, tick_ms: 1000, logical_cpus: 4, total_phys: 1, build: 1, note: String::new() },
            TraceError::Header,
        );
        let group = TraceGroup {
            token: "t".into(),
            section: Section::Apps,
            cpu: 1.0,
            mem: 2,
            io_bps: 3.0,
            hung: false,
            has_windows: true,
        };
        let state = SysState {
            cpu_pct: 1.0,
            mem_load_pct: 2,
            commit_pct: 3.0,
            dpc_irq_pct: 0.0,
            logical_cpus: 4,
            paused: false,
            read_error: None,
        };
        let tick = Tick { t_ms: 0, sys: state, groups: vec![group] };
        let header =
            TraceHeader { version: 1, tick_ms: 1000, logical_cpus: 4, total_phys: 1, build: 1, note: String::new() };
        let trace = Trace { header, ticks: vec![tick] };
        assert_eq!(trace.ticks.len(), 1);
        let _: fn(&Frame, &[CoreTimes], bool) -> SysState = sys_state;
        let _: fn(&ViewNeeds) -> NeedSet = need_set;
        let _: fn(&Control, bool) -> Option<u32> = period_ms;
        let _: fn(u64, u64, bool) -> bool = idle;
        let _: fn(Refresh) -> Option<u32> = Refresh::ms;
        let _: fn(&mut Slow, u32) -> bool = Slow::observe;
        let _: fn(&mut Win32Paths, ProcKey, &mut Vec<u16>) -> Result<(), u32> = <Win32Paths as PathSource>::query;
        let _: fn(&WindowSampler<Win32Windows>) -> Arc<AtomicBool> = WindowSampler::<Win32Windows>::dirty_flag;
        let _: fn(&str) -> Result<Trace, TraceError> = parse_trace;
        let _: fn(&Trace, &mut String) = write_trace;
        let _: fn(&Trace) -> Vec<Step> = replay;
        let _: fn(&[Step], u64) -> u32 = flaps;
        let _: fn(&Trace, &str) -> Option<AppKey> = token_key;
        let _: fn() -> Vec<Box<dyn Sampler>> = default_samplers;
        let _: StartFn = SamplerHost::start;
        let _: fn(&SamplerHost, &mut Frame) -> bool = SamplerHost::take;
        let _: fn(SamplerHost) = SamplerHost::stop;
        let _: Option<SysmonError> = None;
        let _f1: fn(&mut ProcSampler, u64, &mut Frame) -> Result<(), SysmonError> = <ProcSampler as Sampler>::sample;
        let _f2: fn(&mut CoreSampler, u64, &mut Frame) -> Result<(), SysmonError> = <CoreSampler as Sampler>::sample;
        let _f3: fn(&mut PathSampler<Win32Paths>, u64, &mut Frame) -> Result<(), SysmonError> =
            <PathSampler<Win32Paths> as Sampler>::sample;
        let _f4: fn(&mut WindowSampler<Win32Windows>, u64, &mut Frame) -> Result<(), SysmonError> =
            <WindowSampler<Win32Windows> as Sampler>::sample;
        let _f5: fn(&mut ServiceSampler<ScmServices>, u64, &mut Frame) -> Result<(), SysmonError> =
            <ServiceSampler<ScmServices> as Sampler>::sample;
    }
}
