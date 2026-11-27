//! View vocabulary shared by the model and the window.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Page {
    #[default]
    Processes,
    Performance,
    Startup,
    Services,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ViewMode {
    #[default]
    Grouped,
    Tree,
    List,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Section {
    Apps,
    Background,
    Windows,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColumnId {
    Name,
    Cpu,
    Memory,
    Io,
    PrivateBytes,
    WorkingSet,
    PeakWorkingSet,
    ReadPerSec,
    WritePerSec,
    OtherPerSec,
    Threads,
    Handles,
    Pid,
    ParentPid,
    Session,
    User,
    Priority,
    Efficiency,
    Started,
    Path,
    CommandLine,
    Status,
    Gpu,
    GpuEngine,
    GpuMemory,
    NetSent,
    NetReceived,
}
