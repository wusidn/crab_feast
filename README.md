# crab_feast
bevy playground

## Build

Single entry point:

```sh
scripts/build.sh <platform> [--release]
```

Platforms: `pc`, `wasm`, `android`, `ios`, `all` (pc + wasm). See `scripts/build.sh --help` for details.

The web build is served from the repository root:

```sh
scripts/build.sh wasm
python3 -m http.server 8080
# open http://localhost:8080/launcher/wasm/
```
