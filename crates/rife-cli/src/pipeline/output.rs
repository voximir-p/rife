// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Own and validate a destination until its staged output has been committed.
use crate::{cli::Args, encode, images::Sequence, output_lock::OutputLock, paths, ui::Ui};
use anyhow::{Result, bail, ensure};
use rife_core::CancellationToken;
use std::{
    fs,
    path::{Path, PathBuf},
};

enum Kind {
    Video,
    Images(Sequence),
}

pub(super) struct Destination {
    pub input: PathBuf,
    pub path: PathBuf,
    pub parent: PathBuf,
    pub extension: String,
    kind: Kind,
    _lock: OutputLock,
}

pub(super) enum StagedOutput {
    Video(PathBuf),
    Images {
        directory: PathBuf,
        pattern: PathBuf,
    },
}

impl StagedOutput {
    pub fn path(&self) -> &Path {
        match self {
            Self::Video(path) => path,
            Self::Images { pattern, .. } => pattern,
        }
    }
}

impl Destination {
    pub fn acquire(args: &Args, cancel: &CancellationToken, ui: &Ui) -> Result<Self> {
        let (input, path, parent) = paths::resolve_paths(args, cancel)?;
        let kind = Sequence::parse(&path)?.map_or(Kind::Video, Kind::Images);
        let lock_path = match kind {
            Kind::Video => path.clone(),
            Kind::Images(_) => Sequence::lock_path(&parent),
        };
        let lock = OutputLock::acquire(&lock_path, args.overwrite, cancel, || {
            ui.status("Waiting", format_args!("for locked `{}`", path.display()));
        })?;
        let extension = path
            .extension()
            .unwrap()
            .to_string_lossy()
            .to_ascii_lowercase();
        let destination = Self {
            input,
            path,
            parent,
            extension,
            kind,
            _lock: lock,
        };
        destination.revalidate(args.overwrite, cancel)?;
        Ok(destination)
    }

    fn revalidate(&self, overwrite: bool, cancel: &CancellationToken) -> Result<()> {
        match &self.kind {
            Kind::Video => paths::validate_output(&self.input, &self.path, overwrite),
            Kind::Images(sequence) => {
                sequence.preflight(&self.parent, &self.input, overwrite, cancel)
            }
        }
    }

    pub fn is_sequence(&self) -> bool {
        matches!(self.kind, Kind::Images(_))
    }

    pub fn carries_audio(&self) -> bool {
        !self.is_sequence() && encode::carries_audio(&self.extension)
    }

    pub fn validate_minimum_count(&self, frames: u64) -> Result<()> {
        if self.is_sequence() {
            Sequence::validate_count(frames)?;
        }
        Ok(())
    }

    pub fn validate_count(
        &self,
        frames: u64,
        overwrite: bool,
        cancel: &CancellationToken,
    ) -> Result<()> {
        if let Kind::Images(sequence) = &self.kind {
            sequence.validate(&self.parent, &self.input, frames, overwrite, cancel)?;
        }
        Ok(())
    }

    pub fn stage(&self, directory: &Path) -> Result<StagedOutput> {
        let directory = dunce::simplified(directory);
        match self.kind {
            Kind::Video => Ok(StagedOutput::Video(
                directory.join(format!("encoded.{}", self.extension)),
            )),
            Kind::Images(_) => {
                let directory = directory.join("frames");
                fs::create_dir(&directory)?;
                let pattern = Sequence::staged_pattern(&directory, &self.extension);
                Ok(StagedOutput::Images { directory, pattern })
            }
        }
    }

    pub fn commit(
        &self,
        staged: &StagedOutput,
        frames: u64,
        overwrite: bool,
        cancel: &CancellationToken,
    ) -> Result<()> {
        match (&self.kind, staged) {
            (Kind::Video, StagedOutput::Video(path)) => {
                ensure!(
                    path.is_file() && fs::metadata(path)?.len() > 0,
                    "FFmpeg did not produce a valid output"
                );
                paths::commit_output(path, &self.path, overwrite, cancel)
            }
            (Kind::Images(sequence), StagedOutput::Images { directory, .. }) => sequence.commit(
                directory,
                &self.path,
                &self.input,
                frames,
                overwrite,
                cancel,
            ),
            _ => bail!("staged output does not match its destination"),
        }
    }
}
