//! The game's language. The Italian and English CDs hold the same executable; the language
//! is in their data (`DATA\LOCAL.DAT` for the text, `GFX\GRAF2.DAT` / `GFX\GAME2.DAT` for
//! the pictures with words; docs/re/english.md), so the game the player gives decides it.
//!
//! A game's language is read from its text: `DATA\LOCAL.DAT` holds the menu and race
//! strings (`STRINGS.TXT`, `GAME.TXT`), and a few common menu words are looked for as whole
//! lines. The game whose text has clearly more of one language's words is that language;
//! anything else (another language, a modified text) is reported as unknown. This does not
//! depend on file sizes, hashes or line positions, so a re-issue with a small change in its
//! text is still recognised. Only shown (the first-run window, the log): nothing depends on
//! it.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Italian,
    English,
}

impl Language {
    pub fn name(self) -> &'static str {
        match self {
            Language::Italian => "Italian",
            Language::English => "English",
        }
    }
}

/// Menu words of each language (whole lines of the game's text: main menu, championship,
/// settings, loading, save, exit, the pause menu's resume and restart).
const MARKERS: [(Language, &[&str]); 2] = [
    (Language::Italian, &["MENU PRINCIPALE", "CAMPIONATO", "IMPOSTAZIONI", "CARICANDO...", "SALVA", "ESCI", "RIPRENDI", "RICOMINCIA"]),
    (Language::English, &["MAIN MENU", "CHAMPIONSHIP", "GAME SETTINGS", "LOADING...", "SAVE", "EXIT", "RESUME", "RESTART"]),
];

/// Lines needed (and more than the other language has) for a decision.
const MIN_MARKERS: usize = 3;

/// The language of the game's text (the lines of its `.TXT` entries), or None.
pub fn classify(text: &str) -> Option<Language> {
    let lines: Vec<String> = text.lines().filter(|l| !l.starts_with('*')).map(|l| l.trim().to_ascii_uppercase()).collect();
    let score = |words: &[&str]| words.iter().filter(|w| lines.iter().any(|l| l == *w)).count();
    let [(a, wa), (b, wb)] = MARKERS;
    let (sa, sb) = (score(wa), score(wb));
    if sa >= MIN_MARKERS && sa > sb {
        Some(a)
    } else if sb >= MIN_MARKERS && sb > sa {
        Some(b)
    } else {
        None
    }
}

/// The language of a game's text: its `DATA\LOCAL.DAT` archive (`None`: neither language).
pub fn identify(local_dat: &[u8]) -> Result<Option<Language>, String> {
    const FILE: &str = "DATA\\LOCAL.DAT";
    let archive = sr_disc::dat::Archive::parse(local_dat).map_err(|e| format!("{FILE}: {e}"))?;
    let mut text = String::new();
    for name in ["STRINGS.TXT", "GAME.TXT"] {
        if let Some(e) = archive.find(name) {
            let bytes = archive.read(e).map_err(|e| format!("{FILE} {name}: {e}"))?;
            // the text is in a DOS code page; the words looked for are plain ASCII
            text.extend(bytes.iter().map(|&b| b as char));
            text.push('\n');
        }
    }
    Ok(classify(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_by_menu_words() {
        let it = "*max chars 16\r\nGIOCATORE\r\nMENU PRINCIPALE\r\nCAMPIONATO\r\nARCADE\r\nCARICANDO...\r\nSALVA\r\nESCI\r\n";
        let en = "*max chars 16\r\nPLAYER\r\nMAIN MENU\r\nCHAMPIONSHIP\r\nARCADE\r\nLOADING...\r\nSAVE\r\nEXIT\r\n";
        assert_eq!(classify(it), Some(Language::Italian));
        assert_eq!(classify(en), Some(Language::English));
        // case and surrounding spaces do not matter; comment lines are skipped
        assert_eq!(classify("  main menu \nresume\nRestart\n*MENU PRINCIPALE\n*CAMPIONATO\n*ESCI\n"), Some(Language::English));
        // words inside longer lines do not count
        assert_eq!(classify("EXIT NOW\nSAVE GAME\nMAIN MENU 2\n"), None);
        // too few words, or a tie (a mixed text), or another language: unknown
        assert_eq!(classify("MAIN MENU\nEXIT\n"), None);
        assert_eq!(classify("MAIN MENU\nEXIT\nSAVE\nESCI\nSALVA\nRIPRENDI\n"), None);
        assert_eq!(classify("HAUPTMENÜ\nMEISTERSCHAFT\nLADEN...\nSPEICHERN\nENDE\n"), None);
        assert_eq!(classify(""), None);
    }

    #[test]
    fn identify_reads_the_archive() {
        // an archive with one stored entry: data, the directory (32-byte records, the name
        // XOR 3Ah, an empty record at the end) and its size
        let en: &[u8] = b"MAIN MENU\r\nCHAMPIONSHIP\r\nLOADING...\r\nEXIT\r\n";
        let mut rec = [0x3au8; 32];
        for (i, b) in b"STRINGS.TXT".iter().enumerate() {
            rec[i] = b ^ 0x3a;
        }
        for (i, v) in [0u32, en.len() as u32, en.len() as u32, 0].iter().enumerate() {
            rec[16 + 4 * i..20 + 4 * i].copy_from_slice(&v.to_le_bytes());
        }
        let mut a = en.to_vec();
        a.extend_from_slice(&rec);
        a.extend_from_slice(&[0x3a; 32]);
        a.extend_from_slice(&64u32.to_le_bytes());
        assert_eq!(identify(&a).unwrap(), Some(Language::English));
        assert!(identify(&[1, 2, 3]).is_err());
    }
}
