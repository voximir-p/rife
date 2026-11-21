// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.parent().unwrap().parent().unwrap();
    let selection = root.join("third_party/models.txt");
    let lock = root.join("third_party/dependencies.lock");
    let fetch = root.join("scripts/fetch-models.sh");
    for path in [&selection, &lock, &fetch] {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    let text = fs::read_to_string(&selection).expect("could not read third_party/models.txt");
    let models: Vec<_> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    assert!(
        !models.is_empty(),
        "select at least one model in third_party/models.txt"
    );
    for (index, name) in models.iter().enumerate() {
        assert!(
            name.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
                && name.as_bytes()[0] != b'.'
                && !name.ends_with('.')
                && !models[..index].contains(name),
            "invalid or duplicate model name in third_party/models.txt: {name}"
        );
    }
    let source = root.join("third_party/rife-ncnn-vulkan-ex");
    let files: Vec<_> = std::iter::once(PathBuf::from("LICENSE"))
        .chain(models.iter().flat_map(|name| {
            ["flownet.param", "flownet.bin"]
                .map(|file| PathBuf::from("models").join(name).join(file))
        }))
        .collect();
    // Git checkouts are verified even when the selected files already exist.
    // Corresponding Source archives contain the assets without Git metadata and
    // can build offline without invoking the fetcher or requiring Git/Python.
    if source.join(".git").exists() || files.iter().any(|file| !source.join(file).is_file()) {
        let status = Command::new("sh")
            .arg(&fetch)
            .status()
            .expect("could not run model fetcher; install Git, Python and sh, then run sh scripts/fetch-models.sh");
        assert!(
            status.success(),
            "model fetch failed; run sh scripts/fetch-models.sh"
        );
    }
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let assets = out_dir.join("models");
    if assets.exists() {
        fs::remove_dir_all(&assets).expect("could not refresh embedded model assets");
    }
    for file in files {
        let input = source.join(&file);
        println!("cargo:rerun-if-changed={}", input.display());
        assert!(
            fs::metadata(&input).is_ok_and(|metadata| metadata.len() > 0),
            "missing or empty model asset: {}",
            input.display()
        );
        let output = assets.join(file.strip_prefix("models").unwrap_or(&file));
        fs::create_dir_all(output.parent().unwrap())
            .expect("could not create embedded asset directory");
        fs::copy(&input, &output).expect("could not prepare embedded model asset");
    }
    // A generated literal avoids runtime path expansion and works for Windows
    // paths (including backslashes and spaces) without extra dependencies.
    fs::write(
        out_dir.join("embedded_models.rs"),
        format!(
            "#[derive(rust_embed::RustEmbed)]\n#[folder = {:?}]\nstruct Assets;\n",
            assets.to_str().expect("model build path must be UTF-8")
        ),
    )
    .expect("could not generate embedded model declaration");
    println!("cargo:rustc-env=RIFE_EMBEDDED_MODELS={}", models.join(", "));
    println!("cargo:rustc-env=RIFE_DEFAULT_MODEL={}", models[0]);
}
