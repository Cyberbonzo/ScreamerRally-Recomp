//! CRT emulation for Screamer Rally: a Rust port of ShaderGlass's Direct3D 11 shader
//! runtime, running RetroArch's slang shader library (as ShaderGlass precompiles it to
//! DXBC) on the game's finished frames.
//!
//! ShaderGlass: shader effect overlay. Copyright (C) 2021-2025 mausimus (mausimus.net),
//! https://github.com/mausimus/ShaderGlass, GNU General Public License v3.0. The shaders are
//! the libretro slang-shaders (https://github.com/libretro/slang-shaders), each under the
//! licence its authors gave it.
//!
//! * `pack`: the library file (`crt-shaders.pak`) generated from a ShaderGlass checkout by
//!   `import` (`srtool crt-pack`), read on demand;
//! * `chain`: the runtime (passes, textures, uniforms);
//! * `stage`: what the renderer calls each frame;
//! * `config`: the settings shared with the overlay, ShaderGlass's option tables.

pub mod chain;
pub mod config;
pub mod image;
pub mod import;
pub mod pack;
pub mod stage;

pub use config::{CrtLive, CrtSettings, SharedCrt};
pub use pack::Pack;
pub use stage::Stage;
