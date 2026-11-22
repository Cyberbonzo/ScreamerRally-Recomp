//! Finding the player's game: the command line, the game remembered in `settings.ini` (or a
//! disc image an earlier version remembered), a CD image next to the program, or the
//! first-run window (`crate::welcome`), where the player picks the game's folder.
//!
//! A game is either the **CD image** (`.mds`, `.cue` with its `.bin` files, or `.iso`: the
//! data track, and the CD music from the audio tracks) or the **DOS release installed in a
//! folder** that runs without its CD (`STRT3FX.EXE`, `DATA`, `GFX` ...: no CD music). The
//! folder the player picks may be either, or hold one of them up to two levels down (the
//! folder with the image, or `screamer_rally\sr` of an unpacked archive). Every candidate is
//! checked (its `STRT3FX.EXE` must be a version the runtime knows, its `DATA\LOCAL.DAT`
//! readable) before it is used and remembered (`[game] path`); its language is the game's.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MessageBoxW};

use crate::language::{self, Language};
use crate::settings::Settings;

/// The executable on the CD, and in an installed game's folder.
const CD_EXE: &str = "START0\\STRT3FX.EXE";
const EXE: &str = "STRT3FX.EXE";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Show a message box (the program has no console in release builds).
pub fn message(text: &str, error: bool) {
    let (t, c) = (wide(text), wide("Screamer Rally"));
    unsafe { MessageBoxW(std::ptr::null_mut(), t.as_ptr(), c.as_ptr(), MB_OK | if error { MB_ICONERROR } else { MB_ICONINFORMATION }) };
}

/// A checked game.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    /// The CD image, or the installed game's folder (the one holding `STRT3FX.EXE`).
    pub path: PathBuf,
    /// One line for the player: language, kind, music.
    pub description: String,
    /// None: a game whose text is in neither language.
    pub language: Option<Language>,
    /// CD music (audio tracks) available.
    pub music: bool,
}

fn language_name(l: Option<Language>) -> &'static str {
    l.map_or("unknown language", Language::name)
}

/// The executable's version (by its SHA-256), or why it cannot run.
fn known_exe(exe: &[u8], name: &str) -> Result<(), String> {
    let hash = sr_disc::sha256::hex(&sr_disc::sha256::sha256(exe));
    sr_runtime::profile::find(&hash).map(|_| ()).ok_or_else(|| format!("{name} is a version of the game this program does not know (sha256 {hash})"))
}

/// A CD image.
fn validate_image(path: &Path) -> Result<Found, String> {
    let disc = sr_disc::Disc::open(path).map_err(|e| e.to_string())?;
    let fs = disc.filesystem().map_err(|e| format!("no readable data track ({e})"))?;
    let entry = fs.lookup(CD_EXE).map_err(|e| e.to_string())?.ok_or_else(|| "this is not a Screamer Rally CD image".to_string())?;
    known_exe(&fs.read_file(&entry).map_err(|e| e.to_string())?, CD_EXE)?;
    let music = disc.tracks().iter().filter(|t| t.kind == sr_disc::TrackKind::Audio).count();
    // the game reads its text from DATA\LOCAL.DAT: a disc without a readable one cannot run
    let local = fs.lookup("DATA\\LOCAL.DAT").map_err(|e| e.to_string())?.ok_or("DATA\\LOCAL.DAT is not on this disc")?;
    let language = language::identify(&fs.read_file(&local).map_err(|e| e.to_string())?)?;
    let tracks = if music == 0 { "no music tracks".to_string() } else { format!("{music} music tracks") };
    Ok(Found { path: path.to_path_buf(), description: format!("{} CD image, {tracks}", language_name(language)), language, music: music > 0 })
}

/// The DOS release installed in `dir` (its `STRT3FX.EXE` there).
fn validate_folder(dir: &Path) -> Result<Found, String> {
    let exe = std::fs::read(dir.join(EXE)).map_err(|e| format!("{EXE}: {e}"))?;
    known_exe(&exe, EXE)?;
    for sub in ["DATA", "GFX"] {
        if !dir.join(sub).is_dir() {
            return Err(format!("the {sub} folder is missing next to {EXE}"));
        }
    }
    let local = std::fs::read(dir.join("DATA").join("LOCAL.DAT")).map_err(|e| format!("DATA\\LOCAL.DAT: {e}"))?;
    let language = language::identify(&local)?;
    Ok(Found { path: dir.to_path_buf(), description: format!("{} DOS game (no CD music)", language_name(language)), language, music: false })
}

fn is_image(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "mds" | "cue" | "iso"))
}

/// The CD images in `dir`, sorted.
fn images_in(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_file() && is_image(p)).collect();
    v.sort();
    v
}

/// Check `path`: a CD image, a game folder, or a folder holding one of them up to two levels
/// down (an image first, then an installed game; the first that works).
pub fn validate(path: &Path) -> Result<Found, String> {
    if path.is_file() {
        return validate_image(path);
    }
    if !path.is_dir() {
        return Err(format!("{} does not exist", path.display()));
    }
    let mut first_error = None;
    let mut level = vec![path.to_path_buf()];
    for depth in 0..3 {
        let mut next = Vec::new();
        for dir in &level {
            let candidates = images_in(dir).into_iter().map(|p| (p, true)).chain(dir.join(EXE).is_file().then(|| (dir.clone(), false)));
            for (p, image) in candidates {
                match if image { validate_image(&p) } else { validate_folder(&p) } {
                    Ok(f) => return Ok(f),
                    Err(e) => {
                        first_error.get_or_insert(format!("{}: {e}", p.display()));
                    }
                }
            }
            if depth < 2 {
                let mut subs: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).take(64).collect();
                subs.sort();
                next.extend(subs);
            }
        }
        level = next;
    }
    Err(first_error.unwrap_or_else(|| "No Screamer Rally game in this folder: it needs the CD image (.mds, .cue or .iso) or the installed DOS game (STRT3FX.EXE).".into()))
}

pub type Probe = Result<Found, String>;

pub trait Env {
    fn probe(&mut self, path: &Path) -> Probe;
    /// CD images next to the program.
    fn nearby(&mut self) -> Vec<PathBuf>;
    /// Ask the player (the first-run window; `note`: why, when a remembered game is gone).
    /// What it returns has been checked.
    fn pick(&mut self, note: Option<&str>) -> Option<Found>;
    fn error(&mut self, text: &str);
}

pub struct Windows {
    search: Vec<PathBuf>,
    checked: HashMap<PathBuf, Probe>,
}

impl Windows {
    pub fn new(search: Vec<PathBuf>) -> Windows {
        Windows { search, checked: HashMap::new() }
    }
}

fn probe_logged(path: &Path) -> Probe {
    let r = validate(path);
    match &r {
        Ok(f) => crate::log_info(&format!("game {}: {} ({})", path.display(), f.description, f.path.display())),
        Err(e) => crate::log_info(&format!("game {} cannot be used: {e}", path.display())),
    }
    r
}

impl Env for Windows {
    fn probe(&mut self, path: &Path) -> Probe {
        self.checked.entry(path.to_path_buf()).or_insert_with(|| probe_logged(path)).clone()
    }

    fn nearby(&mut self) -> Vec<PathBuf> {
        self.search.iter().flat_map(|d| images_in(d)).collect()
    }

    fn pick(&mut self, note: Option<&str>) -> Option<Found> {
        crate::welcome::run(note, &mut |p: &Path| probe_logged(p))
    }

    fn error(&mut self, text: &str) {
        message(text, true);
    }
}

/// The game to run (`None`: none given, or the player closed the first-run window). What is
/// learnt is remembered in the settings (`game`); a game named on the command line is not.
pub fn choose(s: &mut Settings, given: Option<&Path>, env: &mut dyn Env) -> Option<Found> {
    if let Some(p) = given {
        return match env.probe(p) {
            Ok(f) => Some(f),
            Err(e) => {
                env.error(&format!("{}:\n{e}", p.display()));
                None
            }
        };
    }
    let mut note = None;
    if let Some(p) = s.game.clone() {
        match env.probe(&p) {
            Ok(f) => {
                s.game = Some(f.path.clone());
                return Some(f);
            }
            Err(e) => note = Some(format!("Your game at {} cannot be used any more ({e}). Please select it again.", p.display())),
        }
    }
    let earlier = std::mem::take(&mut s.earlier_games);
    let nearby = env.nearby();
    for p in earlier.iter().chain(nearby.iter()) {
        if let Ok(f) = env.probe(p) {
            s.game = Some(f.path.clone());
            return Some(f);
        }
    }
    let f = env.pick(note.as_deref())?;
    s.game = Some(f.path.clone());
    Some(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        games: HashMap<PathBuf, Option<Language>>,
        next_to_program: Vec<PathBuf>,
        answer: Option<PathBuf>,
        notes: Vec<Option<String>>,
        errors: usize,
    }

    fn fake(games: &[(&str, Option<Language>)]) -> Fake {
        Fake { games: games.iter().map(|(p, l)| (PathBuf::from(p), *l)).collect(), next_to_program: Vec::new(), answer: None, notes: Vec::new(), errors: 0 }
    }

    impl Env for Fake {
        fn probe(&mut self, path: &Path) -> Probe {
            let language = *self.games.get(path).ok_or("not a Screamer Rally game")?;
            Ok(Found { path: path.to_path_buf(), description: String::new(), language, music: true })
        }
        fn nearby(&mut self) -> Vec<PathBuf> {
            self.next_to_program.clone()
        }
        fn pick(&mut self, note: Option<&str>) -> Option<Found> {
            self.notes.push(note.map(String::from));
            let p = self.answer.clone()?;
            self.probe(&p).ok()
        }
        fn error(&mut self, _: &str) {
            self.errors += 1;
        }
    }

    fn settings(text: &str) -> Settings {
        let (s, warn) = Settings::parse(text);
        assert!(warn.is_empty(), "{warn:?}");
        s
    }

    #[test]
    fn the_remembered_game_is_used() {
        let mut env = fake(&[("C:\\sr", Some(Language::English))]);
        let mut s = settings("[game]\npath = C:\\sr\n");
        let f = choose(&mut s, None, &mut env).unwrap();
        assert_eq!((f.path.as_path(), f.language), (Path::new("C:\\sr"), Some(Language::English)));
        assert!(env.notes.is_empty());
    }

    #[test]
    fn earlier_versions_images_then_nearby_then_the_window() {
        // an earlier version's image: taken and remembered
        let mut env = fake(&[("it.mds", Some(Language::Italian))]);
        let mut s = settings("[game]\ndisc_italian = it.mds\n");
        assert_eq!(choose(&mut s, None, &mut env).unwrap().path, Path::new("it.mds"));
        assert_eq!(s.game.as_deref(), Some(Path::new("it.mds")));
        assert!(s.to_ini().contains("\npath = it.mds\n"));
        // nothing remembered: an image next to the program
        let mut env = fake(&[("next.cue", None)]);
        env.next_to_program = vec!["bad.iso".into(), "next.cue".into()];
        let mut s = Settings::default();
        assert_eq!(choose(&mut s, None, &mut env).unwrap().path, Path::new("next.cue"));
        // nothing at all: the window, without a note; closed: no game
        let mut env = fake(&[("D:\\picked", None)]);
        let mut s = Settings::default();
        assert_eq!(choose(&mut s, None, &mut env), None);
        assert_eq!(env.notes, [None]);
        env.answer = Some("D:\\picked".into());
        assert_eq!(choose(&mut s, None, &mut env).unwrap().path, Path::new("D:\\picked"));
        assert_eq!(s.game.as_deref(), Some(Path::new("D:\\picked")));
    }

    #[test]
    fn a_game_gone_is_asked_for_again_with_a_note() {
        let mut env = fake(&[("E:\\new", Some(Language::Italian))]);
        env.answer = Some("E:\\new".into());
        let mut s = settings("[game]\npath = E:\\moved\n");
        assert_eq!(choose(&mut s, None, &mut env).unwrap().path, Path::new("E:\\new"));
        assert!(env.notes[0].as_deref().is_some_and(|n| n.contains("E:\\moved")));
        assert_eq!(s.game.as_deref(), Some(Path::new("E:\\new")));
    }

    #[test]
    fn the_command_line_is_not_remembered() {
        let mut env = fake(&[("x.mds", None)]);
        let mut s = Settings::default();
        assert!(choose(&mut s, Some(Path::new("x.mds")), &mut env).is_some());
        assert_eq!(s.game, None);
        assert!(choose(&mut s, Some(Path::new("y.mds")), &mut env).is_none());
        assert_eq!(env.errors, 1);
    }

    #[test]
    fn folders_are_searched_two_levels_down() {
        let root = std::env::temp_dir().join(format!("sr-firstrun-{}", std::process::id()));
        let game = root.join("screamer_rally").join("sr");
        std::fs::create_dir_all(game.join("DATA")).unwrap();
        std::fs::create_dir_all(game.join("GFX")).unwrap();
        // an executable this program does not know: found, and refused with the reason
        std::fs::write(game.join(EXE), b"not the game").unwrap();
        let e = validate(&root).unwrap_err();
        assert!(e.contains("does not know"), "{e}");
        // nothing there at all
        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(validate(&empty).unwrap_err().contains("No Screamer Rally game"));
        assert!(validate(&root.join("missing")).unwrap_err().contains("does not exist"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
