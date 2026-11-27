//! Screen and Widget (F-K1, P2-kit 2.5): what an app implements. A Screen is one window's content; a Widget is a
//! concrete struct with a typed output.

use crate::command::CommandTable;
use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::{Event, Reply};
use crate::geom::{PointP, RectP, SizeP};
use crate::host::AppMsg;

/// One window's content.
pub trait Screen {
    /// Pushes the nodes; layered windows are sized to the result.
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP;
    /// cx.clip() = band ∩ damage.
    fn paint(&self, cx: &mut PaintCx);
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply;
    /// A message from Proxy::send (or Harness::send).
    fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
        let _ = (cx, msg);
        Reply::None
    }
    /// A WM_COPYDATA sent to the window (TK1): `tag` is its dwData, `bytes` a copy of its payload (at most 64 KiB: the
    /// window refuses a larger one before any call). The answer is what the sender's SendMessage returns.
    fn copy_data(&mut self, cx: &mut EventCx, tag: usize, bytes: &[u8]) -> bool {
        let _ = (cx, tag, bytes);
        false
    }
    /// A message the app routed to the window with EventLoop::route_message (TK1), posted or sent to it.
    fn app_message(&mut self, cx: &mut EventCx, msg: u32, wparam: usize, lparam: isize) -> Reply {
        let _ = (cx, msg, wparam, lparam);
        Reply::None
    }
    fn commands(&self) -> &CommandTable;
    /// Custom caption only (K3.5).
    fn caption_hit(&self, p: PointP) -> CaptionHit {
        let _ = p;
        CaptionHit::Client
    }
}

/// A widget: a concrete struct with a typed output.
pub trait Widget {
    type Out;
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP);
    fn paint(&self, cx: &mut PaintCx);
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<Self::Out>;
    fn preferred(&mut self, cx: &mut LayoutCx) -> SizeP {
        let _ = cx;
        SizeP::default()
    }
}

/// What a point of a custom caption is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptionHit {
    Client,
    Caption,
    MinButton,
    MaxButton,
    CloseButton,
}

impl<S: Screen + ?Sized> Screen for Box<S> {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        (**self).layout(cx, avail)
    }

    fn paint(&self, cx: &mut PaintCx) {
        (**self).paint(cx)
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        (**self).event(cx, ev)
    }

    fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
        (**self).data(cx, msg)
    }

    fn copy_data(&mut self, cx: &mut EventCx, tag: usize, bytes: &[u8]) -> bool {
        (**self).copy_data(cx, tag, bytes)
    }

    fn app_message(&mut self, cx: &mut EventCx, msg: u32, wparam: usize, lparam: isize) -> Reply {
        (**self).app_message(cx, msg, wparam, lparam)
    }

    fn commands(&self) -> &CommandTable {
        (**self).commands()
    }

    fn caption_hit(&self, p: PointP) -> CaptionHit {
        (**self).caption_hit(p)
    }
}
