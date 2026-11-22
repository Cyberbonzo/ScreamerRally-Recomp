//! Native renderer, extraction side (`docs/04-native-renderer.md`): reads the game's world,
//! models, materials, textures and transforms from guest memory into neutral descriptions
//! (no Direct3D types, no guest addresses in the output), plus the diagnostics of each
//! wave-1 topic. Nothing here changes the game's behaviour unless its environment
//! variable is set.

/// The scene description the native renderer draws (crate `sr-scene`).
pub use sr_scene as scene;

/// The drawn chase camera kept out of walls (camera collision).
pub mod camera;
/// Moving objects: cars, shadows, smoke, tyre marks (W3-1).
pub mod dynamics;
/// Which moving objects are drawn: backed by the world under them.
pub mod dynvis;
pub mod extract;
/// The integration (W2-3): switch, scene per race, frames, hooks.
pub mod frame;
/// The menus' 3D scenes (W4-2).
pub mod menus;
/// Near-camera fidelity (W3-2): the engine's near-plane handling and its check.
pub mod near;
/// The designers' piece names and world triangles of a track's scenery (the water's).
pub mod objects;
/// Compressed PNG files for the diagnostics' pictures.
pub mod png;

pub mod polygons;
pub mod textures;
pub mod transforms;
/// Which statics are drawn (W2-3).
pub mod visibility;
/// The game's fake water reflections (reflection cards drawn on the water).
pub mod water;
pub mod world;

use crate::Machine;

/// Called from the executable profile once every hook is in place.
pub fn install(m: &mut Machine) {
    textures::install(m);
    extract::install(m);
    frame::install(m);
    near::install(m);
    dynamics::install(m);
    menus::install(m);
}

/// Called at the start of every race frame (the race frame timer's tick wait has ended).
pub fn race_frame(m: &mut Machine) {
    textures::race_frame(m);
    world::race_frame(m);
    extract::race_frame(m);
    frame::race_frame(m);
}
