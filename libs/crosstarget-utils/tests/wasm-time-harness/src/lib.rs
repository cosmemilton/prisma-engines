// Added for experimental Firebird support (2026-10-18); see MODIFICATIONS.md.
//! Test-only exports for the real shared Wasm timer implementation.

use std::{
    future::Future,
    task::{Context, Waker},
    time::Duration,
};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(js_name = normalSleep)]
pub async fn normal_sleep() {
    crosstarget_utils::time::sleep(Duration::from_millis(25)).await;
}

#[wasm_bindgen(js_name = workWins60Sec)]
pub async fn work_wins_60_sec() -> bool {
    crosstarget_utils::time::timeout(
        Duration::from_secs(60),
        crosstarget_utils::time::sleep(Duration::from_millis(25)),
    )
    .await
    .is_ok()
}

#[wasm_bindgen(js_name = timeoutWinsDrop60Sec)]
pub async fn timeout_wins_drop_60_sec() -> bool {
    crosstarget_utils::time::timeout(
        Duration::from_millis(25),
        crosstarget_utils::time::sleep(Duration::from_secs(60)),
    )
    .await
    .is_err()
}

#[wasm_bindgen(js_name = cancelAfterPending)]
pub fn cancel_after_pending() -> bool {
    let future = crosstarget_utils::time::sleep(Duration::from_secs(60));
    let mut future = std::pin::pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    // Poll once so a real timer exists before the future is canceled.
    future.as_mut().poll(&mut context).is_pending()
}
