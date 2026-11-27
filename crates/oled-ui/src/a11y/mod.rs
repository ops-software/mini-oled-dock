//! The accessibility tree and the AccessKit bridge (kit KG, SPEC-platform 5.3.7). tree.rs turns a window's frame into
//! the tree a client reads; it is pure and always compiled, so its tests run with and without the feature. bridge.rs,
//! behind the `a11y` feature, hands that tree to one accesskit_windows Adapter per window. The action queue below
//! carries a client's requests, made on the client's threads, to the UI thread.
// Without the feature only the tests read the tree and the queue.
#![cfg_attr(not(feature = "a11y"), allow(dead_code))]

#[cfg(feature = "a11y")]
pub(crate) mod bridge;
pub(crate) mod tree;

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard};

use crate::event::A11yAction;
use crate::node::NodeId;

/// A client's action requests on their way to the UI thread: the handler AccessKit calls on any thread queues the
/// request and posts one WM_OLED_A11Y; the window procedure drains the queue into Event::A11y.
#[derive(Default)]
pub(crate) struct ActionQueue {
    inner: Mutex<Pending>,
}

#[derive(Default)]
struct Pending {
    items: VecDeque<(NodeId, A11yAction)>,
    /// A wake-up is posted and the queue was not drained since.
    posted: bool,
    /// The window is gone: requests are dropped.
    closed: bool,
}

impl ActionQueue {
    /// At most this many requests wait; a client that floods a window stuck that long loses the newest.
    pub(crate) const CAP: usize = 256;

    fn lock(&self) -> MutexGuard<'_, Pending> {
        // A panic while the lock was held leaves the queue consistent: take it as it is.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queues a request; true when the caller must post the wake-up (none is in flight).
    pub(crate) fn push(&self, node: NodeId, action: A11yAction) -> bool {
        let mut p = self.lock();
        if p.closed || p.items.len() >= Self::CAP {
            return false;
        }
        p.items.push_back((node, action));
        !std::mem::replace(&mut p.posted, true)
    }

    /// The wake-up could not be posted: the next request tries again.
    pub(crate) fn post_failed(&self) {
        self.lock().posted = false;
    }

    /// Every waiting request, oldest first; the next request posts a new wake-up.
    pub(crate) fn drain(&self) -> Vec<(NodeId, A11yAction)> {
        let mut p = self.lock();
        p.posted = false;
        p.items.drain(..).collect()
    }

    /// The window is gone: what waits is dropped and later requests are refused.
    pub(crate) fn close(&self) {
        let mut p = self.lock();
        p.closed = true;
        p.items.clear();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    use super::*;
    use crate::dispatch::Input;
    use crate::geom::SizeL;
    use crate::harness::Harness;
    use crate::node::{Text, WidgetId};
    use crate::theme::Tokens;
    use crate::widgets::paint::Host;
    use crate::widgets::{Button, ButtonKind, ButtonOut};

    /// A stand-in for AccessKit's action handler: it queues on the client's thread and counts the wake-ups it posts.
    struct FakeHandler {
        queue: Arc<ActionQueue>,
        posts: Arc<AtomicU32>,
    }

    impl FakeHandler {
        fn act(&self, node: NodeId, action: A11yAction) {
            if self.queue.push(node, action) {
                self.posts.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    #[test]
    fn actions_marshal_to_events() {
        let b = Button::new(WidgetId(1), Text::from("OK"), ButtonKind::Primary);
        let node = b.node_id();
        let mut h = Harness::new(Host::new(b, (10, 8, 100, 28)), SizeL { w: 120, h: 44 }, 96, Tokens::oled());
        let queue = Arc::new(ActionQueue::default());
        let posts = Arc::new(AtomicU32::new(0));
        let fake = FakeHandler { queue: Arc::clone(&queue), posts: Arc::clone(&posts) };
        // The client's thread: a burst of two requests posts one wake-up.
        std::thread::spawn(move || {
            fake.act(node, A11yAction::Focus);
            fake.act(node, A11yAction::Click);
        })
        .join()
        .expect("the client's thread");
        assert_eq!(posts.load(Ordering::SeqCst), 1, "one wake-up for a burst");
        // The UI thread (WM_OLED_A11Y): the queue drains into the dispatcher, in order.
        for (n, a) in queue.drain() {
            h.input(Input::A11y(n, a));
        }
        assert_eq!(h.screen().outs, vec![ButtonOut::Clicked], "Event::A11y reached the button");
        assert!(queue.drain().is_empty());
        // After a drain the next request wakes the UI thread again; a failed post is retried; a closed queue refuses.
        assert!(queue.push(node, A11yAction::Click));
        assert!(!queue.push(node, A11yAction::Click), "a wake-up is already in flight");
        queue.post_failed();
        assert!(queue.push(node, A11yAction::Click), "the failed post is retried");
        queue.close();
        assert!(!queue.push(node, A11yAction::Click) && queue.drain().is_empty());
    }

    #[test]
    fn the_queue_is_bounded() {
        let q = ActionQueue::default();
        let node = NodeId::new(WidgetId(1), 0);
        let posted = (0..ActionQueue::CAP + 5).filter(|_| q.push(node, A11yAction::Click)).count();
        assert_eq!((posted, q.drain().len()), (1, ActionQueue::CAP));
    }
}
