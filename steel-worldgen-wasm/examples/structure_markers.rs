//! Prints the browser module's structure markers around one block position.
//!
//! Comparison tools use it to choose fixed anchors. Runs no tests.
//!
//! Usage: `structure_markers SEED BLOCK_X BLOCK_Z RADIUS`

use steel_worldgen_wasm::Generator;

fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let [seed, x, z, radius] = arguments.as_slice() else {
        panic!("usage: structure_markers SEED BLOCK_X BLOCK_Z RADIUS");
    };
    let parse = |value: &str| -> i32 {
        value
            .parse()
            .unwrap_or_else(|error| panic!("invalid integer {value}: {error}"))
    };
    let mut generator = Generator::new(seed, "overworld", Some(256))
        .unwrap_or_else(|error| panic!("generator: {error}"));
    let markers = generator
        .structure_markers_json(parse(x), parse(z), parse(radius))
        .unwrap_or_else(|error| panic!("markers: {error}"));
    println!("{markers}");
}
