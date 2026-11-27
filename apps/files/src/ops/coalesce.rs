//! The coalescer (OP3): what one operation sends the UI. Progress at most once per 100 ms (10 per second), the
//! ItemsDone of the same tick with it, split by bytes (2.2); Started once; Finished after the last ItemsDone and
//! UndoPart. Undo entries stream: an UndoPart leaves as soon as the entries held would no longer fit one frame or
//! reach MAX_LIST, so at most MAX_LIST are held; past UNDO_MAX_ENTRIES top-level entries, or when an UndoPart is due
//! while no UI is attached, the operation's undo is dropped (no more UndoPart, Finished carries undo: None). Pure:
//! the caller passes the time.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use oled_trust::frame::files::{Change, OpEvt, OpId, OpResult, UndoEntry, UndoKind, MAX_LIST, OPS, UNDO_MAX_ENTRIES};
use oled_trust::frame::{WireLen, SPLIT_MARGIN};

/// The shortest gap between two Progress events of one operation.
pub const TICK: Duration = Duration::from_millis(100);
/// The window of the bytes-per-second figure.
const BPS_WINDOW: Duration = Duration::from_secs(1);
/// Bytes of Progress.current at most.
const CURRENT_MAX: usize = 520;

/// The payload bytes one list frame of the ops protocol may carry (split_by_bytes' limit).
fn frame_limit() -> usize {
    OPS.max_payload.saturating_sub(SPLIT_MARGIN) as usize
}

/// A list of elements held until it is sent, never more than one frame's worth.
struct Batch<T> {
    items: Vec<T>,
    bytes: usize,
}

impl<T: WireLen> Batch<T> {
    fn new() -> Self {
        Batch { items: Vec::new(), bytes: 0 }
    }

    /// The fixed fields (op id) and the count before the elements.
    const BASE: usize = 4 + 4;

    /// `item` would not fit in the frame of the items held now.
    fn would_overflow(&self, item: &T) -> bool {
        !self.items.is_empty() && Self::BASE + self.bytes + item.wire_len() > frame_limit()
    }

    fn push(&mut self, item: T) {
        self.bytes += item.wire_len();
        self.items.push(item);
    }

    fn full(&self) -> bool {
        self.items.len() >= MAX_LIST as usize
    }

    fn take(&mut self) -> Vec<T> {
        self.bytes = 0;
        std::mem::take(&mut self.items)
    }
}

pub struct Coalescer {
    op: OpId,
    last_progress: Option<Instant>,
    done: u64,
    total: u64,
    current: Box<str>,
    samples: VecDeque<(Instant, u64)>,
    changes: Batch<Change>,
    undo: Batch<UndoEntry>,
    undo_seen: u32,
    undo_dropped: bool,
    held_max: usize,
    /// A Progress was held back by the tick: the end sends it, so the UI sees the last figures.
    progress_pending: bool,
    /// The bytes per second of the last Progress.
    bps: u64,
}

impl Coalescer {
    pub fn new(op: OpId) -> Coalescer {
        Coalescer {
            op,
            last_progress: None,
            done: 0,
            total: 0,
            current: "".into(),
            samples: VecDeque::new(),
            changes: Batch::new(),
            undo: Batch::new(),
            undo_seen: 0,
            undo_dropped: false,
            held_max: 0,
            progress_pending: false,
            bps: 0,
        }
    }

    /// The item being worked on (its name, cut to 520 bytes on a character boundary).
    pub fn current(&mut self, name: &str) {
        let mut end = name.len().min(CURRENT_MAX);
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        self.current = name.get(..end).unwrap_or_default().into();
    }

    /// UpdateProgress: the work done of the total.
    pub fn progress(&mut self, done: u64, total: u64, now: Instant) -> Vec<OpEvt> {
        self.done = done;
        self.total = total;
        self.samples.push_back((now, done));
        while self.samples.front().is_some_and(|(t, _)| now.saturating_duration_since(*t) > BPS_WINDOW) {
            self.samples.pop_front();
        }
        let out = self.tick(now);
        if out.is_empty() {
            self.progress_pending = true;
        }
        out
    }

    /// One top-level item done: its change leaves with the next tick, or at once when the batch is full.
    pub fn change(&mut self, c: Change, now: Instant) -> Vec<OpEvt> {
        let mut out = Vec::new();
        if self.changes.would_overflow(&c) {
            out.push(self.items_done());
        }
        self.changes.push(c);
        if self.changes.full() {
            out.push(self.items_done());
        }
        out.extend(self.tick(now));
        out
    }

    /// One top-level undo entry.
    pub fn undo(&mut self, e: UndoEntry, attached: bool) -> Vec<OpEvt> {
        if self.undo_dropped {
            return Vec::new();
        }
        self.undo_seen = self.undo_seen.saturating_add(1);
        if self.undo_seen > UNDO_MAX_ENTRIES {
            self.drop_undo();
            return Vec::new();
        }
        let mut out = Vec::new();
        if self.undo.would_overflow(&e) {
            out.extend(self.undo_part(attached));
        }
        if self.undo_dropped {
            return out;
        }
        self.undo.push(e);
        self.held_max = self.held_max.max(self.undo.items.len());
        if self.undo.full() {
            out.extend(self.undo_part(attached));
        }
        out
    }

    /// The end: the changes and undo entries still held, then Finished. `undo` is the kind the operation would
    /// offer; it is None in Finished when the record was dropped or nothing was sent.
    pub fn finish(
        &mut self,
        result: OpResult,
        undo: Option<UndoKind>,
        replaced: u32,
        items: u32,
        failed: u32,
        attached: bool,
    ) -> Vec<OpEvt> {
        let mut out = Vec::new();
        if self.progress_pending {
            out.push(self.progress_evt());
        }
        if !self.changes.items.is_empty() {
            out.push(self.items_done());
        }
        if undo.is_none() {
            self.drop_undo();
        }
        if !self.undo.items.is_empty() {
            out.extend(self.undo_part(attached));
        }
        let undo = if self.undo_dropped { None } else { undo };
        out.push(OpEvt::Finished { op: self.op, result, undo, replaced, items, failed });
        out
    }

    /// The undo record was dropped.
    pub fn undo_dropped(&self) -> bool {
        self.undo_dropped
    }

    /// The most undo entries held at once (tests).
    pub fn held_max(&self) -> usize {
        self.held_max
    }

    fn drop_undo(&mut self) {
        self.undo_dropped = true;
        self.undo = Batch::new();
    }

    fn undo_part(&mut self, attached: bool) -> Option<OpEvt> {
        if !attached {
            self.drop_undo();
            return None;
        }
        Some(OpEvt::UndoPart { op: self.op, entries: self.undo.take() })
    }

    fn items_done(&mut self) -> OpEvt {
        OpEvt::ItemsDone { op: self.op, changes: self.changes.take() }
    }

    fn tick(&mut self, now: Instant) -> Vec<OpEvt> {
        if self.last_progress.is_some_and(|t| now.saturating_duration_since(t) < TICK) {
            return Vec::new();
        }
        self.last_progress = Some(now);
        self.bps = self.bps(now);
        let mut out = vec![self.progress_evt()];
        if !self.changes.items.is_empty() {
            out.push(self.items_done());
        }
        out
    }

    fn progress_evt(&mut self) -> OpEvt {
        self.progress_pending = false;
        OpEvt::Progress {
            op: self.op,
            done: self.done,
            total: self.total,
            bps: self.bps,
            current: self.current.clone(),
        }
    }

    fn bps(&self, now: Instant) -> u64 {
        let Some(&(t0, d0)) = self.samples.front() else { return 0 };
        let span = now.saturating_duration_since(t0).as_millis() as u64;
        if span == 0 {
            return 0;
        }
        self.done.saturating_sub(d0).saturating_mul(1000) / span
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_trust::frame::files::{ChangeKind, WidePath};
    use oled_trust::frame::{decode, encode};

    fn long_path(i: u32) -> WidePath {
        let mut s = format!("C:\\{i:08}\\");
        while s.len() < 32_767 {
            s.push('a');
        }
        s.truncate(32_767);
        WidePath::parse(&s).unwrap()
    }

    fn round_trip(e: &OpEvt) -> OpEvt {
        let mut buf = Vec::new();
        encode(e, &mut buf).expect("every frame the coalescer makes fits");
        decode::<OpEvt>(&buf).unwrap().0
    }

    #[test]
    fn progress_rate() {
        let t0 = Instant::now();
        let mut c = Coalescer::new(OpId(1));
        let mut times = Vec::new();
        for ms in 0..3_000u64 {
            let now = t0 + Duration::from_millis(ms);
            for e in c.progress(ms * 1000, 3_000_000, now) {
                if matches!(e, OpEvt::Progress { .. }) {
                    times.push(ms);
                }
            }
        }
        for start in 0..2_000u64 {
            let n = times.iter().filter(|&&t| t >= start && t < start + 1000).count();
            assert!(n <= 10, "{n} Progress events in the second from {start} ms");
        }
        assert!(times.len() >= 29, "about one per 100 ms: {}", times.len());
        let mut held = Coalescer::new(OpId(9));
        assert_eq!(held.progress(1, 10, t0).len(), 1);
        assert!(held.progress(10, 10, t0 + Duration::from_millis(5)).is_empty(), "held back by the tick");
        let fin = held.finish(OpResult::Done, None, 0, 1, 0, true);
        assert!(matches!(fin[0], OpEvt::Progress { done: 10, total: 10, .. }), "the end sends the last figures");
        let last = c.progress(3_000_000, 3_000_000, t0 + Duration::from_millis(3_200));
        match &last[..] {
            [OpEvt::Progress { done, total, bps, .. }] => {
                assert_eq!((done, total), (&3_000_000, &3_000_000));
                assert!(*bps > 0);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn items_done_split_by_bytes_none_lost() {
        let t0 = Instant::now();
        let mut c = Coalescer::new(OpId(2));
        let mut frames = Vec::new();
        for i in 0..2_048u32 {
            let ch = Change { kind: ChangeKind::Copied, from: Some(long_path(i)), to: Some(long_path(i + 10_000)) };
            frames.extend(c.change(ch, t0));
        }
        frames.extend(c.finish(OpResult::Done, None, 0, 2_048, 0, true));
        let mut got = Vec::new();
        let mut n_frames = 0;
        for f in &frames {
            if let OpEvt::ItemsDone { changes, .. } = round_trip(f) {
                n_frames += 1;
                got.extend(changes);
            }
        }
        assert!(n_frames > 1, "several frames");
        assert_eq!(got.len(), 2_048, "none lost");
        for (i, ch) in got.iter().enumerate() {
            assert_eq!(ch.from.as_ref(), Some(&long_path(i as u32)), "in order");
        }
        assert!(matches!(frames.last(), Some(OpEvt::Finished { .. })), "Finished last");
    }

    #[test]
    fn undo_stream_is_bounded() {
        let mut c = Coalescer::new(OpId(3));
        let path = |i: u32| WidePath::parse(&format!("C:\\d\\{i}")).unwrap();
        let mut parts = Vec::new();
        for i in 0..20_000u32 {
            let e = UndoEntry { from: path(i), to: path(i + 100_000), size: 1, mtime: 2, dir: false, bin: None };
            parts.extend(c.undo(e, true));
            assert!(c.undo.items.len() <= MAX_LIST as usize);
        }
        assert!(c.held_max() <= MAX_LIST as usize, "never more than MAX_LIST held");
        let mut sent = 0;
        for p in &parts {
            match round_trip(p) {
                OpEvt::UndoPart { entries, .. } => sent += entries.len(),
                other => panic!("{other:?}"),
            }
        }
        assert!(sent <= UNDO_MAX_ENTRIES as usize);
        assert!(c.undo_dropped(), "past UNDO_MAX_ENTRIES the record is dropped");
        let fin = c.finish(OpResult::Done, Some(UndoKind::RecycleCopies), 0, 20_000, 0, true);
        assert!(matches!(fin.last(), Some(OpEvt::Finished { undo: None, .. })));
        assert!(!fin.iter().any(|e| matches!(e, OpEvt::UndoPart { .. })), "no more UndoPart once dropped");
        // A small record arrives whole before Finished.
        let mut c = Coalescer::new(OpId(4));
        let e = UndoEntry { from: path(1), to: path(2), size: 1, mtime: 2, dir: false, bin: None };
        assert!(c.undo(e.clone(), true).is_empty());
        let fin = c.finish(OpResult::Done, Some(UndoKind::RecycleCopies), 0, 1, 0, true);
        assert_eq!(fin.len(), 2);
        assert_eq!(fin[0], OpEvt::UndoPart { op: OpId(4), entries: vec![e.clone()] });
        assert!(matches!(fin[1], OpEvt::Finished { undo: Some(UndoKind::RecycleCopies), .. }));
        // No UI attached when a part is due: dropped.
        let mut c = Coalescer::new(OpId(5));
        c.undo(e.clone(), true);
        let fin = c.finish(OpResult::Done, Some(UndoKind::RecycleCopies), 0, 1, 0, false);
        assert_eq!(fin.len(), 1);
        assert!(matches!(fin[0], OpEvt::Finished { undo: None, .. }));
    }

    #[test]
    fn current_is_cut_on_a_char_boundary() {
        let mut c = Coalescer::new(OpId(1));
        c.current(&"é".repeat(400));
        assert!(c.current.len() <= 520 && c.current.len().is_multiple_of(2));
        c.current("a.txt");
        assert_eq!(&*c.current, "a.txt");
    }
}
