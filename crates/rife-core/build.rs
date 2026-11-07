// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn generate_rust_settings(source_path: &Path, out_dir: &Path) {
    const SETTINGS: [(&str, &str, bool); 8] = [
        ("RIFE_RGB_CHANNELS", "RGB_CHANNELS", false),
        ("RIFE_MAX_PADDING", "MAX_PADDING", false),
        ("RIFE_NATIVE_ERROR_CAPACITY", "NATIVE_ERROR_CAPACITY", false),
        ("RIFE_DEVICE_NAME_CAPACITY", "DEVICE_NAME_CAPACITY", false),
        ("RIFE_INPUT_BLOB_0", "INPUT_BLOB_0", true),
        ("RIFE_INPUT_BLOB_1", "INPUT_BLOB_1", true),
        ("RIFE_INPUT_BLOB_2", "INPUT_BLOB_2", true),
        ("RIFE_OUTPUT_BLOB", "OUTPUT_BLOB", true),
    ];
    let source = fs::read_to_string(source_path).expect("Could not read shared native settings");
    let mut generated = String::from("// Generated from native/settings.h by build.rs.\n");
    for (native_name, rust_name, is_string) in SETTINGS {
        let prefix = format!("#define {native_name} ");
        let value = source
            .lines()
            .find_map(|line| line.strip_prefix(&prefix))
            .unwrap_or_else(|| panic!("Missing {native_name} in native/settings.h"));
        if is_string {
            let value = value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .filter(|value| value.chars().all(|c| c.is_ascii_alphanumeric() || c == '.'))
                .unwrap_or_else(|| panic!("Invalid string setting {native_name}"));
            generated.push_str(&format!("pub const {rust_name}: &str = {value:?};\n"));
        } else {
            let value = value
                .parse::<usize>()
                .unwrap_or_else(|_| panic!("Invalid integer setting {native_name}"));
            generated.push_str(&format!("pub const {rust_name}: usize = {value};\n"));
        }
    }
    fs::write(out_dir.join("shared_settings.rs"), generated)
        .expect("Could not generate shared Rust settings");
}

fn main() {
    println!("cargo:rerun-if-changed=native");
    const NCNN_ENV: &str = "RIFE_NCNN_SOURCE";
    const HEADERS_ENV: &str = "RIFE_VULKAN_HEADERS";
    for variable in [NCNN_ENV, HEADERS_ENV] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    generate_rust_settings(&manifest.join("native/settings.h"), &out_dir);
    let root = manifest.parent().unwrap().parent().unwrap();
    let dependency_lock = root.join("third_party/dependencies.lock");
    println!("cargo:rerun-if-changed={}", dependency_lock.display());
    let dependencies =
        fs::read_to_string(dependency_lock).expect("Missing native dependency manifest");
    let dependency = |name: &str| {
        dependencies
            .lines()
            .skip(1)
            .find_map(|line| {
                let fields: Vec<_> = line.split('|').collect();
                assert_eq!(fields.len(), 5, "Malformed native dependency manifest");
                (fields[0] == name).then_some(fields)
            })
            .unwrap_or_else(|| panic!("Missing native dependency: {name}"))
    };
    let ncnn_dependency = dependency("ncnn");
    let headers_dependency = dependency("Vulkan-Headers");
    let ncnn = env::var_os(NCNN_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("third_party").join(ncnn_dependency[0]));
    let headers = env::var_os(HEADERS_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("third_party").join(headers_dependency[0]));
    assert!(
        ncnn.join("glslang/CMakeLists.txt").is_file(),
        "Missing NCNN sources: run sh scripts/fetch-dependencies.sh"
    );
    let mut config = cmake::Config::new("native");
    config
        .profile("Release")
        .out_dir(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("native"))
        .define("CMAKE_POLICY_VERSION_MINIMUM", "3.5")
        .define("CMAKE_TRY_COMPILE_CONFIGURATION", "Release")
        .define("NCNN_VERSION", ncnn_dependency[2])
        .define(
            "RIFE_NATIVE_LIBRARY",
            env::var("CARGO_MANIFEST_LINKS").unwrap(),
        )
        .define("NCNN_SOURCE", ncnn)
        .define("VULKAN_HEADERS", &headers);
    let target = env::var("TARGET").unwrap();
    if target.contains("msvc") {
        config.generator("Ninja");
        let static_crt = env::var("CARGO_CFG_TARGET_FEATURE")
            .unwrap_or_default()
            .split(',')
            .any(|feature| feature == "crt-static");
        config.define("RIFE_STATIC_RUNTIME", if static_crt { "ON" } else { "OFF" });
        config.define(
            "VULKAN_MACHINE",
            if target.starts_with("aarch64") {
                "ARM64"
            } else if target.starts_with("x86_64") {
                "X64"
            } else {
                "X86"
            },
        );
        config
            .define("CMAKE_C_FLAGS_RELEASE", "/O2 /DNDEBUG")
            .define("CMAKE_CXX_FLAGS_RELEASE", "/O2 /DNDEBUG /EHsc");
        // Import the system driver loader; no Vulkan DLL is bundled.
        let header = fs::read_to_string(headers.join("include/vulkan/vulkan_core.h")).unwrap();
        let mut definition = String::from("LIBRARY vulkan-1.dll\nEXPORTS\n");
        for line in header.lines() {
            if line.starts_with("VKAPI_ATTR ")
                && let Some((_, tail)) = line.split_once("VKAPI_CALL ")
                && let Some((name, _)) = tail.split_once('(')
            {
                definition.push_str(name);
                definition.push('\n');
            }
        }
        let path = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("vulkan-1.def");
        fs::write(&path, definition).unwrap();
        config.define("VULKAN_DEF", path);
    }
    let dst = config.build();
    println!(
        "cargo:rustc-link-search=native={}",
        dst.join("lib").display()
    );
    let libraries = fs::read_to_string(dst.join("lib/native-libraries.txt"))
        .expect("CMake did not report its static libraries");
    for library in libraries.lines().filter(|line| !line.is_empty()) {
        println!("cargo:rustc-link-lib=static={library}");
    }
    if target.contains("msvc") {
        println!("cargo:rustc-link-lib=vulkan-1");
    } else if target.contains("apple") {
        println!("cargo:rustc-link-lib=c++");
        println!("cargo:rustc-link-lib=vulkan");
    } else {
        for lib in ["stdc++", "vulkan", "pthread", "dl"] {
            println!("cargo:rustc-link-lib={lib}");
        }
    }
}
