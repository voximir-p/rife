// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! File destinations own locks and transactional output; stdout owns neither.
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

pub(super) enum Destination {
    File(FileDestination),
    Stdout,
}

pub(super) struct FileDestination {
    path: PathBuf,
    parent: PathBuf,
    extension: String,
    kind: Kind,
    _lock: OutputLock,
}

pub(super) enum StagedOutput {
    Video(PathBuf),
    Images {
        directory: PathBuf,
        pattern: PathBuf,
    },
    Stdout,
}

impl StagedOutput {
    pub fn path(&self) -> &Path {
        match self {
            Self::Video(path) => path,
            Self::Images { pattern, .. } => pattern,
            Self::Stdout => Path::new("pipe:1"),
        }
    }
}

impl Destination {
    pub fn acquire(args: &Args, input: &Path, cancel: &CancellationToken, ui: &Ui) -> Result<Self> {
        if args.streams_output() {
            return Ok(Self::Stdout);
        }
        let (path, parent) = paths::resolve_output(&args.output, input, args.overwrite, cancel)?;
        let kind = Sequence::parse(&path)?.map_or(Kind::Video, Kind::Images);
        let lock_path = match kind {
            Kind::Video => path.clone(),
            Kind::Images(_) => Sequence::lock_path(&parent),
        };
        let lock = OutputLock::acquire(&lock_path, args.overwrite, cancel, || {
            ui.status("Waiting", format_args!("for locked `{}`", path.display()));
        })?;
        // A previous writer may have committed while this process waited.
        match &kind {
            Kind::Video => paths::validate_output(input, &path, args.overwrite)?,
            Kind::Images(sequence) => sequence.preflight(&parent, input, args.overwrite, cancel)?,
        }
        let extension = path
            .extension()
            .unwrap()
            .to_string_lossy()
            .to_ascii_lowercase();
        Ok(Self::File(FileDestination {
            path,
            parent,
            extension,
            kind,
            _lock: lock,
        }))
    }

    pub fn parent(&self) -> Option<&Path> {
        match self {
            Self::File(file) => Some(&file.parent),
            Self::Stdout => None,
        }
    }

    pub fn format<'a>(&'a self, args: &'a Args) -> &'a str {
        args.output_format().unwrap_or_else(|| match self {
            Self::File(file) => &file.extension,
            Self::Stdout => "nut",
        })
    }

    pub fn encoder_output<'a>(&'a self, args: &'a Args) -> encode::Output<'a> {
        encode::Output {
            format: self.format(args),
            stdout: self.is_stdout(),
        }
    }

    pub fn is_stdout(&self) -> bool {
        matches!(self, Self::Stdout)
    }
    pub fn is_sequence(&self) -> bool {
        matches!(
            self,
            Self::File(FileDestination {
                kind: Kind::Images(_),
                ..
            })
        )
    }
    pub fn carries_audio(&self, args: &Args) -> bool {
        !self.is_sequence() && encode::carries_audio(self.format(args))
    }

    pub fn validate_minimum_count(&self, frames: u64) -> Result<()> {
        if self.is_sequence() {
            Sequence::validate_count(frames)?;
        }
        Ok(())
    }

    pub fn validate_count(
        &self,
        input: &Path,
        frames: u64,
        overwrite: bool,
        cancel: &CancellationToken,
    ) -> Result<()> {
        if let Self::File(FileDestination {
            kind: Kind::Images(sequence),
            parent,
            ..
        }) = self
        {
            sequence.validate(parent, input, frames, overwrite, cancel)?;
        }
        Ok(())
    }

    pub fn stage(&self, directory: &Path) -> Result<StagedOutput> {
        let directory = dunce::simplified(directory);
        match self {
            Self::Stdout => Ok(StagedOutput::Stdout),
            Self::File(file) => match file.kind {
                Kind::Video => Ok(StagedOutput::Video(
                    directory.join(format!("encoded.{}", file.extension)),
                )),
                Kind::Images(_) => {
                    let directory = directory.join("frames");
                    fs::create_dir(&directory)?;
                    let pattern = Sequence::staged_pattern(&directory, &file.extension);
                    Ok(StagedOutput::Images { directory, pattern })
                }
            },
        }
    }

    pub fn commit(
        &self,
        staged: &StagedOutput,
        input: &Path,
        frames: u64,
        overwrite: bool,
        cancel: &CancellationToken,
    ) -> Result<()> {
        match (self, staged) {
            (Self::Stdout, StagedOutput::Stdout) => Ok(()),
            (Self::File(file), StagedOutput::Video(path)) if matches!(file.kind, Kind::Video) => {
                ensure!(
                    path.is_file() && fs::metadata(path)?.len() > 0,
                    "FFmpeg did not produce a valid output"
                );
                paths::commit_output(path, &file.path, overwrite, cancel)
            }
            (
                Self::File(FileDestination {
                    kind: Kind::Images(sequence),
                    path,
                    ..
                }),
                StagedOutput::Images { directory, .. },
            ) => sequence.commit(directory, path, input, frames, overwrite, cancel),
            _ => bail!("staged output does not match its destination"),
        }
    }
}
