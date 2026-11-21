# Build dependencies and model assets

| File | Purpose |
| --- | --- |
| [`dependencies.lock`](dependencies.lock) | Upstream URLs and exact revisions for NCNN, Vulkan-Headers, and the RIFE model repository; also records required native submodules. |
| [`models.txt`](models.txt) | Model directories to embed in the CLI. The first entry sets its default model. |

From the repository root, run `sh scripts/fetch-dependencies.sh` to prepare all
inputs, or `sh scripts/fetch-models.sh` to fetch only selected model assets.
The CLI build script also fetches missing selected assets before embedding them.

Fetched repositories are ignored local build inputs. Native libraries are linked
into the executable, and selected model graphs, weights, and their license are
embedded with `rust-embed`. Changing `models.txt` changes the embedded selection;
it does not remove previously downloaded model directories. Release source
bundles include the selected assets and native sources for offline builds.

See [CONTRIBUTING.md](../CONTRIBUTING.md#selecting-bundled-models) for model
selection and update instructions, and
[THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md) for attributions and notices.
