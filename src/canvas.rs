//! `Send`/`Sync` canvas wrapper.
//!
//! Aurea's `Canvas` is not `Send`, but Glacia shares one canvas between the
//! window content and the draw loop behind an `Arc<Mutex<…>>`. This wrapper
//! asserts the threading contract (the canvas is only ever touched under the
//! lock on the UI thread) and forwards `Element` to the inner canvas.

use std::ops::{Deref, DerefMut};
use std::os::raw::c_void;
use std::sync::{Arc, Mutex, MutexGuard};

use aurea::Element;
use aurea::render::{Canvas, Rect};

/// Locks the mutex, recovering from poisoning rather than panicking.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A `Canvas` marked `Send`/`Sync` so it can live in an `Arc<Mutex<…>>`.
/// Sound because every access goes through the mutex.
pub struct SendableCanvas(pub Canvas);
unsafe impl Send for SendableCanvas {}
unsafe impl Sync for SendableCanvas {}

impl Deref for SendableCanvas {
    type Target = Canvas;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl DerefMut for SendableCanvas {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Element for SendableCanvas {
    fn handle(&self) -> *mut c_void {
        self.0.handle()
    }
    unsafe fn invalidate_platform(&self, rect: Option<Rect>) {
        unsafe { Element::invalidate_platform(&self.0, rect) }
    }
}

/// The window-content handle: a shared `SendableCanvas`. Set as the window's
/// content so platform input/paint reach the same canvas the run loop draws to.
pub struct SharedCanvas(pub Arc<Mutex<SendableCanvas>>);
impl Element for SharedCanvas {
    fn handle(&self) -> *mut c_void {
        lock(self.0.as_ref()).handle()
    }
    unsafe fn invalidate_platform(&self, rect: Option<Rect>) {
        let g = lock(self.0.as_ref());
        unsafe { Element::invalidate_platform(&*g, rect) }
    }
}
