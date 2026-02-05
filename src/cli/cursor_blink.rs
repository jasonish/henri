// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Jason Ish

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static CURSOR_BLINK_VISIBLE: AtomicBool = AtomicBool::new(true);
static CURSOR_BLINK_GEN: AtomicU64 = AtomicU64::new(0);
static CURSOR_BLINK_STARTED: AtomicBool = AtomicBool::new(false);

pub(crate) fn init() {
    if CURSOR_BLINK_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }

    tokio::spawn(async {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
        loop {
            interval.tick().await;
            CURSOR_BLINK_VISIBLE.fetch_xor(true, Ordering::AcqRel);
            CURSOR_BLINK_GEN.fetch_add(1, Ordering::Release);
        }
    });
}

pub(crate) fn visible() -> bool {
    CURSOR_BLINK_VISIBLE.load(Ordering::Acquire)
}

pub(crate) fn generation() -> u64 {
    CURSOR_BLINK_GEN.load(Ordering::Acquire)
}
