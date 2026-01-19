
# build lib
cargo build --release --target wasm32-unknown-unknown -p crab_feast_wasm

# generate js
wasm-bindgen --out-name crab_feast_wasm --out-dir launcher/wasm/target --target web target/wasm32-unknown-unknown/release/crab_feast_wasm.wasm

# assets copy

# powershell
# New-Item -ItemType SymbolicLink -Path "./assets" -Target "../../assets"


# run
http-server launcher/wasm --cors -p 8080

# run on basic http server
basic-http-server launcher/wasm


# debug
# $env:RUSTFLAGS = "-C debuginfo=2 -C opt-level=0"
# cargo build --release --target wasm32-unknown-unknown -p crab_feast_wasm
# wasm-bindgen --out-name crab_feast_wasm --out-dir launcher/wasm/target --target web target/wasm32-unknown-unknown/release/crab_feast_wasm.wasm --keep-debug-symbols