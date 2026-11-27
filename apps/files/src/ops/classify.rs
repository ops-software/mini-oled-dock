//! Whether a delete can go to the Recycle Bin (OP2): pure classify_facts over facts read by gather_facts. Filled by
//! F-OPS.

use crate::listing::WidePath;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoBin {
    Network,
    Removable,
    OverQuota,
    Disabled,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recyclability {
    Yes,
    No(NoBin),
}
pub struct BinFacts {
    pub drive_type: u32,
    pub unc: bool,
    pub size: u64,
    pub bin_free: Option<u64>,
    pub bin_disabled: Option<bool>,
}

/// Pure.
pub fn classify_facts(f: &BinFacts) -> Recyclability {
    let _ = f;
    todo!("F-OPS")
}

/// OS reads only; off the UI thread.
pub fn gather_facts(item: &WidePath) -> BinFacts {
    let _ = item;
    todo!("F-OPS")
}
