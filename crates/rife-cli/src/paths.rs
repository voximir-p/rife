// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

use crate::{Args, Model, config};
use anyhow::{Context, Result, bail, ensure};
use std::{
    env,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};
use tempfile::TempDir;
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
        return Ok(path);
    }

    bail!(
        "NCNN model `{dir}` is missing {} or {} in {}. It must contain a compatible RIFE v4 flownet graph.",
        Model::PARAM_FILE,
        Model::WEIGHTS_FILE,
        path.display()
    )
}

pub(crate) fn resolve_paths(args: &Args) -> Result<(PathBuf, PathBuf, PathBuf)> {
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

    validate_output(&input, &output, args.overwrite)?;
    Ok((input, output, parent))
}

pub(crate) fn validate_output(input: &Path, output: &Path, overwrite: bool) -> Result<()> {
    ensure!(output != input, "input and output must be different files");
    if output.exists() {
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
        "output must have a video file extension"
    );
    Ok(())
}

pub(crate) fn commit_output(
    staged: &Path,
    output: &Path,
    temp: &mut Option<TempDir>,
    overwrite: bool,
) -> Result<()> {
    if !output.exists() {
        fs::rename(staged, output).context("could not move finished video into place")?;
        return Ok(());
    }
    ensure!(
        overwrite,
        "output was created during processing; refusing to overwrite it"
    );

    // Backup lives on the same filesystem. A failed final move can be rolled back.
    let backup = temp
        .as_ref()
        .context("temporary directory was removed prematurely")?
        .path()
        .join("previous_output.backup");
    fs::rename(output, &backup).context("could not back up existing output")?;
    match fs::rename(staged, output) {
        Ok(()) => Ok(()),
        Err(err) => {
            if let Err(restore) = fs::rename(&backup, output) {
                // Preserve the user's previous output if rollback fails: don't delete its backup.
                let saved = temp.take().expect("temp directory exists").keep();
                bail!(
                    "failed to commit new output: {err}; also failed to restore the previous output: {restore}. backup kept in {}",
                    saved.display()
                );
            }
            Err(err).context("could not move finished video into place; old output was restored")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    fn temp() -> TempDir {
        tempfile::tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target")).unwrap()
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
        for name in [
            "rife-v4.25",
            "rife-v4.25-lite",
            "rife-v4.25-heavy",
            "rife-v4.26",
            "rife-v4.26-large",
            "rife-v4.28-large",
        ] {
            let model: Model = name.parse().unwrap();
            let directory = runtime
                .path()
                .join(config::MODELS_DIRECTORY)
                .join(model.directory());
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join(Model::PARAM_FILE), []).unwrap();
            assert!(find_model(runtime.path(), &model).is_err());
            fs::write(directory.join(Model::WEIGHTS_FILE), []).unwrap();
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
        assert!(resolve_paths(&args).is_err());
        args.output = output;
        args.overwrite = false;
        assert!(resolve_paths(&args).is_err());
        args.overwrite = true;
        assert!(resolve_paths(&args).is_ok());
        assert_eq!(fs::read_to_string(input).unwrap(), "original");
    }
    #[test]
    fn staged_output_replaces_existing_output_and_is_cleaned_up() {
        let root = temp();
        let output = root.path().join("output.mp4");
        fs::write(&output, "old").unwrap();
        let staging = tempfile::tempdir_in(root.path()).unwrap();
        let staged = staging.path().join("encoded.mp4");
        fs::write(&staged, "new").unwrap();
        let mut staging = Some(staging);
        assert!(commit_output(&staged, &output, &mut staging, false).is_err());
        commit_output(&staged, &output, &mut staging, true).unwrap();
        drop(staging);
        assert_eq!(fs::read_to_string(output).unwrap(), "new");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
