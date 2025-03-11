
# build lib
cargo build --release --target wasm32-unknown-unknown -p crab_feast_pc

# generate js
wasm-bindgen --out-name crab_feast_wasm --out-dir launcher/wasm/target --target web target/wasm32-unknown-unknown/release/crab_feast_pc.wasm

# run
http-server launcher/wasm --cors -p 8080