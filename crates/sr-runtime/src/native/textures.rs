//! Topic C: the textures and palettes of the loaded track, read from the game's memory
//! (`docs/re/native/textures.md`; link addresses as in `docs/re/strt3fx.md`).
//!
//! When the game loads a material library (`%s.MLB`: the track's, the cars', a menu's),
//! the loader `ccae4` resamples every texture of the file to a power-of-two size, converts
//! it to its Glide format in one buffer ("MATERIALS", [1479cc]) and describes it in a
//! texture record (table at 1479d4, 38h bytes each, up to BF8h). A polygon names a material
//! (an entry of the library's directory); the material names a list of records (one per
//! animation frame). The texture manager in `cb6af` uploads a record to texture memory
//! when a polygon needs it and the record is not resident (a ring over the TMU). So every
//! texture of a race is in RAM, converted, for the whole race; only the background picture
//! (sky) is freed after its one download and is read back from the emulated TMU.
//!
//! `SR_DUMP_TEXTURES=<dir>`: at the first frame of each race with new libraries, every
//! texture record as PNG (palette applied), the background picture, the palettes,
//! `index.tsv` and `materials.tsv` in `<dir>/race<n>_<track library>`. For the whole run
//! every texture bind, download and chroma value the game sets is compared with what is
//! read from RAM (log lines `textures: check ...`).

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::Machine;
use crate::glide::Glide;
use crate::glide::consts::*;
use crate::glide::tex::{self, TexInfo};
use crate::mem;

/// Texture records: table, record size, number of records (the loader stops at BF8h:
/// "Superato limite texture cache").
pub const RECORDS: u32 = 0x1479d4;
pub const RECORD_SIZE: u32 = 0x38;
pub const RECORD_COUNT: u32 = 0xbf8;
/// Loaded files ("libraries"): 32 slots of 2Ch bytes, +0 data block, +8 name (16 bytes).
pub const LIBRARIES: u32 = 0x147420;
pub const LIBRARY_SIZE: u32 = 0x2c;
pub const LIBRARY_COUNT: u32 = 32;
/// Texture manager: 400h slots of (TMU address, record id); a record's +4 is its slot.
pub const CACHE_SLOTS: u32 = 0x171814;
pub const CACHE_SLOT_COUNT: u32 = 0x400;
/// Background picture: its GrTexInfo, its TMU address (grTexMinAddress), its RAM pixels
/// (pointer cleared once downloaded).
pub const SKY_INFO: u32 = 0x173fd4;
pub const SKY_TMU: u32 = 0x173f80;
pub const SKY_PIXELS: u32 = 0x14734c;
/// Texture resolution factor of the last library load (1.0 = the file's own sizes).
pub const TEX_SCALE: u32 = 0x12374c;
/// Polygon set-up state: data block of the model being drawn ([model + B4h]), material
/// entry of the polygon, animation phase of the object (Q14), and the texture id handed to
/// the polygon routine `cb6af` (its argument block at aa5b8: width, height, id, colour,
/// mode).
pub const DATA_BASE: u32 = 0x14739c;
pub const MATERIAL: u32 = 0x7bb80;
pub const PHASE: u32 = 0x7bb94;
pub const POLY_TEXTURE: u32 = 0xaa5c0;
pub const POLY_MODE: u32 = 0xaa5c8;
/// Second animation phase of the object (instance +3Ch, 16.16), used by mode 5.
pub const PHASE2: u32 = 0x7bb98;
/// Data block of the environment-map library (loaded with flag 4: `env`, ENVPxyz.DAT),
/// 0 when none: mode-5 polygons take its material 0 instead of their own (d59c7).
pub const ENV_BASE: u32 = 0xaa6a8;
/// RGB565 -> 8-bit tables of the chroma-colour conversion `54e2b` (32 and 64 dwords).
pub const CHROMA5: u32 = 0x12378c;
pub const CHROMA6: u32 = 0x12380c;
/// Return addresses of the texture manager's `grTexSource` calls (cb6af); the background
/// picture's is cb745.
const MANAGER_BINDS: std::ops::RangeInclusive<u32> = 0xcbd40..=0xcc388;
const SKY_BIND: u32 = 0xcb745;

/// Runtime addresses of the above.
#[derive(Clone, Copy)]
struct Addrs {
    records: u32,
    libraries: u32,
    cache: u32,
    sky_info: u32,
    sky_tmu: u32,
    sky_pixels: u32,
    scale: u32,
    data_base: u32,
    material: u32,
    phase: u32,
    poly_texture: u32,
    poly_mode: u32,
    phase2: u32,
    env_base: u32,
    chroma5: u32,
    chroma6: u32,
}

static ADDRS: OnceLock<Addrs> = OnceLock::new();

fn addrs() -> Addrs {
    *ADDRS.get_or_init(|| {
        let m = crate::machine();
        let l = |a: u32| m.img.link(a);
        Addrs {
            records: l(RECORDS),
            libraries: l(LIBRARIES),
            cache: l(CACHE_SLOTS),
            sky_info: l(SKY_INFO),
            sky_tmu: l(SKY_TMU),
            sky_pixels: l(SKY_PIXELS),
            scale: l(TEX_SCALE),
            data_base: l(DATA_BASE),
            material: l(MATERIAL),
            phase: l(PHASE),
            poly_texture: l(POLY_TEXTURE),
            poly_mode: l(POLY_MODE),
            phase2: l(PHASE2),
            env_base: l(ENV_BASE),
            chroma5: l(CHROMA5),
            chroma6: l(CHROMA6),
        }
    })
}

/// A texture record (1479d4 + 38h * id).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Record {
    pub id: u32,
    /// +00: non-zero = in use.
    pub used: u32,
    /// +04: texture-manager slot while resident in texture memory, else -1.
    pub cache_slot: i32,
    /// +08: bytes in texture memory (texels x bytes per texel).
    pub tmu_bytes: u32,
    /// +0C: palette of a GR_TEXFMT_P_8 texture (256 x 0x00RRGGBB, right after its texels).
    pub palette: u32,
    /// +10: library slot (LIBRARIES) that loaded it.
    pub library: u32,
    /// +14: chroma-key colour (0x00RRGGBB: the file palette's entry 0 through `54e2b`).
    pub chroma: u32,
    /// +18..+2B: GrTexInfo (small LOD, large LOD, aspect, format, data).
    pub info: TexInfo,
    /// +2C, +30: width and height over the larger of the two (1.0 or 1/2, 1/4, 1/8).
    pub s_scale: f32,
    pub t_scale: f32,
    /// +34: 1 = an alpha texture (file type 120h) with texels 30h-3Fh, converted to a grey
    /// RGB565 ramp instead.
    pub grey: u32,
}

impl Record {
    /// # Safety
    /// The executable's data must be loaded (any time after start-up).
    pub unsafe fn read(id: u32) -> Record {
        let p = addrs().records + id * RECORD_SIZE;
        unsafe {
            Record {
                id,
                used: mem::rd32(p),
                cache_slot: mem::rd32(p + 4) as i32,
                tmu_bytes: mem::rd32(p + 8),
                palette: mem::rd32(p + 0x0c),
                library: mem::rd32(p + 0x10),
                chroma: mem::rd32(p + 0x14),
                info: TexInfo::read(p + 0x18),
                s_scale: f32::from_bits(mem::rd32(p + 0x2c)),
                t_scale: f32::from_bits(mem::rd32(p + 0x30)),
                grey: mem::rd32(p + 0x34),
            }
        }
    }

    pub fn size(&self) -> (u32, u32) {
        tex::level_size(self.info.large_lod, self.info.aspect)
    }

    fn texel_bytes(&self) -> u32 {
        let (w, h) = self.size();
        w * h * tex::bytes_per_texel(self.info.format)
    }
}

/// Every record in use.
pub fn records() -> Vec<Record> {
    (0..RECORD_COUNT).map(|id| unsafe { Record::read(id) }).filter(|r| r.used != 0).collect()
}

/// A loaded file (library slot).
#[derive(Clone, Debug)]
pub struct Library {
    pub slot: u32,
    /// Its data block (material directory first; converted textures after the model data).
    pub data: u32,
    pub name: String,
    pub name2: String,
}

pub fn libraries() -> Vec<Library> {
    let a = addrs();
    let mut out = Vec::new();
    for slot in 0..LIBRARY_COUNT {
        let p = a.libraries + slot * LIBRARY_SIZE;
        let data = unsafe { mem::rd32(p) };
        if data == 0 {
            continue;
        }
        let s = |o: u32| String::from_utf8_lossy(&unsafe { mem::cstr(p + o, 16) }).to_string();
        out.push(Library { slot, data, name: s(8), name2: s(0x18) });
    }
    out
}

/// A material: an entry (20h bytes) of a library's directory, at the start of its data
/// block. Polygons name it by its offset from the block (polygon record +4, see topic B).
#[derive(Clone, Debug)]
pub struct Material {
    pub library: u32,
    pub index: u32,
    /// Offset from the library's data block.
    pub offset: u32,
    /// +00: the file's texture type (10h opaque, 40h colour-keyed, E0h, 100h / 120h alpha).
    pub kind: u32,
    /// +04: offset of its frame list from the data block (0 = no texture).
    pub list: u32,
    /// +08, +0C: texture width and height (the converted, power-of-two size).
    pub width: u32,
    pub height: u32,
    /// +10..+1F (+10: the file palette's offset).
    pub words: [u32; 4],
    /// Frame list +4 (meaning unknown).
    pub list_word1: u32,
    /// Frame list +8..: the record id of each frame.
    pub frames: Vec<u32>,
}

/// The directory of a data block (entries until one whose first four dwords are 0, as the
/// loader `ccae4` walks it).
pub fn materials(lib: &Library) -> Vec<Material> {
    let mut out = Vec::new();
    for index in 0..8192u32 {
        let e = lib.data + index * 0x20;
        if !mem::readable(e, 0x20) {
            break;
        }
        let w = |o: u32| unsafe { mem::rd32(e + o) };
        if w(0) == 0 && w(4) == 0 && w(8) == 0 && w(0x0c) == 0 {
            break;
        }
        let list = w(4);
        let (mut frames, mut list_word1) = (Vec::new(), 0);
        if list != 0 && mem::readable(lib.data + list, 8) {
            let l = lib.data + list;
            let count = unsafe { mem::rd32(l) };
            list_word1 = unsafe { mem::rd32(l + 4) };
            if count <= 256 && mem::readable(l + 8, count * 4) {
                frames = (0..count).map(|f| unsafe { mem::rd32(l + 8 + 4 * f) }).collect();
            }
        }
        out.push(Material {
            library: lib.slot,
            index,
            offset: index * 0x20,
            kind: w(0),
            list,
            width: w(8),
            height: w(0x0c),
            words: [w(0x10), w(0x14), w(0x18), w(0x1c)],
            list_word1,
            frames,
        });
    }
    out
}

/// The record a material shows at animation phase `phase`, as the polygon set-up routines
/// of modes 0, 1, 3, 4, 6, 7 pick it (e.g. ca8f4..ca946): frame = count * phase >> 14,
/// with the phase's low 16 bits only when it carries the flags 60000h. `phase` is the
/// object's [7bb94]: instance +20h >> 2, or the animation clock [1473a0] & 3FFFh for
/// instances with +20h < 0 (8246c).
///
/// # Safety
/// `base` must be a data block and `material` one of its directory entries.
pub unsafe fn material_texture(base: u32, material: u32, phase: u32) -> u32 {
    unsafe {
        let list = base.wrapping_add(mem::rd32(material + 4));
        let count = mem::rd32(list) as i32;
        let p = if phase & 0x60000 != 0 { phase & 0xffff } else { phase };
        let frame = count.wrapping_mul(p as i32) >> 14;
        mem::rd32(list.wrapping_add(8).wrapping_add((frame as u32).wrapping_mul(4)))
    }
}

/// The record a polygon of mode `mode` draws with (what the set-up routine stores in
/// [aa5c0]). Mode 5 (reflection, d59c0..d5a17): material 0 of the environment library
/// `env_base` when one is loaded (else the polygon's own material), frame = count *
/// `phase2` >> 16 with `phase2` = [7bb98] (instance +3Ch). Other textured modes: see
/// [`material_texture`].
///
/// # Safety
/// As [`material_texture`]; `env_base` 0 or a data block.
pub unsafe fn polygon_texture(mode: u32, base: u32, material: u32, phase: u32, env_base: u32, phase2: u32) -> u32 {
    if mode != 5 {
        return unsafe { material_texture(base, material, phase) };
    }
    unsafe {
        let (base, material) = if env_base & 0x0fff_ffff != 0 { (env_base, env_base) } else { (base, material) };
        let list = base.wrapping_add(mem::rd32(material + 4));
        let count = mem::rd32(list) as i32;
        let frame = count.wrapping_mul(phase2 as i32) >> 16;
        mem::rd32(list.wrapping_add(8).wrapping_add((frame as u32).wrapping_mul(4)))
    }
}

/// Texel formats the game's converter produces.
#[derive(Clone, Debug, PartialEq)]
pub enum TexFormat {
    /// GR_TEXFMT_P_8 with its palette (0x00RRGGBB).
    Indexed8 { palette: Box<[u32; 256]> },
    /// GR_TEXFMT_RGB_565 (little-endian u16 per texel).
    Rgb565,
    /// GR_TEXFMT_ARGB_4444 (little-endian u16 per texel).
    Argb4444,
}

/// One texture as the game holds it in RAM: the draft `TextureData` of the scene contract.
#[derive(Clone, Debug)]
pub struct Texture {
    pub width: u32,
    pub height: u32,
    pub format: TexFormat,
    /// Texels row by row (1 byte per texel for Indexed8, 2 otherwise).
    pub pixels: Vec<u8>,
    pub record: Record,
}

/// Read a record's texels (and palette). None: not in use, unknown format, unreadable.
pub fn texture(rec: &Record) -> Option<Texture> {
    if rec.used == 0 || rec.info.small_lod != rec.info.large_lod {
        return None;
    }
    let (width, height) = rec.size();
    let n = rec.texel_bytes();
    if !mem::readable(rec.info.data, n) {
        return None;
    }
    let format = match rec.info.format {
        GR_TEXFMT_P_8 => {
            if !mem::readable(rec.palette, 1024) {
                return None;
            }
            let mut p = Box::new([0u32; 256]);
            for (i, e) in p.iter_mut().enumerate() {
                *e = unsafe { mem::rd32(rec.palette + 4 * i as u32) };
            }
            TexFormat::Indexed8 { palette: p }
        }
        GR_TEXFMT_RGB_565 => TexFormat::Rgb565,
        GR_TEXFMT_ARGB_4444 => TexFormat::Argb4444,
        _ => return None,
    };
    let pixels = unsafe { mem::slice(rec.info.data, n) }.to_vec();
    Some(Texture { width, height, format, pixels, record: *rec })
}

fn rgba(r: u32, g: u32, b: u32, a: u32) -> u32 {
    r | g << 8 | b << 16 | a << 24
}
fn expand5(v: u32) -> u32 {
    (v << 3) | (v >> 2)
}
fn expand6(v: u32) -> u32 {
    (v << 2) | (v >> 4)
}

impl Texture {
    /// Texels as RGBA8 (`r | g<<8 | b<<16 | a<<24`), expanded as the Glide layer's TMU
    /// decoder does (5/6-bit channels by bit replication, 4-bit x 17, palette alpha 255).
    pub fn rgba(&self) -> Vec<u32> {
        let w16 = || self.pixels.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c) as u32);
        match &self.format {
            TexFormat::Indexed8 { palette } => self
                .pixels
                .iter()
                .map(|&i| {
                    let p = palette[i as usize];
                    rgba((p >> 16) & 0xff, (p >> 8) & 0xff, p & 0xff, 255)
                })
                .collect(),
            TexFormat::Rgb565 => w16().map(|v| rgba(expand5(v >> 11), expand6((v >> 5) & 63), expand5(v & 31), 255)).collect(),
            TexFormat::Argb4444 => w16().map(|v| rgba(((v >> 8) & 15) * 17, ((v >> 4) & 15) * 17, (v & 15) * 17, (v >> 12) * 17)).collect(),
        }
    }
}

fn fnv(words: &[u32]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &w in words {
        h = (h ^ w as u64).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Minimal PNG (RGBA8 with alpha, zlib "stored" blocks): sr-runtime cannot use sr-render's.
fn png(width: u32, height: u32, px: &[u32]) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }
    fn chunk(out: &mut Vec<u8>, ty: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(ty);
        out.extend_from_slice(data);
        let crc = crc32(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }
    let mut raw = Vec::with_capacity((width * 4 + 1) as usize * height as usize);
    for y in 0..height {
        raw.push(0);
        for &p in &px[(y * width) as usize..][..width as usize] {
            raw.extend_from_slice(&p.to_le_bytes());
        }
    }
    let (mut a, mut b) = (1u32, 0u32);
    for c in raw.chunks(5552) {
        for &x in c {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    let mut z = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65535).peekable();
    while let Some(blk) = blocks.next() {
        z.push(blocks.peek().is_none() as u8);
        z.extend_from_slice(&(blk.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(blk.len() as u16)).to_le_bytes());
        z.extend_from_slice(blk);
    }
    z.extend_from_slice(&(b << 16 | a).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn format_name(f: u32) -> &'static str {
    match f {
        GR_TEXFMT_P_8 => "P8",
        GR_TEXFMT_RGB_565 => "RGB565",
        GR_TEXFMT_ARGB_4444 => "ARGB4444",
        GR_TEXFMT_ARGB_1555 => "ARGB1555",
        _ => "other",
    }
}

// ---- SR_DUMP_TEXTURES ------------------------------------------------------------------

fn dump_dir() -> Option<&'static PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| std::env::var_os("SR_DUMP_TEXTURES").filter(|v| !v.is_empty()).map(PathBuf::from)).as_ref()
}

static CHECKING: AtomicBool = AtomicBool::new(false);

/// Texture binds and downloads are being checked (`SR_DUMP_TEXTURES`).
#[inline]
pub fn checking() -> bool {
    CHECKING.load(Ordering::Relaxed)
}

#[derive(Default)]
struct Check {
    race_frames: u64,
    /// Signature of the last dumped set of libraries and records, and dumps so far.
    dumped: Option<u64>,
    dumps: u32,
    binds: u64,
    sky_binds: u64,
    other_binds: u64,
    pixel_ok: u64,
    pixel_bad: u64,
    poly_ok: u64,
    poly_bad: u64,
    material_ok: u64,
    material_bad: u64,
    slot_ok: u64,
    slot_bad: u64,
    /// Binds right after the record's download: the manager writes the slot after
    /// `grTexSource`, so these are checked at the next bind.
    slot_pending: Vec<(u32, u32)>,
    last_download: Option<(u32, u32)>,
    /// Material path matches per polygon mode.
    mode_ok: [u64; 8],
    mode_bad: [u64; 8],
    downloads: u64,
    download_ok: u64,
    download_bad: u64,
    download_other: u64,
    multi_lod: u64,
    ids: HashSet<u32>,
    /// (id, data, palette) -> hash of the RAM texture's RGBA.
    ram: HashMap<(u32, u32, u32), u64>,
    logged: u32,
    /// grChromakeyValue from the manager's mode set-up = the polygon record's +14h.
    chroma_ok: u64,
    chroma_bad: u64,
    /// [14739c] is a library's data block and [7bb80] an entry of its directory.
    entry_ok: u64,
    entry_bad: u64,
    /// Materials with several frames: (data block, material) -> frames seen (bit mask).
    frames_seen: HashMap<(u32, u32), (u32, u128)>,
    /// Data blocks of the loaded libraries with their directory sizes.
    blocks: Vec<(u32, u32)>,
    /// Binds per (polygon mode, material type +00).
    mode_kind: HashMap<(u32, u32), u64>,
}

static CHECK: Mutex<Option<Check>> = Mutex::new(None);

pub fn install(_m: &mut Machine) {
    let _ = addrs();
    if let Some(dir) = dump_dir() {
        *CHECK.lock().unwrap() = Some(Check::default());
        CHECKING.store(true, Ordering::Relaxed);
        crate::info!("textures: dump to {} at the first frame of each race; checking every texture bind and download", dir.display());
    }
}

/// What is loaded: the libraries and the records (without the manager's slot, +4).
fn signature() -> u64 {
    let a = addrs();
    let mut words = Vec::with_capacity(4096);
    for lib in libraries() {
        words.extend([lib.slot, lib.data]);
        words.extend(lib.name.bytes().map(u32::from));
    }
    for id in 0..RECORD_COUNT {
        let p = a.records + id * RECORD_SIZE;
        if unsafe { mem::rd32(p) } != 0 {
            words.push(id);
            words.extend((2..RECORD_SIZE / 4).map(|k| unsafe { mem::rd32(p + 4 * k) }));
        }
    }
    fnv(&words)
}

/// Start of a race frame: the first frame of each race whose libraries differ from the
/// last dumped ones (the attract-mode race, then the player's) dumps the loaded textures
/// to `<dir>/race<n>_<first library>`.
pub fn race_frame(m: &mut Machine) {
    let Some(dir) = dump_dir() else { return };
    let sig = signature();
    let (dump_no, frame) = {
        let mut g = CHECK.lock().unwrap();
        let Some(c) = g.as_mut() else { return };
        c.race_frames += 1;
        let new = c.dumped != Some(sig);
        if new {
            c.dumped = Some(sig);
            c.dumps += 1;
            c.blocks.clear();
        }
        (new.then_some(c.dumps), c.race_frames)
    };
    if let Some(n) = dump_no {
        let track = libraries().last().map_or("race".to_string(), |l| l.name.clone());
        let sub = dir.join(format!("race{n}_{track}"));
        if let Err(e) = dump(m, &sub, frame) {
            crate::warn!("textures: dump to {} failed: {e}", sub.display());
        }
    }
    if frame % 250 == 0 {
        log_summary();
    }
}

fn log_summary() {
    let g = CHECK.lock().unwrap();
    let Some(c) = g.as_ref() else { return };
    crate::info!(
        "textures: check after {} race frames: {} binds ({} distinct records, {} background, {} other): texels as in RAM {} / differ {}; \
         polygon id [aa5c0] = bound record {} / {}; material frame list -> [aa5c0] {} / {}; manager slot -> TMU address {} / {}; \
         {} downloads ({} not records): TMU bytes = RAM {} / differ {}; {} multi-LOD binds; material path by mode (ok/differ) {}",
        c.race_frames,
        c.binds,
        c.ids.len(),
        c.sky_binds,
        c.other_binds,
        c.pixel_ok,
        c.pixel_bad,
        c.poly_ok,
        c.poly_bad,
        c.material_ok,
        c.material_bad,
        c.slot_ok,
        c.slot_bad,
        c.downloads,
        c.download_other,
        c.download_ok,
        c.download_bad,
        c.multi_lod,
        (0..8).filter(|&m| c.mode_ok[m] + c.mode_bad[m] > 0).map(|m| format!("{m}:{}/{}", c.mode_ok[m], c.mode_bad[m])).collect::<Vec<_>>().join(" ")
    );
    // animated materials: short lists one by one, the 128-entry lists (cars) as one union
    let frames = |seen: u128| (0..128u32).filter(|f| seen >> f & 1 != 0).map(|f| f.to_string()).collect::<Vec<_>>().join(",");
    let mut anim: Vec<String> = c
        .frames_seen
        .iter()
        .filter(|(_, (count, _))| *count < 128)
        .map(|(&(b, off), &(count, seen))| format!("{b:#x}+{off:#x} {}/{count} frames ({})", seen.count_ones(), frames(seen)))
        .collect();
    anim.sort();
    let long: Vec<&(u32, u128)> = c.frames_seen.values().filter(|(count, _)| *count >= 128).collect();
    anim.push(format!("{} 128-entry lists, entries used {}", long.len(), frames(long.iter().fold(0, |u, (_, s)| u | s))));
    let mut mk: Vec<(&(u32, u32), &u64)> = c.mode_kind.iter().collect();
    mk.sort();
    let mk: Vec<String> = mk.iter().map(|((m, k), n)| format!("{m}/{k:#x}:{n}")).collect();
    crate::info!(
        "textures: check: chroma value = record +14h {} / {}; [14739c] a library block and [7bb80] one of its entries {} / {}; \
         binds by polygon mode / material type [{}]; animated materials bound: {}",
        c.chroma_ok,
        c.chroma_bad,
        c.entry_ok,
        c.entry_bad,
        mk.join(" "),
        anim.join("; ")
    );
}

fn record_of(info_ptr: u32) -> Option<u32> {
    let off = info_ptr.wrapping_sub(addrs().records + 0x18);
    (off.is_multiple_of(RECORD_SIZE) && off / RECORD_SIZE < RECORD_COUNT).then_some(off / RECORD_SIZE)
}

/// After `grTexSource(tmu, start, even_odd, info)`: the texels the game binds (TMU content
/// decoded with the palette in effect, as the renderer draws them) against the record read
/// from RAM; the polygon's texture id and its material path; the manager's slot.
pub fn check_bind(g: &Glide, caller: u32, start: u32, even_odd: u32, info_ptr: u32) {
    let a = addrs();
    let mut lock = CHECK.lock().unwrap();
    let Some(c) = lock.as_mut() else { return };
    c.binds += 1;
    if info_ptr == a.sky_info || caller == SKY_BIND {
        c.sky_binds += 1;
        return;
    }
    let Some(id) = record_of(info_ptr) else {
        c.other_binds += 1;
        return;
    };
    c.ids.insert(id);
    let rec = unsafe { Record::read(id) };
    if rec.info.small_lod != rec.info.large_lod {
        c.multi_lod += 1;
    }
    // the manager's slot of the record holds this TMU address and this record (written
    // after grTexSource when the record was just downloaded: checked at the next bind)
    let slot_holds = |id: u32, start: u32| {
        let slot = unsafe { Record::read(id) }.cache_slot;
        (0..CACHE_SLOT_COUNT as i32).contains(&slot)
            && unsafe { mem::rd32(a.cache + 8 * slot as u32) == start && mem::rd32(a.cache + 8 * slot as u32 + 4) == id }
    };
    for (pid, pstart) in std::mem::take(&mut c.slot_pending) {
        if slot_holds(pid, pstart) {
            c.slot_ok += 1;
        } else {
            c.slot_bad += 1;
        }
    }
    if c.last_download.take() == Some((id, start)) {
        c.slot_pending.push((id, start));
    } else if slot_holds(id, start) {
        c.slot_ok += 1;
    } else {
        c.slot_bad += 1;
    }
    if MANAGER_BINDS.contains(&caller) {
        let poly = unsafe { mem::rd32(a.poly_texture) };
        if poly == id {
            c.poly_ok += 1;
        } else {
            c.poly_bad += 1;
        }
        let (base, mat, phase, mode, env, phase2) = unsafe {
            (mem::rd32(a.data_base), mem::rd32(a.material), mem::rd32(a.phase), mem::rd8(a.poly_mode) as u32, mem::rd32(a.env_base), mem::rd32(a.phase2))
        };
        let got = mem::readable(mat, 8).then(|| unsafe { polygon_texture(mode, base, mat, phase, env, phase2) });
        // the polygon's material is an entry of the model's library directory
        if !c.blocks.iter().any(|&(b, _)| b == base) {
            c.blocks = libraries().iter().map(|l| (l.data, materials(l).len() as u32)).collect();
        }
        let in_dir = c.blocks.iter().any(|&(b, n)| b == base && mat >= b && (mat - b) % 0x20 == 0 && (mat - b) / 0x20 < n);
        if in_dir {
            c.entry_ok += 1;
            *c.mode_kind.entry((mode, unsafe { mem::rd32(mat) })).or_default() += 1;
        } else {
            c.entry_bad += 1;
        }
        // animated materials (mode 5's frame comes from the environment library instead)
        if mode != 5 && mem::readable(mat, 8) {
            let list = base.wrapping_add(unsafe { mem::rd32(mat + 4) });
            let count = unsafe { mem::rd32(list) };
            if count > 1 && count <= 128 {
                let p = if phase & 0x60000 != 0 { phase & 0xffff } else { phase };
                let frame = ((count as i32).wrapping_mul(p as i32) >> 14) as u32;
                let e = c.frames_seen.entry((base, mat - base)).or_insert((count, 0));
                if frame < 128 {
                    e.1 |= 1u128 << frame;
                }
            }
        }
        let m = (mode & 7) as usize;
        if got == Some(poly) {
            c.material_ok += 1;
            c.mode_ok[m] += 1;
        } else {
            c.material_bad += 1;
            c.mode_bad[m] += 1;
            if c.logged < 10 {
                c.logged += 1;
                crate::warn!(
                    "textures: material path: mode {mode} base {base:#x} material {mat:#x} phase {phase:#x} env {env:#x} phase2 {phase2:#x} -> {got:?}, game {poly}, bound {id}"
                );
            }
        }
    }
    let key = (id, rec.info.data, rec.palette);
    let want = *c.ram.entry(key).or_insert_with(|| texture(&rec).map_or(0, |t| fnv(&t.rgba())));
    let (_, _, got) = tex::decode_level(&g.tmu, start, &rec.info, even_odd, rec.info.large_lod, &g.palette, &g.ncc, 0);
    if fnv(&got) == want {
        c.pixel_ok += 1;
    } else {
        c.pixel_bad += 1;
        if c.logged < 10 {
            c.logged += 1;
            let ram = texture(&rec).map(|t| t.rgba()).unwrap_or_default();
            let differ = ram.iter().zip(&got).filter(|(x, y)| x != y).count();
            crate::warn!(
                "textures: bind of record {id} at TMU {start:#x} ({:?}): {differ} of {} texels differ from RAM (caller {caller:#x})",
                rec.info,
                got.len()
            );
        }
    }
}

/// `grChromakeyValue(value)` from the texture manager's mode set-up (cc388..cc5e8, modes 1
/// and 3): the value against the polygon's record +14h.
pub fn check_chroma(caller: u32, value: u32) {
    if !(0xcc388..0xcc5e8).contains(&caller) {
        return;
    }
    let mut lock = CHECK.lock().unwrap();
    let Some(c) = lock.as_mut() else { return };
    let id = unsafe { mem::rd32(addrs().poly_texture) };
    if id < RECORD_COUNT && unsafe { Record::read(id) }.chroma == value {
        c.chroma_ok += 1;
    } else {
        c.chroma_bad += 1;
    }
}

/// After `grTexDownloadMipMap(tmu, start, even_odd, info)`: the bytes now in texture
/// memory against the record's texels in RAM.
pub fn check_download(g: &Glide, start: u32, info_ptr: u32) {
    let mut lock = CHECK.lock().unwrap();
    let Some(c) = lock.as_mut() else { return };
    c.downloads += 1;
    let Some(id) = record_of(info_ptr) else {
        c.download_other += 1;
        let info = unsafe { TexInfo::read(info_ptr) };
        let link = crate::machine().img.to_link(info_ptr);
        crate::info!("textures: download of a non-record texture: info {info_ptr:#x} (link {link:x?}) {info:?} to TMU {start:#x}");
        return;
    };
    c.last_download = Some((id, start));
    let rec = unsafe { Record::read(id) };
    let n = rec.texel_bytes();
    let tmu = g.tmu.ram.get(start as usize..(start + n) as usize);
    let ok = mem::readable(rec.info.data, n) && tmu.is_some_and(|t| t == unsafe { mem::slice(rec.info.data, n) });
    if ok {
        c.download_ok += 1;
    } else {
        c.download_bad += 1;
    }
}

/// Write every texture of the loaded libraries to `dir`.
fn dump(m: &Machine, dir: &Path, race_frame: u64) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let a = addrs();
    let libs = libraries();
    let recs = records();
    let scale = f32::from_bits(unsafe { mem::rd32(a.scale) });

    // materials and which of them name each record
    let mut refs: HashMap<u32, Vec<String>> = HashMap::new();
    let mut kind_of: HashMap<u32, u32> = HashMap::new();
    let mut mats_tsv = String::from("library\tindex\toffset\ttype\tlist\tframes\tlist_word1\twidth\theight\tw10\tw14\tw18\tw1c\trecords\n");
    let (mut n_mats, mut n_textured, mut n_animated, mut size_mismatch) = (0, 0, 0, 0);
    for lib in &libs {
        for mat in materials(lib) {
            n_mats += 1;
            if !mat.frames.is_empty() {
                n_textured += 1;
            }
            if mat.frames.len() > 1 {
                n_animated += 1;
            }
            for (f, &id) in mat.frames.iter().enumerate() {
                let tag = if mat.frames.len() > 1 { format!("{}:{}#{}", lib.slot, mat.index, f) } else { format!("{}:{}", lib.slot, mat.index) };
                refs.entry(id).or_default().push(tag);
                kind_of.entry(id).or_insert(mat.kind);
                if recs.iter().find(|r| r.id == id).is_some_and(|r| r.size() != (mat.width, mat.height)) {
                    size_mismatch += 1;
                }
            }
            let ids: Vec<String> = mat.frames.iter().map(|i| i.to_string()).collect();
            let _ = writeln!(
                mats_tsv,
                "{}\t{}\t{:#x}\t{:#x}\t{:#x}\t{}\t{:#x}\t{}\t{}\t{:#x}\t{:#x}\t{:#x}\t{:#x}\t{}",
                lib.slot,
                mat.index,
                mat.offset,
                mat.kind,
                mat.list,
                mat.frames.len(),
                mat.list_word1,
                mat.width,
                mat.height,
                mat.words[0],
                mat.words[1],
                mat.words[2],
                mat.words[3],
                ids.join(",")
            );
        }
    }

    // textures
    let mut index = String::new();
    let _ = writeln!(index, "# SR_DUMP_TEXTURES at race frame {race_frame} (Glide frame {}); texture scale [12374c] = {scale}", m.glide.as_ref().map_or(0, |g| g.frames));
    let env = unsafe { mem::rd32(a.env_base) };
    for lib in &libs {
        let tag = if lib.data == env { " (environment map library [aa6a8]: mode-5 polygons draw its material 0)" } else { "" };
        let _ = writeln!(index, "# library {}: '{}' / '{}', data block {:#x}{tag}", lib.slot, lib.name, lib.name2, lib.data);
    }
    let _ = writeln!(
        index,
        "id\tlibrary\tmaterials\ttype\twidth\theight\tformat\tglide_fmt\tlod\taspect\ttmu_bytes\tdata\tlib_offset\tpalette\tpalette_no\tchroma\tkeyed_texels\tgrey\ts_scale\tt_scale\tcache_slot\trgba_fnv\tfile"
    );
    // P8 palettes: entries as the converter writes them (8-bit channels = 5/6-bit values
    // shifted, low bits 0) and entry 0 = the chroma colour (type 40h)
    let (mut pal_shifted, mut pal_entry0_chroma, mut p8_keyed_type) = (0, 0, 0);
    // RGB565 / ARGB4444 records whose texel 0-index colour equals the chroma colour
    let mut by_kind: HashMap<(u32, &str), u32> = HashMap::new();
    let mut palettes: Vec<Box<[u32; 256]>> = Vec::new();
    let mut pal_no: HashMap<u64, usize> = HashMap::new();
    let mut by_format: HashMap<&str, (u32, u64)> = HashMap::new();
    let mut by_size: HashMap<(u32, u32), u32> = HashMap::new();
    let (mut written, mut skipped, mut keyed, mut unreferenced, mut pal_after, mut scale_bad, mut multi) = (0, 0, 0, 0, 0, 0, 0);
    for rec in &recs {
        if rec.info.small_lod != rec.info.large_lod {
            multi += 1;
        }
        let Some(t) = texture(rec) else {
            skipped += 1;
            crate::warn!("textures: record {} not exported: {:?}", rec.id, rec);
            continue;
        };
        let px = t.rgba();
        let file = format!("tex_{:04}.png", rec.id);
        std::fs::write(dir.join(&file), png(t.width, t.height, &px))?;
        written += 1;
        let (w, h) = (t.width, t.height);
        let e = by_format.entry(format_name(rec.info.format)).or_default();
        e.0 += 1;
        e.1 += rec.tmu_bytes as u64;
        *by_size.entry((w, h)).or_default() += 1;
        // the chroma colour 0x00RRGGBB as a decoded texel (r | g << 8 | b << 16)
        let key = rgba((rec.chroma >> 16) & 0xff, (rec.chroma >> 8) & 0xff, rec.chroma & 0xff, 0);
        let keyed_texels = px.iter().filter(|&&p| p & 0xff_ffff == key).count();
        if keyed_texels > 0 {
            keyed += 1;
        }
        let kind = kind_of.get(&rec.id).copied().unwrap_or(0);
        *by_kind.entry((kind, format_name(rec.info.format))).or_default() += 1;
        let (pal, pno) = match &t.format {
            TexFormat::Indexed8 { palette } => {
                if rec.palette == rec.info.data + w * h {
                    pal_after += 1;
                }
                if palette[1..].iter().all(|&c| c & 0x0007_0307 == 0) {
                    pal_shifted += 1;
                }
                if kind == 0x40 {
                    p8_keyed_type += 1;
                    if palette[0] == rec.chroma {
                        pal_entry0_chroma += 1;
                    }
                }
                let hsh = fnv(&palette[..]);
                let n = *pal_no.entry(hsh).or_insert_with(|| {
                    palettes.push(palette.clone());
                    palettes.len() - 1
                });
                (format!("{:#x}", rec.palette), n.to_string())
            }
            _ => ("-".into(), "-".into()),
        };
        let max = w.max(h) as f32;
        if (rec.s_scale - w as f32 / max).abs() > 1e-6 || (rec.t_scale - h as f32 / max).abs() > 1e-6 {
            scale_bad += 1;
        }
        let mats = refs.get(&rec.id).map(|v| v.join(",")).unwrap_or_else(|| {
            unreferenced += 1;
            "-".into()
        });
        let lib_off = libs.iter().find(|l| l.slot == rec.library).map_or("-".into(), |l| format!("{:#x}", rec.info.data.wrapping_sub(l.data)));
        let _ = writeln!(
            index,
            "{}\t{}\t{}\t{:#x}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:#x}\t{}\t{}\t{}\t{:06x}\t{}\t{}\t{}\t{}\t{}\t{:016x}\t{}",
            rec.id,
            rec.library,
            mats,
            kind,
            w,
            h,
            format_name(rec.info.format),
            rec.info.format,
            rec.info.large_lod,
            rec.info.aspect,
            rec.tmu_bytes,
            rec.info.data,
            lib_off,
            pal,
            pno,
            rec.chroma,
            keyed_texels,
            rec.grey,
            rec.s_scale,
            rec.t_scale,
            rec.cache_slot,
            fnv(&px),
            file
        );
    }
    for (n, p) in palettes.iter().enumerate() {
        let px: Vec<u32> = p.iter().map(|&c| rgba((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff, 255)).collect();
        std::fs::write(dir.join(format!("pal_{n:03}.png")), png(16, 16, &px))?;
    }

    // background picture: from the emulated texture memory (its RAM copy is freed)
    let sky_info = unsafe { TexInfo::read(a.sky_info) };
    let sky_start = unsafe { mem::rd32(a.sky_tmu) };
    let sky_ram = unsafe { mem::rd32(a.sky_pixels) };
    let mut sky_line = String::from("none");
    if let Some(g) = m.glide.as_ref()
        && sky_info.format == GR_TEXFMT_RGB_565
    {
        let (w, h, px) = tex::decode_level(&g.tmu, sky_start, &sky_info, 3, sky_info.large_lod, &g.palette, &g.ncc, 0);
        std::fs::write(dir.join("sky.png"), png(w, h, &px))?;
        sky_line = format!("sky.png {w}x{h} {} from TMU {sky_start:#x} (RAM pointer [14734c] = {sky_ram:#x}), rgba_fnv {:016x}", format_name(sky_info.format), fnv(&px));
    }
    let _ = writeln!(index, "# background: {sky_line}");

    // the chroma colour tables against the TMU decoder's expansion
    let (t5, t6): (Vec<u32>, Vec<u32>) =
        unsafe { ((0..32).map(|i| mem::rd32(a.chroma5 + 4 * i)).collect(), (0..64).map(|i| mem::rd32(a.chroma6 + 4 * i)).collect()) };
    let chroma_expand = t5.iter().enumerate().all(|(i, &v)| v == expand5(i as u32)) && t6.iter().enumerate().all(|(i, &v)| v == expand6(i as u32));
    let _ = writeln!(index, "# chroma tables 12378c (5 bit): {t5:?}");
    let _ = writeln!(index, "# chroma tables 12380c (6 bit): {t6:?}");

    std::fs::write(dir.join("index.tsv"), index)?;
    std::fs::write(dir.join("materials.tsv"), mats_tsv)?;

    let mut fmts: Vec<String> = by_format.iter().map(|(k, (n, b))| format!("{k} {n} ({} KB)", b / 1024)).collect();
    fmts.sort();
    let mut sizes: Vec<((u32, u32), u32)> = by_size.into_iter().collect();
    sizes.sort();
    let sizes: Vec<String> = sizes.iter().map(|((w, h), n)| format!("{w}x{h}:{n}")).collect();
    let names: Vec<String> = libs.iter().map(|l| format!("{} '{}'", l.slot, l.name)).collect();
    crate::info!(
        "textures: dumped {written} records to {} ({skipped} skipped, {multi} with several LODs); libraries [{}]; formats [{}]; sizes [{}]; \
         {} palettes ({pal_after} right after their texels); {keyed} with texels of their chroma colour; {unreferenced} named by no material; \
         s/t scale = size / larger side: {} of {written}; {n_mats} materials, {n_textured} textured, {n_animated} animated, \
         {size_mismatch} frames whose record size differs from the material's; chroma tables = TMU expansion: {chroma_expand}; background: {sky_line}",
        dir.display(),
        names.join(", "),
        fmts.join(", "),
        sizes.join(" "),
        palettes.len(),
        written - scale_bad,
    );
    let mut kinds: Vec<String> = by_kind.iter().map(|((k, f), n)| format!("{k:#x} {f}: {n}")).collect();
    kinds.sort();
    crate::info!(
        "textures: by material type and format [{}]; P8 palettes with 5/6-bit channels shifted (entries 1-255): {pal_shifted}; \
         type-40h P8 textures {p8_keyed_type}, palette entry 0 = chroma colour: {pal_entry0_chroma}",
        kinds.join(", ")
    );
    Ok(())
}
