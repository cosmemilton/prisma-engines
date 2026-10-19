// Modified for experimental Firebird support (2026-10-18); see MODIFICATIONS.md.
// `clippy::empty_docs` is required because of the `wasm-bindgen` crate.
#![allow(clippy::empty_docs)]

use std::{
    cell::RefCell,
    future::Future,
    rc::Rc,
    task::{Poll, Waker},
    time::Duration,
};

pub use crate::common::datetime::*;
use derive_more::Display;
use js_sys::{Date, Function, Reflect};
use wasm_bindgen::prelude::*;

use crate::common::timeout::TimeoutError;

#[wasm_bindgen]
extern "C" {
    type Performance;

    #[wasm_bindgen(method)]
    fn now(this: &Performance) -> f64;

    #[wasm_bindgen(js_name = setTimeout)]
    fn set_timeout(closure: &Function, millis: u32) -> JsValue;

    #[wasm_bindgen(js_name = clearTimeout)]
    fn clear_timeout(handle: &JsValue);

}

#[derive(Clone, Copy)]
pub struct ElapsedTimeCounter {
    start_time: f64,
}

impl ElapsedTimeCounter {
    pub fn start() -> Self {
        Self { start_time: now() }
    }

    pub fn elapsed_time(&self) -> Duration {
        Duration::from_millis((self.start_time - now()) as u64)
    }
}

#[derive(Copy, Clone, Debug)]
pub struct SystemTime(Duration);

impl SystemTime {
    pub const UNIX_EPOCH: Self = Self(Duration::ZERO);

    pub fn now() -> Self {
        let ms = js_sys::Date::now() as i64;
        let ms = ms.try_into().expect("negative timestamps are not supported");
        Self(Duration::from_millis(ms))
    }

    pub fn duration_since(&self, other: Self) -> Result<Duration, SystemTimeError> {
        self.0
            .checked_sub(other.0)
            .ok_or_else(|| SystemTimeError(other.0 - self.0))
    }
}

impl std::ops::Add<Duration> for SystemTime {
    type Output = Self;

    fn add(self, rhs: Duration) -> SystemTime {
        Self(self.0 + rhs)
    }
}

#[derive(Clone, Debug, Display)]
#[display("second time provided was later than self")]
pub struct SystemTimeError(Duration);

impl SystemTimeError {
    pub fn duration(&self) -> Duration {
        self.0
    }
}

impl std::error::Error for SystemTimeError {}

#[derive(Default)]
struct SleepState {
    fired: bool,
    waker: Option<Waker>,
}

struct JsSleep {
    handle: JsValue,
    // Own the callback until the timer is cleared. A canceled unresolved Promise
    // would otherwise retain its Rust-backed callbacks until it settles.
    _callback: Closure<dyn FnMut()>,
    state: Rc<RefCell<SleepState>>,
}

impl Future for JsSleep {
    type Output = ();

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<()> {
        let mut state = self.state.borrow_mut();
        if state.fired {
            Poll::Ready(())
        } else {
            state.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

impl JsSleep {
    fn new(duration: Duration) -> Self {
        let state = Rc::new(RefCell::new(SleepState::default()));
        let callback_state = Rc::clone(&state);
        let callback = Closure::wrap(Box::new(move || {
            let waker = {
                let mut state = callback_state.borrow_mut();
                state.fired = true;
                state.waker.take()
            };
            if let Some(waker) = waker {
                waker.wake();
            }
        }) as Box<dyn FnMut()>);
        let handle = set_timeout(callback.as_ref().unchecked_ref(), duration.as_millis() as u32);
        Self {
            handle,
            _callback: callback,
            state,
        }
    }
}

impl Drop for JsSleep {
    fn drop(&mut self) {
        clear_timeout(&self.handle);
        self.state.borrow_mut().waker.take();
    }
}

// Like the previous JsFuture wrapper, this future stays on its Wasm instance's
// JavaScript thread; it is not transferred between workers.
unsafe impl Send for JsSleep {}

pub async fn sleep(duration: Duration) {
    JsSleep::new(duration).await;
}

pub async fn timeout<F>(duration: Duration, future: F) -> Result<F::Output, TimeoutError>
where
    F: Future,
{
    tokio::select! {
        result = future => Ok(result),
        _ = sleep(duration) => Err(TimeoutError)
    }
}

fn now() -> f64 {
    let global = js_sys::global();
    Reflect::get(&global, &"performance".into())
        .ok()
        .and_then(|value| {
            if value.is_undefined() {
                None
            } else {
                Some(Performance::from(value))
            }
        })
        .map(|p| p.now())
        .unwrap_or_else(Date::now)
}
