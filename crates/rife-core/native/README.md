# Native backend

This directory contains the statically linked C++/NCNN implementation behind
the checked C ABI in `bridge.cpp`. Build prerequisites, dependency setup,
native test instructions, and guidance for changing model support are in the
root [CONTRIBUTING.md](../../../CONTRIBUTING.md).

Runtime model compatibility is described in the root [README](../../../README.md).
The CLI embeds the selected model assets and supplies their extracted paths to
its native worker. Library callers supply their own model directory; model
selection and extraction are CLI responsibilities.

The Rust build script and `CMakeLists.txt` compile pinned NCNN/glslang sources
with project overlays. Keep fetched upstream sources unchanged; validate overlay
anchors when updating a dependency. Model graphs are checked in Rust before
native loading, and the CLI supervises native work in a separate process.

Required native attributions and license texts are listed in
[THIRD_PARTY_NOTICES.md](../../../THIRD_PARTY_NOTICES.md) and
[LICENSES/native/](../../../LICENSES/native/) at the repository root.
