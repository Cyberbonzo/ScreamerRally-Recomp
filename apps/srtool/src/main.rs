//! `srtool` — developer utility for inspecting the original game files.
//!
//! ```text
//! srtool disc    <image.mds>            table of contents and volume info
//! srtool ls      <image.mds>            list every file on the data track
//! srtool extract <image.mds> <dir>      copy the data track's files to a folder
//! srtool raw     <image.mds> <lba> <count> <file>  raw 2352-byte sectors (decimal LBA) to a file
//! srtool dat     <image.mds> <DATA\LOCAL.DAT> [<entry> <file>]  list a game archive, or unpack an entry
//! srtool le     <file.exe>             LE objects, entry point and fixup summary
//! srtool hash    <file>...              SHA-256 of files
//! srtool dis     <file.exe> <addr> [len] disassemble at a link address (hex), fixups applied
//! srtool crt-pack <ShaderGlass checkout> [out.pak [category,...]]  the CRT shader library pack
//!                                       (crt-shaders.pak; the package: category crt)
//! srtool crt-list <pack> [filter]       presets of a pack (with parameters when one matches)
//! ```

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use sr_disc::sha256::{hex, sha256};
use sr_disc::{Disc, TrackKind};
use sr_le::LeImage;

type Res = Result<(), Box<dyn std::error::Error>>;

fn cmd_disc(path: &str) -> Res {
    let disc = Disc::open(path)?;
    println!("{}: {} sectors", disc.path().display(), disc.total_sectors());
    for t in disc.tracks() {
        let secs = t.length as f64 / 75.0;
        let (m, s, f) = sr_disc::lba_to_msf(t.start);
        println!(
            "  track {:2} {:5} lba {:6}  msf {:02}:{:02}:{:02}  length {:6} ({}:{:05.2})",
            t.number,
            if t.kind == TrackKind::Data { "data" } else { "audio" },
            t.start,
            m,
            s,
            f,
            t.length,
            (secs / 60.0) as u32,
            secs % 60.0
        );
    }
    let fs = disc.filesystem()?;
    println!("volume '{}' ({} blocks)", fs.volume_id, fs.volume_blocks);
    Ok(())
}

fn cmd_ls(path: &str) -> Res {
    let disc = Disc::open(path)?;
    let fs = disc.filesystem()?;
    fs.walk(|p, e| {
        let (y, mo, d, h, mi, s) = e.time;
        let kind = if e.is_dir { "<DIR>" } else { "" };
        println!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02} {:>10} {kind:5} {p}", e.size);
    })?;
    Ok(())
}

fn cmd_extract(path: &str, dest: &str) -> Res {
    let disc = Disc::open(path)?;
    let fs = disc.filesystem()?;
    let mut files = Vec::new();
    fs.walk(|p, e| files.push((p.to_string(), e.clone())))?;
    let mut bytes = 0u64;
    for (p, e) in &files {
        let out = Path::new(dest).join(p.replace('\\', "/"));
        if e.is_dir {
            std::fs::create_dir_all(&out)?;
        } else {
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&out, fs.read_file(e)?)?;
            bytes += e.size as u64;
        }
    }
    println!("extracted {} entries, {} bytes", files.len(), bytes);
    Ok(())
}

/// Write `count` raw sectors from `lba` as the disc reader returns them (audio as 16-bit
/// stereo PCM; sectors the image does not hold as silence).
fn cmd_raw(path: &str, lba: &str, count: &str, out: &str) -> Res {
    let disc = Disc::open(path)?;
    let (lba, count): (u32, u32) = (lba.parse()?, count.parse()?);
    let mut buf = vec![0u8; count as usize * sr_disc::RAW_SECTOR];
    disc.read_raw(lba, count, &mut buf)?;
    std::fs::write(out, &buf)?;
    println!("{count} sectors from lba {lba} -> {out}");
    Ok(())
}

/// List the entries of one of the game's `.DAT` archives on the disc, or unpack one entry
/// (decompressed) to a file.
fn cmd_dat(path: &str, archive: &str, entry: Option<(&str, &str)>) -> Res {
    let disc = Disc::open(path)?;
    let fs = disc.filesystem()?;
    let e = fs.lookup(archive)?.ok_or_else(|| format!("{archive} is not on this disc"))?;
    let data = fs.read_file(&e)?;
    let arc = sr_disc::dat::Archive::parse(&data)?;
    match entry {
        None => {
            for e in arc.entries() {
                println!("{:16} offset {:8} stored {:8} size {:8} flags {}", e.name, e.offset, e.stored, e.size, e.flags);
            }
        }
        Some((name, out)) => {
            let e = arc.find(name).ok_or_else(|| format!("{name} is not in {archive}"))?;
            let bytes = arc.read(e)?;
            std::fs::write(out, &bytes)?;
            println!("{} ({} bytes) -> {out}", e.name, bytes.len());
        }
    }
    Ok(())
}

fn cmd_le(path: &str) -> Res {
    let data = std::fs::read(path)?;
    let le = LeImage::parse(&data)?;
    println!("{path}: page size {}, {} fixups", le.page_size, le.fixups.len());
    println!(
        "entry obj{}:0x{:x}  stack obj{}:0x{:x}  auto data obj{}",
        le.entry_object, le.entry_offset, le.stack_object, le.stack_offset, le.auto_data_object
    );
    for o in &le.objects {
        println!(
            "  obj{} link 0x{:08x} vsize 0x{:06x} stored 0x{:06x} flags 0x{:04x}{}{}{}",
            o.number,
            o.link_base,
            o.virtual_size,
            o.data.len(),
            o.flags,
            if o.is_code() { " code" } else { " data" },
            if o.is_32bit() { " 32-bit" } else { " 16-bit" },
            if o.flags & sr_le::flags::ALIAS_1616 != 0 { " 16:16-alias" } else { "" },
        );
    }
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for f in &le.fixups {
        *kinds.entry(format!("{:?} -> obj{}", f.kind, f.target_object)).or_default() += 1;
    }
    for (k, n) in kinds {
        println!("  {n:6} {k}");
    }
    Ok(())
}

/// Disassemble `len` bytes at link address `addr` (both hex) of an LE executable, with
/// its fixups applied at the objects' link addresses.
fn cmd_dis(path: &str, addr: &str, len: &str) -> Res {
    use iced_x86::{Decoder, DecoderOptions, Formatter, IntelFormatter};
    let hex = |s: &str| u32::from_str_radix(s.trim_start_matches("0x"), 16);
    let (start, len) = (hex(addr)?, hex(len)?);
    let le = LeImage::parse(&std::fs::read(path)?)?;
    let bases: Vec<u32> = le.objects.iter().map(|o| o.link_base).collect();
    let images = le.relocate(&bases, |_| 0)?;
    let (o, img) = le
        .objects
        .iter()
        .zip(&images)
        .find(|(o, img)| start >= o.link_base && start < o.link_base + img.len() as u32)
        .ok_or("address outside every object")?;
    let from = (start - o.link_base) as usize;
    let code = &img[from..(from + len as usize).min(img.len())];
    let mut dec = Decoder::with_ip(32, code, start as u64, DecoderOptions::NONE);
    let mut fmt = IntelFormatter::new();
    let mut text = String::new();
    while dec.can_decode() {
        let ins = dec.decode();
        text.clear();
        fmt.format(&ins, &mut text);
        let at = (ins.ip() - start as u64) as usize;
        let bytes: String = code[at..at + ins.len()].iter().map(|b| format!("{b:02x}")).collect();
        println!("{:08x}  {bytes:24} {text}", ins.ip());
    }
    Ok(())
}

/// Generate the CRT shader library pack from a ShaderGlass checkout (its generated
/// headers under `ShaderGlass/Shaders`).
fn cmd_crt_pack(root: &str, out: Option<&str>, only: &[&str]) -> Res {
    let out = out.map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from(sr_crt::pack::FILE_NAME));
    let t = std::time::Instant::now();
    let r = sr_crt::import::build_pack(Path::new(root), &out, only, &mut |m| println!("  {m}"))?;
    for s in &r.skipped {
        println!("skipped: {s}");
    }
    println!(
        "{}: {} presets, {} shaders ({:.1} MB of bytecode), {} textures ({:.1} MB), {:.1} MB in {:.1} s",
        out.display(),
        r.presets,
        r.shaders,
        r.shader_bytes as f64 / 1048576.0,
        r.textures,
        r.texture_bytes as f64 / 1048576.0,
        r.bytes as f64 / 1048576.0,
        t.elapsed().as_secs_f64()
    );
    Ok(())
}

/// List a pack's presets (those whose key contains `filter`), with the parameters of each
/// when at most three match.
fn cmd_crt_list(pack: &str, filter: Option<&str>) -> Res {
    let p = sr_crt::Pack::open(Path::new(pack))?;
    println!("{}: {} presets in {} categories", p.index.source, p.index.presets.len(), p.index.categories.len());
    let hits: Vec<usize> = (0..p.index.presets.len()).filter(|&i| filter.is_none_or(|f| p.index.key(i).to_lowercase().contains(&f.to_lowercase()))).collect();
    for &i in &hits {
        let d = p.load(i, false)?;
        println!("{:5} {}  ({} passes, {} textures)", i, p.index.key(i), d.passes.len(), d.textures.len());
        if hits.len() <= 3 {
            for (n, pass) in d.passes.iter().enumerate() {
                println!("      pass {n}: {} {:?}", pass.shader.name, pass.options);
            }
            for u in d.user_params() {
                println!("      {} = {} ({} .. {} step {}) {}", u.name, u.default, u.min, u.max, u.step, u.description);
            }
        }
    }
    Ok(())
}

fn cmd_hash(paths: &[String]) -> Res {
    for p in paths {
        println!("{}  {p}", hex(&sha256(&std::fs::read(p)?)));
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["disc", p] => cmd_disc(p),
        ["ls", p] => cmd_ls(p),
        ["extract", p, d] => cmd_extract(p, d),
        ["raw", p, l, n, o] => cmd_raw(p, l, n, o),
        ["dat", p, a] => cmd_dat(p, a, None),
        ["dat", p, a, e, o] => cmd_dat(p, a, Some((e, o))),
        ["le", p] => cmd_le(p),
        ["hash", ..] => cmd_hash(&args[1..]),
        ["dis", p, a] => cmd_dis(p, a, "100"),
        ["dis", p, a, n] => cmd_dis(p, a, n),
        ["crt-pack", r] => cmd_crt_pack(r, None, &[]),
        ["crt-pack", r, o] => cmd_crt_pack(r, Some(o), &[]),
        ["crt-pack", r, o, c] => cmd_crt_pack(r, Some(o), &c.split(',').collect::<Vec<_>>()),
        ["crt-list", p] => cmd_crt_list(p, None),
        ["crt-list", p, f] => cmd_crt_list(p, Some(f)),
        _ => {
            eprintln!("usage: srtool disc|ls|extract|raw|dat|le|hash|dis|crt-pack|crt-list ... (see source header)");
            return ExitCode::from(2);
        }
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
