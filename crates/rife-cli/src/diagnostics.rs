// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Shared presentation data for media and native backend failures.
#[derive(Debug)]
pub(crate) struct Report {
    pub reason: String,
    pub details: Vec<String>,
}
