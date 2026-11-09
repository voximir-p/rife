// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Numbered image output and a bounded-memory, rollback-capable file commit.
use crate::{config, paths};
use anyhow::{Context, Result, bail, ensure};
use rife_core::CancellationToken;
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};
use tempfile::{Builder, TempDir, TempPath};

pub(crate) struct Sequence {
    prefix: OsString,
    suffix: OsString,
    padding: usize,
}

// All callers split immediately before/after ASCII printf syntax, which is a
// valid boundary in OsStr's self-synchronizing encoding on every platform.
fn segment(bytes: &[u8]) -> &OsStr {
    // SAFETY: slices originate from as_encoded_bytes in this process and only
    // split around ASCII characters, never inside an encoded non-ASCII value.
    unsafe { OsStr::from_encoded_bytes_unchecked(bytes) }
}

impl Sequence {
    pub(crate) fn parse(output: &Path) -> Result<Option<Self>> {
        let filename = output.file_name().context("output must be a file path")?;
        let bytes = filename.as_encoded_bytes();
        let mut prefix = OsString::new();
        let mut suffix = OsString::new();
        let mut padding = None;
        let mut invalid = false;
        let (mut start, mut index) = (0, 0);
        while index < bytes.len() {
            if bytes[index] != b'%' {
                index += 1;
                continue;
            }
            let part = if padding.is_some() {
                &mut suffix
            } else {
                &mut prefix
            };
            part.push(segment(&bytes[start..index]));
            index += 1;
            if bytes.get(index) == Some(&b'%') {
                part.push("%");
                index += 1;
            } else {
                let width_start = index;
                while bytes.get(index).is_some_and(u8::is_ascii_digit) {
                    index += 1;
                }
                if bytes.get(index) == Some(&b'd') {
                    ensure!(
                        padding.is_none(),
                        "image output must contain exactly one %d or %0Nd placeholder"
                    );
                    let width = &bytes[width_start..index];
                    let value = if width.is_empty() {
                        0
                    } else {
                        ensure!(
                            width[0] == b'0',
                            "image output placeholder must be %d or %0Nd"
                        );
                        let value = std::str::from_utf8(width)?.parse::<usize>()?;
                        ensure!(
                            (1..=255).contains(&value),
                            "image output padding must be between 1 and 255"
                        );
                        value
                    };
                    padding = Some(value);
                    index += 1;
                } else {
                    invalid = true;
                    part.push("%");
                    part.push(segment(&bytes[width_start..index]));
                }
            }
            start = index;
        }
        let Some(padding) = padding else {
            return Ok(None); // Ordinary filenames may contain literal percent signs.
        };
        ensure!(
            !invalid,
            "invalid image output pattern; use %d or %0Nd and %% for a literal percent sign"
        );
        suffix.push(segment(&bytes[start..]));
        Ok(Some(Self {
            prefix,
            suffix,
            padding,
        }))
    }

    fn filename(&self, number: u64) -> OsString {
        let mut name = self.prefix.clone();
        name.push(format!("{number:0width$}", width = self.padding));
        name.push(&self.suffix);
        name
    }

    pub(crate) fn validate(
        &self,
        parent: &Path,
        input: &Path,
        frames: u64,
        overwrite: bool,
        cancel: &CancellationToken,
    ) -> Result<()> {
        ensure!(
            frames <= i32::MAX as u64,
            "image sequence has too many frames for FFmpeg"
        );
        for number in 1..=frames {
            cancel.check()?;
            paths::validate_output(input, &parent.join(self.filename(number)), overwrite)?;
        }
        Ok(())
    }

    /// All sequences in a directory share one lock, including overlapping patterns.
    pub(crate) fn lock_path(parent: &Path) -> PathBuf {
        parent.join(format!(".{}-images", config::APP_NAME))
    }

    pub(crate) fn staged_pattern(directory: &Path, extension: &str) -> PathBuf {
        let bytes = directory.as_os_str().as_encoded_bytes();
        let mut escaped = OsString::new();
        let mut start = 0;
        for (index, &byte) in bytes.iter().enumerate() {
            if byte == b'%' {
                escaped.push(segment(&bytes[start..index]));
                escaped.push("%%");
                start = index + 1;
            }
        }
        escaped.push(segment(&bytes[start..]));
        PathBuf::from(escaped).join(format!("frame_%d.{extension}"))
    }

    pub(crate) fn commit(
        &self,
        staged: &Path,
        output: &Path,
        input: &Path,
        frames: u64,
        overwrite: bool,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let parent = output.parent().context("output has no parent directory")?;
        let extension = output.extension().unwrap().to_ascii_lowercase();
        self.validate(parent, input, frames, overwrite, cancel)?;
        let mut count = 0u64;
        for entry in fs::read_dir(staged)? {
            cancel.check()?;
            entry?;
            count += 1;
        }
        ensure!(
            count == frames,
            "FFmpeg did not produce the expected number of images"
        );
        let mut commit = Commit::new(self, parent)?;
        // Prepare every complete image on the destination filesystem before
        // changing any existing frame. No per-frame paths are retained in RAM.
        for number in 1..=frames {
            cancel.check()?;
            let mut name = OsString::from(format!("frame_{number}."));
            name.push(&extension);
            let source = staged.join(name);
            let metadata =
                fs::metadata(&source).context("FFmpeg did not produce an expected image")?;
            ensure!(
                metadata.is_file() && metadata.len() > 0,
                "FFmpeg did not produce a valid image"
            );
            paths::stage_output(&source, &commit.ready(number), cancel)?;
        }
        let result = (|| {
            for number in 1..=frames {
                cancel.check()?;
                commit.install(number, input, overwrite)?;
            }
            // This is the transaction boundary: cancellation before it rolls
            // back all installed images; afterwards the completed output is kept.
            cancel.check()?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                commit.finished = true;
                Ok(())
            }
            Err(error) => match commit.rollback() {
                Ok(()) => Err(error),
                Err(rollback) => Err(error.context(rollback)),
            },
        }
    }
}

struct Commit<'a> {
    sequence: &'a Sequence,
    parent: &'a Path,
    directory: Option<TempDir>,
    installed: u64,
    attempted: u64,
    finished: bool,
}

impl<'a> Commit<'a> {
    fn new(sequence: &'a Sequence, parent: &'a Path) -> Result<Self> {
        let directory = Builder::new()
            .prefix(&config::temp_prefix())
            .tempdir_in(parent)?;
        fs::create_dir(directory.path().join("ready"))?;
        fs::create_dir(directory.path().join("backup"))?;
        Ok(Self {
            sequence,
            parent,
            directory: Some(directory),
            installed: 0,
            attempted: 0,
            finished: false,
        })
    }

    fn ready(&self, number: u64) -> PathBuf {
        self.directory
            .as_ref()
            .unwrap()
            .path()
            .join("ready")
            .join(number.to_string())
    }

    fn backup(&self, number: u64) -> PathBuf {
        self.directory
            .as_ref()
            .unwrap()
            .path()
            .join("backup")
            .join(number.to_string())
    }

    fn install(&mut self, number: u64, input: &Path, overwrite: bool) -> Result<()> {
        let output = self.parent.join(self.sequence.filename(number));
        paths::validate_output(input, &output, overwrite)?;
        self.attempted = number;
        if fs::symlink_metadata(&output).is_ok() {
            fs::rename(&output, self.backup(number)).context("could not back up existing image")?;
        }
        // Never replace a file that appeared after validation, even with -y:
        // it has not been backed up and cannot be restored on a later failure.
        TempPath::try_from_path(self.ready(number))?
            .persist_noclobber(&output)
            .map_err(|error| error.error)
            .with_context(|| format!("could not save image {}", output.display()))?;
        self.installed = number;
        Ok(())
    }

    fn rollback(&mut self) -> Result<()> {
        self.finished = true;
        let mut first_error = None;
        for number in (1..=self.attempted).rev() {
            let output = self.parent.join(self.sequence.filename(number));
            if number <= self.installed
                && let Err(error) = fs::remove_file(&output)
                && error.kind() != ErrorKind::NotFound
            {
                first_error.get_or_insert(error);
                continue;
            }
            let backup = self.backup(number);
            if fs::symlink_metadata(&backup).is_ok() {
                match TempPath::try_from_path(backup) {
                    Ok(backup) => {
                        if let Err(mut error) = backup.persist_noclobber(&output) {
                            // Do not clobber an externally created file during
                            // rollback, and never delete its displaced backup.
                            error.path.disable_cleanup(true);
                            first_error.get_or_insert(error.error);
                        }
                    }
                    Err(error) => {
                        first_error.get_or_insert(error);
                    }
                }
            }
        }
        if let Some(error) = first_error {
            let saved = self.directory.take().unwrap().keep();
            bail!(
                "could not restore previous images: {error}; backups kept in {}",
                saved.display()
            );
        }
        Ok(())
    }
}

impl Drop for Commit<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.rollback();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sequence(pattern: &str) -> Sequence {
        Sequence::parse(Path::new(pattern)).unwrap().unwrap()
    }

    #[test]
    fn printf_patterns_expand_without_truncating_large_numbers() {
        for (pattern, first, large) in [
            ("frame_%d.png", "frame_1.png", "frame_12345.png"),
            ("frame_%03d.png", "frame_001.png", "frame_12345.png"),
            (
                "视%%频_%08d.tiff",
                "视%频_00000001.tiff",
                "视%频_00012345.tiff",
            ),
        ] {
            let pattern = sequence(pattern);
            assert_eq!(pattern.filename(1), first);
            assert_eq!(pattern.filename(12345), large);
        }
        for pattern in ["video.mp4", "100%.mp4", "literal_%%d.mp4"] {
            assert!(Sequence::parse(Path::new(pattern)).unwrap().is_none());
        }
        for pattern in [
            "%d_%d.png",
            "%08d_%s.png",
            "%8d.png",
            "%00d.png",
            "%0999999999d.png",
        ] {
            assert!(Sequence::parse(Path::new(pattern)).is_err(), "{pattern}");
        }
        assert_eq!(
            Sequence::staged_pattern(Path::new("a%b"), "png"),
            Path::new("a%%b/frame_%d.png")
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_pattern_names_are_preserved() {
        use std::os::unix::ffi::OsStrExt;
        let pattern = OsStr::from_bytes(b"\xff_%02d.png");
        let parsed = Sequence::parse(Path::new(pattern)).unwrap().unwrap();
        assert_eq!(parsed.filename(1).as_bytes(), b"\xff_01.png");
    }

    fn staged_frames(directory: &Path, frames: u64) {
        for number in 1..=frames {
            fs::write(
                directory.join(format!("frame_{number}.png")),
                format!("new {number}"),
            )
            .unwrap();
        }
    }

    #[test]
    fn sequences_publish_only_complete_outputs_and_preserve_unrelated_files() {
        let root = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();
        let input = root.path().join("source.mp4");
        let output = root.path().join("frame_%03d.png");
        let pattern = sequence("frame_%03d.png");
        fs::write(&input, "source").unwrap();
        fs::write(root.path().join("frame_099.png"), "unrelated").unwrap();
        for overwrite in [false, true] {
            staged_frames(staging.path(), 2);
            pattern
                .commit(
                    staging.path(),
                    &output,
                    &input,
                    2,
                    overwrite,
                    &CancellationToken::default(),
                )
                .unwrap();
            for number in 1..=2 {
                assert_eq!(
                    fs::read_to_string(root.path().join(pattern.filename(number))).unwrap(),
                    format!("new {number}")
                );
            }
            assert_eq!(
                fs::read_to_string(root.path().join("frame_099.png")).unwrap(),
                "unrelated"
            );
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 4);
        }
        staged_frames(staging.path(), 2);
        assert!(
            pattern
                .commit(
                    staging.path(),
                    &output,
                    &input,
                    2,
                    false,
                    &CancellationToken::default()
                )
                .is_err()
        );
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 4);
    }

    #[test]
    fn invalid_or_cancelled_sequences_never_change_existing_outputs() {
        let root = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();
        let input = root.path().join("source.mp4");
        let output = root.path().join("frame_%d.png");
        let pattern = sequence("frame_%d.png");
        fs::write(&input, "source").unwrap();
        fs::write(root.path().join("frame_1.png"), "old").unwrap();
        staged_frames(staging.path(), 1);
        assert!(
            pattern
                .commit(
                    staging.path(),
                    &output,
                    &input,
                    2,
                    true,
                    &CancellationToken::default()
                )
                .is_err()
        );
        fs::write(staging.path().join("frame_2.png"), []).unwrap();
        assert!(
            pattern
                .commit(
                    staging.path(),
                    &output,
                    &input,
                    2,
                    true,
                    &CancellationToken::default()
                )
                .is_err()
        );
        staged_frames(staging.path(), 2);
        let cancel = CancellationToken::default();
        cancel.cancel();
        assert!(
            pattern
                .commit(staging.path(), &output, &input, 2, true, &cancel)
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(root.path().join("frame_1.png")).unwrap(),
            "old"
        );
        assert!(!root.path().join("frame_2.png").exists());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
        assert!(
            pattern
                .validate(
                    root.path(),
                    &root.path().join("frame_1.png"),
                    2,
                    true,
                    &CancellationToken::default()
                )
                .is_err()
        );
    }

    #[test]
    fn partial_commit_rolls_back_new_and_replaced_images() {
        let root = tempfile::tempdir().unwrap();
        let pattern = sequence("frame_%d.png");
        let input = root.path().join("source.mp4");
        for replace in [false, true] {
            if replace {
                fs::write(root.path().join("frame_1.png"), "old").unwrap();
            }
            {
                let mut commit = Commit::new(&pattern, root.path()).unwrap();
                fs::write(commit.ready(1), "new").unwrap();
                commit.install(1, &input, replace).unwrap();
                assert_eq!(
                    fs::read_to_string(root.path().join("frame_1.png")).unwrap(),
                    "new"
                );
                assert!(commit.install(2, &input, replace).is_err());
                // Dropping a failed/interrupted transaction restores every
                // prior frame and removes all newly installed frames.
            }
            if replace {
                assert_eq!(
                    fs::read_to_string(root.path().join("frame_1.png")).unwrap(),
                    "old"
                );
            } else {
                assert!(!root.path().join("frame_1.png").exists());
            }
            assert!(!root.path().join("frame_2.png").exists());
            assert_eq!(
                fs::read_dir(root.path()).unwrap().count(),
                usize::from(replace)
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn rollback_restores_relative_symlinks() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let pattern = sequence("frame_%d.png");
        let target = root.path().join("original.png");
        fs::write(&target, "old").unwrap();
        symlink("original.png", root.path().join("frame_1.png")).unwrap();
        {
            let mut commit = Commit::new(&pattern, root.path()).unwrap();
            fs::write(commit.ready(1), "new").unwrap();
            commit
                .install(1, &root.path().join("input.mp4"), true)
                .unwrap();
        }
        assert_eq!(
            fs::read_link(root.path().join("frame_1.png")).unwrap(),
            Path::new("original.png")
        );
        assert_eq!(fs::read_to_string(target).unwrap(), "old");
    }

    #[test]
    fn failed_rollback_keeps_the_previous_image_in_its_backup() {
        let root = tempfile::tempdir().unwrap();
        let pattern = sequence("frame_%d.png");
        let output = root.path().join("frame_1.png");
        fs::write(&output, "old").unwrap();
        let mut commit = Commit::new(&pattern, root.path()).unwrap();
        let backup = commit.backup(1);
        fs::write(commit.ready(1), "new").unwrap();
        commit
            .install(1, &root.path().join("input.mp4"), true)
            .unwrap();
        fs::remove_file(&output).unwrap();
        fs::create_dir(&output).unwrap(); // Simulate an external filesystem change.
        assert!(
            commit
                .rollback()
                .unwrap_err()
                .to_string()
                .contains("backups kept in")
        );
        drop(commit);
        assert_eq!(fs::read_to_string(backup).unwrap(), "old");
        assert!(output.is_dir());
    }
}
