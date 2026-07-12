# Installation

Weft is published on crates.io:

```sh
cargo install weft-dst      # installs the `weft` binary
cargo build --release -p weft-shim   # libweft_shim.so (Linux only)
```

Or from source:

```sh
git clone https://github.com/arnavsinghal09/weft && cd weft
cargo install --path crates/weft-dst
cargo build --release -p weft-shim
```

`weft run` finds the shim via `WEFT_SHIM`, or next to the `weft` binary:

```sh
cp "${CARGO_TARGET_DIR:-target}/release/libweft_shim.so" "$(dirname "$(command -v weft)")/"
```

Interception itself needs Linux (x86-64, glibc, dynamically linked targets —
see [Limitations](limitations.md) §1). On macOS, run everything inside
Docker; the [user guide](user-guide.md) has the exact container recipe.

Crate pages: [weft-dst](https://crates.io/crates/weft-dst) ·
[weft-abi](https://crates.io/crates/weft-abi) ·
[weft-net](https://crates.io/crates/weft-net) ·
[weft-scenario](https://crates.io/crates/weft-scenario) ·
[weft-replay](https://crates.io/crates/weft-replay) ·
[weft-fuzz](https://crates.io/crates/weft-fuzz)
