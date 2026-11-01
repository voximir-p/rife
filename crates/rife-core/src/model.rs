// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    Result,
    settings::{INPUT_BLOB_0, INPUT_BLOB_1, INPUT_BLOB_2, MAX_PADDING, OUTPUT_BLOB},
};
use anyhow::{Context, ensure};
use std::collections::HashSet;

/// An executable-relative model directory name; compatibility is graph-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model(String);
impl Model {
    pub const DEFAULT: &str = "rife-v4.25";
    pub const PARAM_FILE: &str = "flownet.param";
    pub const WEIGHTS_FILE: &str = "flownet.bin";
    pub fn directory(&self) -> &str {
        &self.0
    }
}
impl Default for Model {
    fn default() -> Self {
        Self(Self::DEFAULT.into())
    }
}
impl std::fmt::Display for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::str::FromStr for Model {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        ensure!(
            !value.is_empty()
                && value != "."
                && value != ".."
                && !value
                    .chars()
                    .any(|c| c.is_control() || "\\/:*?\"<>|".contains(c))
                && !value.ends_with([' ', '.']),
            "model must be a single directory name"
        );
        Ok(Self(value.into()))
    }
}
#[derive(Debug)]
pub(crate) struct Metadata {
    pub padding: u32,
}

/// Validate the text graph before handing it to the older native parser. In
/// particular, declared counts and unresolved blobs must never become unchecked
/// native vector indices. We accept the layer set compiled into this backend.
pub(crate) fn validate(parameters: &[u8]) -> Result<Metadata> {
    ensure!(
        parameters.len() <= 16 * 1024 * 1024,
        "model parameter file is too large"
    );
    let text = std::str::from_utf8(parameters).context("model parameters must be UTF-8 text")?;
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    ensure!(
        lines.next() == Some("7767517"),
        "invalid NCNN parameter magic"
    );
    let counts = lines
        .next()
        .context("missing NCNN graph counts")?
        .split_whitespace()
        .map(str::parse::<usize>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(
        counts.len() == 2
            && (1..=100_000).contains(&counts[0])
            && (1..=1_000_000).contains(&counts[1]),
        "invalid NCNN graph counts"
    );
    let mut blobs = HashSet::new();
    let supported_layers: HashSet<&str> = include_str!("../native/layers.txt").lines().collect();
    let mut layer_count = 0;
    let mut input_names = HashSet::new();
    let mut scales = std::collections::HashMap::<&str, f64>::new();
    let mut alignment = 1.0_f64;
    for line in lines {
        layer_count += 1;
        let fields = line.split_whitespace().collect::<Vec<_>>();
        ensure!(fields.len() >= 4, "incomplete model layer");
        let kind = fields[0];
        ensure!(
            supported_layers.contains(kind),
            "unsupported NCNN layer {kind}"
        );
        let bottom: usize = fields[2].parse()?;
        let top: usize = fields[3].parse()?;
        let node_end = 4usize
            .checked_add(bottom)
            .and_then(|n| n.checked_add(top))
            .context("invalid model layer arity")?;
        ensure!(
            node_end <= fields.len() && bottom <= counts[1] && top <= counts[1],
            "invalid model layer arity"
        );
        ensure!(
            fields[1..node_end].iter().all(|name| name.len() <= 255),
            "model layer or blob name exceeds native parser limits"
        );
        let inputs = &fields[4..4 + bottom];
        for name in inputs {
            ensure!(blobs.contains(name), "unresolved model input {name}");
        }
        for name in &fields[4 + bottom..node_end] {
            ensure!(
                !blobs.contains(name) || inputs.contains(name),
                "duplicate model output {name}"
            );
            blobs.insert(*name);
        }
        ensure!(
            blobs.len() <= counts[1],
            "model exceeds its declared blob count"
        );
        let attributes = &fields[node_end..];
        for attribute in attributes {
            let (id, value) = attribute
                .split_once('=')
                .context("invalid model parameter")?;
            let id: i32 = id.parse()?;
            if id <= -23300 {
                let key = -(id as i64) - 23300;
                ensure!((0..32).contains(&key), "invalid model array parameter ID");
                let mut values = value.split(',');
                let count: usize = values.next().context("missing array size")?.parse()?;
                let values = values.collect::<Vec<_>>();
                ensure!(count == values.len(), "invalid model parameter array size");
                for value in values {
                    ensure!(
                        value.parse::<f32>()?.is_finite(),
                        "non-finite model parameter"
                    );
                }
            } else {
                ensure!(
                    (0..32).contains(&id) && value.parse::<f32>()?.is_finite(),
                    "invalid model parameter"
                );
            }
        }
        if kind == "Input" {
            ensure!(bottom == 0 && top == 1, "invalid RIFE input arity");
            input_names.insert(fields[4]);
        }
        // Propagate spatial scale through the graph. RIFE's coarse branches
        // require exact alignment after resizing and strided convolutions.
        // MemoryData contains learned constants and contributes no image scale.
        let attr = |id: i32, default: f64| -> Result<f64> {
            let prefix = format!("{id}=");
            attributes
                .iter()
                .find_map(|a| a.strip_prefix(&prefix))
                .map(str::parse)
                .transpose()
                .map(|v| v.unwrap_or(default))
                .map_err(Into::into)
        };
        let mut scale = inputs
            .iter()
            .filter_map(|name| scales.get(name).copied())
            .reduce(f64::min);
        if kind == "Input" {
            scale = Some(1.0);
        }
        if let Some(mut value) = scale {
            match kind {
                "Convolution" | "ConvolutionDepthWise" => {
                    value /= attr(3, 1.0)?.max(attr(13, attr(3, 1.0)?)?)
                }
                "Deconvolution" => value *= attr(3, 1.0)?.min(attr(13, attr(3, 1.0)?)?),
                "Interp" => {
                    ensure!(
                        attr(3, 0.0)? == 0.0 && attr(4, 0.0)? == 0.0,
                        "fixed-size resize graphs are not supported"
                    );
                    value *= attr(1, 1.0)?.min(attr(2, 1.0)?);
                }
                "PixelShuffle" => value *= attr(0, 1.0)?,
                _ => {}
            }
            ensure!(
                value.is_finite() && value > 0.0 && value >= 1.0 / MAX_PADDING as f64,
                "unsupported model spatial scale"
            );
            alignment = alignment.max(1.0 / value);
            for name in &fields[4 + bottom..node_end] {
                scales.insert(name, value);
            }
        }
        if matches!(kind, "Convolution" | "Deconvolution") {
            // Prevent zero divisors in native convolution pipeline preparation.
            for id in [0, 1] {
                let prefix = format!("{id}=");
                let value: i32 = attributes
                    .iter()
                    .find_map(|attribute| attribute.strip_prefix(&prefix))
                    .context("missing convolution dimension")?
                    .parse()?;
                ensure!(value > 0, "invalid convolution dimension");
            }
            for id in [2, 3, 11, 12, 13] {
                let prefix = format!("{id}=");
                if let Some(value) = attributes
                    .iter()
                    .find_map(|attribute| attribute.strip_prefix(&prefix))
                {
                    ensure!(
                        value.parse::<i32>()? > 0,
                        "invalid convolution dimension or stride"
                    );
                }
            }
        }
    }
    ensure!(
        layer_count == counts[0] && blobs.len() == counts[1],
        "model graph counts do not match its contents"
    );
    for name in [INPUT_BLOB_0, INPUT_BLOB_1, INPUT_BLOB_2, OUTPUT_BLOB] {
        ensure!(blobs.contains(name), "missing RIFE v4 blob {name}");
    }
    ensure!(
        input_names == HashSet::from([INPUT_BLOB_0, INPUT_BLOB_1, INPUT_BLOB_2]),
        "expected a RIFE v4 three-input flownet graph"
    );
    ensure!(
        scales
            .get(OUTPUT_BLOB)
            .is_some_and(|s| (*s - 1.0).abs() < 1e-6),
        "RIFE output must have the input spatial scale"
    );
    let padding = (alignment.ceil() as u32).max(32).next_power_of_two();
    ensure!(padding <= MAX_PADDING as u32, "unsupported model padding");
    Ok(Metadata { padding })
}

#[cfg(test)]
mod tests {
    use super::*;
    const GRAPH: &str = "7767517\n4 4\nInput a 0 1 in0\nInput b 0 1 in1\nInput c 0 1 in2\nBinaryOp mix 2 1 in0 in1 out0 0=0\n";
    #[test]
    fn dynamic_directory_names_are_preserved_and_paths_rejected() {
        for name in [
            "rife-v4.25",
            "rife-v4.25-lite",
            "rife-v4.25-heavy",
            "rife-v4.26",
            "rife-v4.26-large",
            "rife-v4.28-large",
            "CustomModel",
        ] {
            let model: Model = name.parse().unwrap();
            assert_eq!(model.to_string(), name);
            assert_eq!(model.directory(), name);
        }
        for name in [
            "", ".", "..", "../model", "a/b", "a\\b", "D:model", "model.",
        ] {
            assert!(name.parse::<Model>().is_err(), "{name}");
        }
    }
    #[test]
    fn supplied_models_determine_their_own_padding() {
        for (graph, padding) in [
            (
                include_bytes!("../tests/fixtures/rife-v4.26.param").as_slice(),
                64,
            ),
            (
                include_bytes!("../tests/fixtures/rife-v4.26-large.param").as_slice(),
                64,
            ),
            (
                include_bytes!("../tests/fixtures/rife-v4.25-lite.param").as_slice(),
                128,
            ),
        ] {
            assert_eq!(validate(graph).unwrap().padding, padding);
        }
    }

    #[test]
    fn rejects_inconsistent_counts_unresolved_blobs_and_bad_parameters() {
        validate(GRAPH.as_bytes()).unwrap();
        for bad in [
            GRAPH.replace("4 4", "3 4"),
            GRAPH.replace("4 4", "4 3"),
            GRAPH.replace("in0 in1 out0", "missing in1 out0"),
            GRAPH.replace("0=0", "0=NaN"),
            GRAPH.replace("0=0", "-23303=2,1"),
            GRAPH.replace("BinaryOp", "Unknown"),
        ] {
            assert!(validate(bad.as_bytes()).is_err());
        }
    }
}
