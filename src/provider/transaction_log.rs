// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

use chrono::Utc;
use serde::Serialize;

#[derive(Serialize)]
pub(crate) struct Transaction {
    pub timestamp: String,
    pub host: String,
    pub path: String,
    pub request_headers: HashMap<String, String>,
    pub request_body: serde_json::Value,
    pub response_headers: HashMap<String, String>,
    pub response_body: serde_json::Value,
}

struct LogState {
    active: bool,
    path: PathBuf,
}

static STATE: OnceLock<RwLock<LogState>> = OnceLock::new();

fn get_state() -> &'static RwLock<LogState> {
    STATE.get_or_init(|| {
        RwLock::new(LogState {
            active: false,
            path: PathBuf::from("henri-transactions.json"),
        })
    })
}

pub(crate) fn start(path: Option<PathBuf>) -> PathBuf {
    let mut state = get_state().write().unwrap();
    state.active = true;
    if let Some(p) = path {
        state.path = p;
    }
    state.path.clone()
}

pub(crate) fn stop() {
    let mut state = get_state().write().unwrap();
    state.active = false;
}

pub(crate) fn is_active() -> bool {
    get_state().read().unwrap().active
}

pub(crate) fn log(
    url: &str,
    request_headers: HashMap<String, String>,
    request_body: serde_json::Value,
    response_headers: HashMap<String, String>,
    response_body: serde_json::Value,
) {
    if !is_active() {
        return;
    }

    let parsed_url = match url::Url::parse(url) {
        Ok(u) => u,
        Err(_) => return,
    };

    let transaction = Transaction {
        timestamp: Utc::now().to_rfc3339(),
        host: parsed_url.host_str().unwrap_or_default().to_string(),
        path: parsed_url.path().to_string(),
        request_headers,
        request_body,
        response_headers,
        response_body,
    };

    let state = get_state().read().unwrap();
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&state.path)
        && let Ok(line) = serde_json::to_string(&transaction)
    {
        let _ = writeln!(file, "{}", line);
    }
}

pub(crate) fn header_map_to_hash_map(
    headers: &reqwest::header::HeaderMap,
) -> HashMap<String, String> {
    headers
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("<binary>").to_string()))
        .collect()
}
