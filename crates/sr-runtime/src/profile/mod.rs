//! Knowledge about specific original executables.
//!
//! A profile is selected by the SHA-256 of the executable and installs the hooks that
//! replace hardware-facing library code. Every hook re-checks the original bytes at its
//! address before patching, so a wrong profile can never patch the wrong code.

mod strt3fx;

use crate::Machine;

pub struct Profile {
    pub name: &'static str,
    pub sha256: &'static str,
    pub install: fn(&mut Machine) -> Result<(), String>,
}

pub static PROFILES: &[Profile] = &[
    Profile {
        // the same file on the Italian and the English CD (docs/re/english.md)
        name: "Screamer Rally, Italian / English CD, 3dfx Glide build (START0\\STRT3FX.EXE)",
        sha256: "9eac6e12ab309389bdda019cfa695bfbbc791fbda1a4634464a444fce35165d1",
        install: strt3fx::install,
    },
    Profile {
        // the DOS release installed without its CD: the CD's file with 7 bytes changed (the
        // CD check at 39c98 returns 0; the code check at 3a290 copies the expected bytes
        // instead of comparing them), neither touched by a hook (docs/re/strt3fx.md)
        name: "Screamer Rally, DOS release without the CD, 3dfx Glide build (STRT3FX.EXE)",
        sha256: "f7ae60e0325995826c60c7e4df2a181a748378ec49ed45ddb5a3749a2256eccb",
        install: strt3fx::install,
    },
];

pub fn find(sha256: &str) -> Option<&'static Profile> {
    PROFILES.iter().find(|p| p.sha256 == sha256)
}
