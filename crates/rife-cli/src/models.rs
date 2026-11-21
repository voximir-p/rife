// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Resolve custom models or unpack one bundled model into parent-owned staging.

use crate::{config, paths};
use anyhow::{Context, Result};
use rife_core::{CancellationToken, Model};
use std::{
    fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

include!(concat!(env!("OUT_DIR"), "/embedded_models.rs"));

pub(crate) enum Source {
    External(PathBuf),
    Embedded(String),
}

impl Source {
    pub(crate) fn resolve(root: &Path, model: &Model) -> Result<Self> {
        let name = model.directory();
        let external = root.join(config::MODELS_DIRECTORY).join(name);
        // An explicit external model still overrides a bundled version. Broken
        // or incomplete overrides must report their error rather than fall back.
        match external.symlink_metadata() {
            Ok(_) => return paths::find_model(root, model).map(Self::External),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("could not inspect external model directory"),
        }
        if Assets::get(&format!("{name}/{}", Model::PARAM_FILE)).is_none()
            || Assets::get(&format!("{name}/{}", Model::WEIGHTS_FILE)).is_none()
        {
            return paths::find_model(root, model).map(Self::External);
        }
        Ok(Self::Embedded(name.into()))
    }

    pub(crate) fn materialize(
        &self,
        staging: &Path,
        cancel: &CancellationToken,
    ) -> Result<PathBuf> {
        cancel.check()?;
        let name = match self {
            Self::External(path) => return Ok(path.clone()),
            Self::Embedded(name) => name,
        };
        let directory = staging.join("model");
        fs::create_dir(&directory).context("could not create temporary model directory")?;
        for filename in [Model::PARAM_FILE, Model::WEIGHTS_FILE, "LICENSE"] {
            let key = if filename == "LICENSE" {
                filename.to_owned()
            } else {
                format!("{name}/{filename}")
            };
            let asset = Assets::get(&key).context("missing embedded model asset")?;
            let mut file = fs::File::create(directory.join(filename))
                .with_context(|| format!("could not create temporary model file {filename}"))?;
            for chunk in asset.data.chunks(64 * 1024) {
                cancel.check()?;
                file.write_all(chunk)
                    .with_context(|| format!("could not extract embedded model file {filename}"))?;
            }
        }
        cancel.check()?;
        Ok(directory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_only_the_selected_model_and_cleans_it_with_staging() {
        let root = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();
        let model: Model = env!("RIFE_DEFAULT_MODEL").parse().unwrap();
        let source = Source::resolve(root.path(), &model).unwrap();
        let directory = source
            .materialize(staging.path(), &CancellationToken::default())
            .unwrap();
        for filename in [Model::PARAM_FILE, Model::WEIGHTS_FILE, "LICENSE"] {
            let key = if filename == "LICENSE" {
                filename.into()
            } else {
                format!("{model}/{filename}")
            };
            assert_eq!(
                fs::read(directory.join(filename)).unwrap(),
                Assets::get(&key).unwrap().data.as_ref()
            );
        }
        assert_eq!(fs::read_dir(staging.path()).unwrap().count(), 1);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 3);
        drop(staging);
        assert!(!directory.exists());
    }

    #[test]
    fn selection_has_complete_assets_and_no_unselected_models() {
        let names: Vec<_> = env!("RIFE_EMBEDDED_MODELS").split(", ").collect();
        assert_eq!(names[0], env!("RIFE_DEFAULT_MODEL"));
        assert_eq!(Assets::iter().count(), names.len() * 2 + 1);
        for name in names {
            for filename in [Model::PARAM_FILE, Model::WEIGHTS_FILE] {
                assert!(
                    !Assets::get(&format!("{name}/{filename}"))
                        .unwrap()
                        .data
                        .is_empty()
                );
            }
        }
    }

    #[test]
    fn external_overrides_are_preserved_and_invalid_ones_are_not_hidden() {
        let root = tempfile::tempdir().unwrap();
        let model: Model = env!("RIFE_DEFAULT_MODEL").parse().unwrap();
        let directory = root
            .path()
            .join(config::MODELS_DIRECTORY)
            .join(model.directory());
        fs::create_dir_all(&directory).unwrap();
        assert!(Source::resolve(root.path(), &model).is_err());
        for filename in [Model::PARAM_FILE, Model::WEIGHTS_FILE] {
            fs::write(directory.join(filename), b"external model").unwrap();
        }
        let source = Source::resolve(root.path(), &model).unwrap();
        let staging = tempfile::tempdir().unwrap();
        assert_eq!(
            source
                .materialize(staging.path(), &CancellationToken::default())
                .unwrap(),
            directory
        );
        assert_eq!(fs::read_dir(staging.path()).unwrap().count(), 0);
        assert!(Source::resolve(root.path(), &"missing-custom-model".parse().unwrap()).is_err());
    }

    #[test]
    fn cancelled_extraction_creates_no_files() {
        let root = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();
        let model = env!("RIFE_DEFAULT_MODEL").parse().unwrap();
        let source = Source::resolve(root.path(), &model).unwrap();
        let cancel = CancellationToken::default();
        cancel.cancel();
        assert!(source.materialize(staging.path(), &cancel).is_err());
        assert_eq!(fs::read_dir(staging.path()).unwrap().count(), 0);
    }
}
