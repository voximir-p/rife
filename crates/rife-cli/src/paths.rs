// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

use crate::{Args, Model, config, images::Sequence};
use anyhow::{Context, Result, bail, ensure};
use rife_core::CancellationToken;
use std::{
    env,
    ffi::OsStr,
    fs,
    io::{ErrorKind, Read, Write},
    path::{Path, PathBuf},
};
use tempfile::{Builder, TempDir, TempPath};

pub(crate) fn staging_directory(output_parent: &Path) -> Result<TempDir> {
    staging_directory_in(&env::temp_dir(), output_parent)
}

fn staging_directory_in(preferred: &Path, output_parent: &Path) -> Result<TempDir> {
    let prefix = config::temp_prefix();
    Builder::new()
        .prefix(&prefix)
        .tempdir_in(preferred)
        .or_else(|_| Builder::new().prefix(&prefix).tempdir_in(output_parent))
        .context("unable to create temporary output staging directory")
}

pub(crate) fn tool_path(root: &Path, tool: &str) -> Result<PathBuf> {
    let filename = format!("{tool}{}", env::consts::EXE_SUFFIX);
    let path = root.join(config::FFMPEG_DIRECTORY).join(filename);
    ensure!(
        path.is_file(),
        "missing FFmpeg tool: expected {}",
        path.display()
    );
    Ok(path)
}

pub(crate) fn find_model(root: &Path, model: &Model) -> Result<PathBuf> {
    let dir = model.directory();
    let path = root.join(config::MODELS_DIRECTORY).join(dir);
    if path.join(Model::PARAM_FILE).is_file() && path.join(Model::WEIGHTS_FILE).is_file() {
        for filename in [Model::PARAM_FILE, Model::WEIGHTS_FILE] {
            let file = fs::File::open(path.join(filename))
                .with_context(|| format!("could not read model file {filename}"))?;
            ensure!(file.metadata()?.len() > 0, "model file {filename} is empty");
        }
        return Ok(path);
    }

    bail!(
        "NCNN model `{dir}` is missing {} or {} in {}. It must contain a compatible RIFE v4 flownet graph.",
        Model::PARAM_FILE,
        Model::WEIGHTS_FILE,
        path.display()
    )
}

pub(crate) fn resolve_paths(
    args: &Args,
    cancel: &CancellationToken,
) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let input = dunce::canonicalize(&args.input)
        .with_context(|| format!("input does not exist: {}", args.input.display()))?;
    ensure!(input.is_file(), "input must be a file");

    let filename = args
        .output
        .file_name()
        .filter(|name| *name != OsStr::new(".") && *name != OsStr::new(".."))
        .context("output must be a file path")?;
    let output_parent = args.output.parent().filter(|p| !p.as_os_str().is_empty());
    let parent = dunce::canonicalize(output_parent.unwrap_or(Path::new(".")))
        .context("output directory does not exist")?;
    let output = parent.join(filename);

    ensure!(
        output.extension().is_some(),
        "output must have a file extension"
    );
    if let Some(sequence) = Sequence::parse(&output)? {
        sequence.preflight(&parent, &input, args.overwrite, cancel)?;
    } else {
        validate_output(&input, &output, args.overwrite)?;
    }
    Ok((input, output, parent))
}

pub(crate) fn validate_output(input: &Path, output: &Path, overwrite: bool) -> Result<()> {
    ensure!(output != input, "input and output must be different files");
    if match fs::symlink_metadata(output) {
        Ok(_) => true,
        Err(error) if error.kind() == ErrorKind::NotFound => false,
        Err(error) => return Err(error).context("could not inspect output path"),
    } {
        ensure!(
            dunce::canonicalize(output)? != input,
            "refusing to overwrite the original input"
        );
        ensure!(
            overwrite,
            "output already exists (pass --overwrite to replace it)"
        );
        ensure!(output.is_file(), "output path is not a file");
    }
    ensure!(
        output.extension().is_some(),
        "output must have a file extension"
    );
    Ok(())
}

pub(crate) fn commit_output(
    staged: &Path,
    output: &Path,
    overwrite: bool,
    cancel: &CancellationToken,
) -> Result<()> {
    cancel.check()?;
    ensure!(
        !output.exists() || overwrite,
        "output was created during processing; refusing to overwrite it"
    );
    let parent = output.parent().context("output has no parent directory")?;
    // Prepare the complete file on the destination filesystem before changing
    // an existing output. Cross-drive copies must never expose partial video.
    let ready = output_tempfile(parent)?;
    stage_output(staged, &ready, cancel)?;
    cancel.check()?;
    if !output.exists() {
        fs::rename(&ready, output).context("could not move finished video into place")?;
        return Ok(());
    }
    ensure!(
        overwrite,
        "output was created during processing; refusing to overwrite it"
    );

    // Backup lives on the same filesystem. A failed final move can be rolled back.
    let mut backup = output_tempfile(parent)?;
    fs::rename(output, &backup).context("could not back up existing output")?;
    match fs::rename(&ready, output) {
        Ok(()) => Ok(()),
        Err(err) => {
            if let Err(restore) = fs::rename(&backup, output) {
                // Preserve the user's previous output if rollback fails: don't delete its backup.
                backup.disable_cleanup(true);
                let saved = backup.to_path_buf();
                bail!(
                    "failed to commit new output: {err}; also failed to restore the previous output: {restore}. backup kept in {}",
                    saved.display()
                );
            }
            Err(err).context("could not move finished video into place; old output was restored")
        }
    }
}

pub(crate) fn stage_output(staged: &Path, ready: &Path, cancel: &CancellationToken) -> Result<()> {
    match fs::rename(staged, ready) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::CrossesDevices => {
            copy_output(staged, ready, cancel)
                .context("could not copy finished output to output directory")?;
        }
        Err(error) => return Err(error).context("could not stage finished output"),
    }
    Ok(())
}

fn copy_output(source: &Path, destination: &Path, cancel: &CancellationToken) -> Result<()> {
    let mut source = fs::File::open(source)?;
    let mut destination = fs::File::create(destination)?;
    let mut buffer = [0; 64 * 1024];
    loop {
        cancel.check()?;
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        destination.write_all(&buffer[..count])?;
    }
    destination.set_permissions(source.metadata()?.permissions())?;
    Ok(())
}

fn output_tempfile(parent: &Path) -> Result<TempPath> {
    Ok(Builder::new()
        .prefix(&config::temp_prefix())
        .tempfile_in(parent)
        .context("could not create temporary file in output directory")?
        .into_temp_path())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    fn temp() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    fn commit_output(staged: &Path, output: &Path, overwrite: bool) -> Result<()> {
        super::commit_output(staged, output, overwrite, &CancellationToken::default())
    }

    #[test]
    fn cancelled_commit_preserves_existing_output() {
        let root = temp();
        let output = root.path().join("output.mp4");
        let staged = root.path().join("encoded.mp4");
        fs::write(&output, "old").unwrap();
        fs::write(&staged, "new").unwrap();
        let cancel = CancellationToken::default();
        cancel.cancel();
        assert!(super::commit_output(&staged, &output, true, &cancel).is_err());
        assert_eq!(fs::read_to_string(output).unwrap(), "old");
        assert_eq!(fs::read_to_string(staged).unwrap(), "new");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
    }

    #[test]
    fn staging_uses_preferred_directory_and_falls_back_when_unavailable() {
        let output_parent = temp();
        let preferred = temp();
        let staging = staging_directory_in(preferred.path(), output_parent.path()).unwrap();
        assert_eq!(staging.path().parent(), Some(preferred.path()));
        for unavailable in [preferred.path().join("missing"), {
            let file = preferred.path().join("file");
            fs::write(&file, "not a directory").unwrap();
            file
        }] {
            let fallback = staging_directory_in(&unavailable, output_parent.path()).unwrap();
            assert_eq!(fallback.path().parent(), Some(output_parent.path()));
        }
    }

    #[test]
    fn staged_outputs_commit_and_preserve_existing_files_without_overwrite() {
        let root = temp();
        let output = root.path().join("output.mp4");
        for overwrite in [false, true] {
            let staging = staging_directory(root.path()).unwrap();
            let staging_path = staging.path().to_path_buf();
            let staged = staging.path().join("encoded.mp4");
            let expected = vec![if overwrite { 42 } else { 21 }; 200_000];
            fs::write(&staged, &expected).unwrap();
            if overwrite {
                let previous = fs::read(&output).unwrap();
                assert!(commit_output(&staged, &output, false).is_err());
                assert_eq!(fs::read(&output).unwrap(), previous);
            }
            commit_output(&staged, &output, overwrite).unwrap();
            drop(staging);
            assert!(!staging_path.exists());
            assert_eq!(fs::read(&output).unwrap(), expected);
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn preparation_failure_leaves_old_output_and_no_temporary_files() {
        let root = temp();
        let output = root.path().join("output.mp4");
        fs::write(&output, "old").unwrap();
        assert!(commit_output(&root.path().join("missing.mp4"), &output, true).is_err());
        assert_eq!(fs::read_to_string(output).unwrap(), "old");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }
    #[test]
    fn finds_only_ffmpeg_tools_and_models_in_the_new_layout() {
        let runtime = temp();
        fs::create_dir(runtime.path().join(config::FFMPEG_DIRECTORY)).unwrap();
        for tool in ["ffmpeg", "ffprobe"] {
            let expected = runtime
                .path()
                .join("ffmpeg")
                .join(format!("{tool}{}", env::consts::EXE_SUFFIX));
            assert!(tool_path(runtime.path(), tool).is_err());
            fs::write(&expected, []).unwrap();
            assert_eq!(tool_path(runtime.path(), tool).unwrap(), expected);
        }
        for name in ["CustomModel", "rife-v9.99-custom"] {
            let model: Model = name.parse().unwrap();
            let directory = runtime
                .path()
                .join(config::MODELS_DIRECTORY)
                .join(model.directory());
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join(Model::PARAM_FILE), "parameters").unwrap();
            assert!(find_model(runtime.path(), &model).is_err());
            fs::write(directory.join(Model::WEIGHTS_FILE), []).unwrap();
            assert!(find_model(runtime.path(), &model).is_err());
            fs::write(directory.join(Model::WEIGHTS_FILE), [1]).unwrap();
            assert_eq!(find_model(runtime.path(), &model).unwrap(), directory);
        }
    }
    #[test]
    fn input_is_protected_and_replacement_is_explicit() {
        let temp = temp();
        let input = temp.path().join("source.mp4");
        let output = temp.path().join("output.mp4");
        fs::write(&input, "original").unwrap();
        fs::write(&output, "previous").unwrap();
        let mut args = Args::try_parse_from([
            std::ffi::OsStr::new(config::APP_NAME),
            std::ffi::OsStr::new("-i"),
            input.as_os_str(),
            input.as_os_str(),
        ])
        .unwrap();
        args.overwrite = true;
        assert!(resolve_paths(&args, &CancellationToken::default()).is_err());
        args.output = output;
        args.overwrite = false;
        assert!(resolve_paths(&args, &CancellationToken::default()).is_err());
        args.overwrite = true;
        assert!(resolve_paths(&args, &CancellationToken::default()).is_ok());
        assert_eq!(fs::read_to_string(input).unwrap(), "original");
    }
}
