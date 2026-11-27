//! The EventLoop and LoopCx (P2-kit 2.5; K1a.7, K1a.11, KA2): one loop per UI thread, its windows, the handles it
//! waits on and the theme it follows. K0.1b writes the types; until K1a.7 new() refuses, so nothing here runs.
#![allow(dead_code)] // The fields are read by the loop K1a.7 writes.

use std::marker::PhantomData;

use windows::Win32::Foundation::{HANDLE, HWND};

use super::{AppId, HandleId, PlacementStore, Proxy, UiError, WindowRef, WindowSpec};
use crate::screen::Screen;
use crate::strings::{Lang, UiLang};
use crate::theme::{ThemeFile, ThemeSpec};

/// The UI thread's loop. Not Send: one per UI thread.
pub struct EventLoop {
    app: AppId,
    lang: UiLang,
    theme: ThemeSpec,
    store: Option<Box<dyn PlacementStore>>,
    quit: Option<i32>,
    caption_spike: bool,
    _ui_thread: PhantomData<*const ()>,
}

impl EventLoop {
    /// A loop for `app` speaking `lang` (resolved by UiLang::from_lang) with `theme`. Filled by K1a.7; until then
    /// Err(UiError::NotReady("K1a.7")).
    pub fn new(app: AppId, lang: Lang, theme: ThemeSpec) -> Result<EventLoop, UiError> {
        let _ = (app, lang, theme);
        Err(UiError::NotReady("K1a.7"))
    }

    pub fn set_placement_store(&mut self, s: Box<dyn PlacementStore>) {
        self.store = Some(s);
    }

    /// Opens a window with `screen`. Filled by K1a.7.
    pub fn open<S: Screen + 'static>(&mut self, spec: WindowSpec, screen: S) -> Result<WindowRef, UiError> {
        let _ = (spec, screen);
        Err(UiError::NotReady("K1a.7"))
    }

    /// The window's proxy. Filled by K1a.11; until then a proxy with no window.
    pub fn proxy(&self, w: WindowRef) -> Proxy {
        let _ = w;
        Proxy::detached()
    }

    /// Waits on `h` too (at most 56 handles); `on_signal` runs on this thread when it is signalled. Filled by K1a.11.
    pub fn add_handle(&mut self, h: HANDLE, on_signal: Box<dyn FnMut(&mut LoopCx)>) -> Result<HandleId, UiError> {
        let _ = (h, on_signal);
        Err(UiError::NotReady("K1a.11"))
    }

    /// Filled by K1a.11.
    pub fn remove_handle(&mut self, id: HandleId) {
        let _ = id;
    }

    /// Resolves the theme; if the generation changed, every window relayouts and repaints (K1a.11).
    pub fn set_theme(&mut self, spec: ThemeSpec) {
        self.theme = spec;
    }

    /// theme.json -> set_theme(ThemeSpec::from(file)) -> on_change (the app's language and the rest). Filled by K1a.11.
    #[allow(clippy::type_complexity)]
    pub fn follow_theme_file(
        &mut self,
        watch: oled_config::theme::ThemeWatch,
        on_change: Box<dyn FnMut(&mut LoopCx, &ThemeFile)>,
    ) -> Result<HandleId, UiError> {
        let _ = (watch, on_change);
        Err(UiError::NotReady("K1a.11"))
    }

    pub fn set_lang(&mut self, lang: Lang) {
        self.lang = UiLang::from_lang(lang);
    }

    /// The window's HWND, while it exists. Filled by K1a.7.
    pub fn hwnd(&self, w: WindowRef) -> Option<HWND> {
        let _ = w;
        None
    }

    /// S-CAPTION's rig only (K3.5): Custom takes effect for this loop's windows while CUSTOM_CAPTION is false. An app
    /// that calls it fails review.
    #[doc(hidden)]
    pub fn caption_spike(&mut self) {
        self.caption_spike = true;
    }

    /// Runs until the last opaque window is destroyed (0) or quit(code). Filled by K1a.7; with no window it returns 0.
    pub fn run(self) -> i32 {
        self.quit.unwrap_or(0)
    }
}

/// What a handle callback or the theme follower may do to the loop.
pub struct LoopCx<'a> {
    lp: &'a mut EventLoop,
}

impl LoopCx<'_> {
    /// Screen::data of window `w`. Filled by K1a.11.
    pub fn send(&mut self, w: WindowRef, msg: super::AppMsg) {
        let _ = (w, msg, &self.lp);
    }

    pub fn set_theme(&mut self, spec: ThemeSpec) {
        self.lp.set_theme(spec);
    }

    pub fn open<S: Screen + 'static>(&mut self, spec: WindowSpec, screen: S) -> Result<WindowRef, UiError> {
        self.lp.open(spec, screen)
    }

    /// Ends run() with `code`.
    pub fn quit(&mut self, code: i32) {
        self.lp.quit = Some(code);
    }
}
