//! The shell host's writer thread (SH4a): one queue of frames, one WriteFile at a time, so an STA never blocks on the
//! pipe and two frames never interleave.

use std::io::Write;
use std::sync::mpsc;

use oled_trust::frame;
use oled_trust::frame::files::{ReqId, ShellEvt};
use windows::Win32::Foundation::E_FAIL;

/// A handle to the writer thread; clones share it. The thread ends when every Writer is gone or the pipe breaks.
#[derive(Clone)]
pub struct Writer {
    tx: mpsc::Sender<Vec<u8>>,
}

impl Writer {
    /// Starts the thread that writes to `out`.
    pub fn start(mut out: impl Write + Send + 'static) -> std::io::Result<Writer> {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        std::thread::Builder::new().name("shell-writer".into()).stack_size(64 * 1024).spawn(move || {
            while let Ok(bytes) = rx.recv() {
                if out.write_all(&bytes).and_then(|()| out.flush()).is_err() {
                    break;
                }
            }
        })?;
        Ok(Writer { tx })
    }

    /// Queues bytes already framed (the Hello).
    pub fn raw(&self, bytes: Vec<u8>) {
        let _ = self.tx.send(bytes);
    }

    /// Queues one event. One that cannot be encoded (it does not fit a frame) is logged and replaced by Failed for its
    /// request, so the client is never left waiting.
    pub fn send(&self, evt: &ShellEvt) {
        let mut out = Vec::new();
        match frame::encode(evt, &mut out) {
            Ok(()) => self.raw(out),
            Err(e) => {
                oled_sys::log!("shell: an event does not fit a frame ({e:?})");
                if let Some(req) = req_of(evt) {
                    self.send(&ShellEvt::Failed { req, hr: E_FAIL.0 });
                }
            }
        }
    }
}

/// The request an event answers, if any.
pub fn req_of(evt: &ShellEvt) -> Option<ReqId> {
    use ShellEvt::*;
    match evt {
        MenuShown { req }
        | MenuDone { req, .. }
        | Modern { req, .. }
        | ExecDone { req, .. }
        | Icon { req, .. }
        | Thumb { req, .. }
        | ClassIcon { req, .. }
        | TypeNames { req, .. }
        | ShellNew { req, .. }
        | Pins { req, .. }
        | Failed { req, .. }
        | ReqDone { req } => Some(*req),
        Ready { .. } | Busy { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A sink that keeps what the thread wrote.
    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().map_err(|_| std::io::ErrorKind::Other)?.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn frames_are_written_whole_and_in_order() {
        let sink = Sink::default();
        let w = Writer::start(sink.clone()).unwrap();
        let evts: Vec<ShellEvt> = (0..50).map(|i| ShellEvt::ExecDone { req: ReqId(i), hr: 0 }).collect();
        for e in &evts {
            w.send(e);
        }
        drop(w);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut got = Vec::new();
        while got.len() < evts.len() && std::time::Instant::now() < deadline {
            let bytes = sink.0.lock().unwrap().clone();
            got.clear();
            let mut at = 0;
            while let Ok((e, n)) = frame::decode::<ShellEvt>(&bytes[at..]) {
                got.push(e);
                at += n;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(got, evts);
        assert_eq!(req_of(&ShellEvt::Busy { on: true }), None);
        assert_eq!(req_of(&ShellEvt::ReqDone { req: ReqId(9) }), Some(ReqId(9)));
    }
}
