//! The shader library pack (`crt-shaders.pak`): ShaderGlass's precompiled RetroArch library
//! (presets, DXBC shaders, lookup textures) in one file next to the program.
//!
//! The pack is generated from a ShaderGlass checkout (`import`, `srtool crt-pack`); it is
//! never committed. The game reads the small index at start (preset names and categories)
//! and one preset's records and blobs when it is chosen: the library itself (about 100 MB)
//! never sits in the 32-bit process's memory.
//!
//! Layout (little-endian):
//!
//! ```text
//! header   "SRCRTPAK", u32 version, u32 index offset, u32 index length, u32 0
//! blobs    shader bytecode, PNG files, preset and shader records (each written once)
//! index    str source, u32 n + str categories, u32 n + presets (str name, u16 category,
//!          u32 record offset, u32 record length), u32 n + community favourites (u32
//!          preset), u32 default preset (u32::MAX: none)
//! preset   u8 n + passes (u32 shader record offset, u32 length, u16 n + str key, str value),
//!          u8 n + textures (str name, u32 PNG offset, u32 length, u16 n + key/value),
//!          u16 n + overrides (str name, f32 value)
//! shader   str name, str format, u32 vertex offset, length, u32 fragment offset, length,
//!          u16 n + params (str name, i8 buffer, u16 offset, u16 size, f32 min, max,
//!          default, step, str description), u8 n + samplers (str name, u8 binding)
//! str      u16 length, UTF-8 bytes
//! ```

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const MAGIC: &[u8; 8] = b"SRCRTPAK";
pub const VERSION: u32 = 1;
/// The pack's file name, next to the program.
pub const FILE_NAME: &str = "crt-shaders.pak";

/// A shader parameter (ShaderGlass `ShaderParam`): `buffer` -1 is the push constants
/// (register b1), 0 the UBO (register b0).
#[derive(Clone, Debug, PartialEq)]
pub struct ParamDef {
    pub name: String,
    pub buffer: i32,
    pub offset: u32,
    pub size: u32,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub step: f32,
    pub description: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShaderData {
    pub name: String,
    /// `#pragma format` of the shader ("" = the default).
    pub format: String,
    pub vs: Vec<u8>,
    pub ps: Vec<u8>,
    pub params: Vec<ParamDef>,
    /// Texture name and register (`AddSampler`).
    pub samplers: Vec<(String, u32)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PassData {
    pub shader: ShaderData,
    /// The preset's options for this pass (`filter_linear`, `scale_type`, `alias`, ...).
    pub options: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextureData {
    /// The name the shaders sample it by.
    pub name: String,
    pub png: Vec<u8>,
    /// `linear`, `mipmap`, `wrap_mode`.
    pub options: Vec<(String, String)>,
}

/// One preset, as loaded for the runtime (or without its blobs, for the menu).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PresetData {
    pub name: String,
    pub category: String,
    pub passes: Vec<PassData>,
    pub textures: Vec<TextureData>,
    pub overrides: Vec<(String, f32)>,
}

pub fn option<'a>(options: &'a [(String, String)], key: &str) -> Option<&'a str> {
    options.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// A parameter the player can change (ShaderGlass's parameters window: one per name over
/// all passes, floats with a range).
#[derive(Clone, Debug, PartialEq)]
pub struct UserParam {
    pub name: String,
    pub description: String,
    pub min: f32,
    pub max: f32,
    pub step: f32,
    /// The shader's default, or the preset's override of it.
    pub default: f32,
}

impl PresetData {
    /// The parameters of the preset in pass order, once per name (ShaderGlass `Params()`
    /// and its window: 4-byte values except `FrameCount`, ranges that are not empty).
    pub fn user_params(&self) -> Vec<UserParam> {
        let mut out: Vec<UserParam> = Vec::new();
        for pass in &self.passes {
            for p in &pass.shader.params {
                if p.size != 4 || p.name == "FrameCount" || p.max == p.min || out.iter().any(|u| u.name == p.name) {
                    continue;
                }
                let default = self.overrides.iter().find(|(n, _)| *n == p.name).map_or(p.default, |o| o.1);
                out.push(UserParam { name: p.name.clone(), description: p.description.clone(), min: p.min, max: p.max, step: p.step, default });
            }
        }
        out
    }
}

#[derive(Clone, Debug)]
pub struct PresetEntry {
    pub name: String,
    pub category: u16,
    record: (u32, u32),
}

/// The pack's index: what the menu lists.
#[derive(Clone, Debug, Default)]
pub struct PackIndex {
    /// Where the library came from (ShaderGlass version and commit).
    pub source: String,
    pub categories: Vec<String>,
    pub presets: Vec<PresetEntry>,
    /// ShaderGlass's "Community Favorites" (`Options.h`).
    pub community: Vec<u32>,
    /// ShaderGlass's default preset.
    pub default_preset: Option<u32>,
}

impl PackIndex {
    pub fn category(&self, preset: usize) -> &str {
        self.presets.get(preset).and_then(|p| self.categories.get(p.category as usize)).map_or("", |s| s.as_str())
    }

    /// "category/name": the key a preset is saved under (names repeat across categories).
    pub fn key(&self, preset: usize) -> String {
        match self.presets.get(preset) {
            Some(p) => format!("{}/{}", self.category(preset), p.name),
            None => String::new(),
        }
    }

    /// The preset saved as `key` ("category/name", or a bare name: the first with it).
    pub fn find(&self, key: &str) -> Option<usize> {
        let key = key.trim();
        if key.is_empty() {
            return None;
        }
        if let Some(i) = (0..self.presets.len()).find(|&i| self.key(i) == key) {
            return Some(i);
        }
        self.presets.iter().position(|p| p.name == key)
    }
}

/// An open pack: the index in memory, records and blobs read when asked for.
pub struct Pack {
    pub path: PathBuf,
    pub index: PackIndex,
    file: Mutex<std::fs::File>,
}

fn bad(what: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, format!("crt pack: {what}"))
}

/// Little-endian reader over a byte slice.
struct Cursor<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> std::io::Result<&'a [u8]> {
        let s = self.b.get(self.at..self.at + n).ok_or_else(|| bad("truncated record"))?;
        self.at += n;
        Ok(s)
    }
    fn u8(&mut self) -> std::io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> std::io::Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> std::io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> std::io::Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn str(&mut self) -> std::io::Result<String> {
        let n = self.u16()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn kv(&mut self) -> std::io::Result<Vec<(String, String)>> {
        let n = self.u16()?;
        (0..n).map(|_| Ok((self.str()?, self.str()?))).collect()
    }
}

impl Pack {
    pub fn open(path: &Path) -> std::io::Result<Pack> {
        let mut f = std::fs::File::open(path)?;
        let mut head = [0u8; 24];
        f.read_exact(&mut head)?;
        if &head[0..8] != MAGIC {
            return Err(bad("not a pack file"));
        }
        let version = u32::from_le_bytes(head[8..12].try_into().unwrap());
        if version != VERSION {
            return Err(bad(&format!("version {version}, this program reads {VERSION}: generate the pack again")));
        }
        let (off, len) = (u32::from_le_bytes(head[12..16].try_into().unwrap()), u32::from_le_bytes(head[16..20].try_into().unwrap()));
        let mut buf = vec![0u8; len as usize];
        f.seek(SeekFrom::Start(off as u64))?;
        f.read_exact(&mut buf)?;
        let mut c = Cursor { b: &buf, at: 0 };
        let source = c.str()?;
        let n = c.u32()?;
        let categories = (0..n).map(|_| c.str()).collect::<std::io::Result<Vec<_>>>()?;
        let n = c.u32()?;
        let mut presets = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let name = c.str()?;
            let category = c.u16()?;
            let record = (c.u32()?, c.u32()?);
            presets.push(PresetEntry { name, category, record });
        }
        let n = c.u32()?;
        let community = (0..n).map(|_| c.u32()).collect::<std::io::Result<Vec<_>>>()?;
        let d = c.u32()?;
        let index = PackIndex { source, categories, presets, community, default_preset: (d != u32::MAX).then_some(d) };
        Ok(Pack { path: path.to_path_buf(), index, file: Mutex::new(f) })
    }

    fn read(&self, (off, len): (u32, u32)) -> std::io::Result<Vec<u8>> {
        let mut f = self.file.lock().unwrap();
        f.seek(SeekFrom::Start(off as u64))?;
        let mut v = vec![0u8; len as usize];
        f.read_exact(&mut v)?;
        Ok(v)
    }

    /// Preset `i` with its shaders' bytecode and its textures (`blobs`), or only the
    /// metadata (names, parameters, pass options) for the menu.
    pub fn load(&self, i: usize, blobs: bool) -> std::io::Result<PresetData> {
        let e = self.index.presets.get(i).ok_or_else(|| bad("no such preset"))?;
        let rec = self.read(e.record)?;
        let mut c = Cursor { b: &rec, at: 0 };
        let mut p = PresetData { name: e.name.clone(), category: self.index.category(i).to_string(), ..Default::default() };
        let n = c.u8()?;
        for _ in 0..n {
            let srec = (c.u32()?, c.u32()?);
            let options = c.kv()?;
            let shader = self.load_shader(srec, blobs)?;
            p.passes.push(PassData { shader, options });
        }
        let n = c.u8()?;
        for _ in 0..n {
            let name = c.str()?;
            let blob = (c.u32()?, c.u32()?);
            let options = c.kv()?;
            let png = if blobs { self.read(blob)? } else { Vec::new() };
            p.textures.push(TextureData { name, png, options });
        }
        let n = c.u16()?;
        for _ in 0..n {
            p.overrides.push((c.str()?, c.f32()?));
        }
        Ok(p)
    }

    fn load_shader(&self, rec: (u32, u32), blobs: bool) -> std::io::Result<ShaderData> {
        let b = self.read(rec)?;
        let mut c = Cursor { b: &b, at: 0 };
        let name = c.str()?;
        let format = c.str()?;
        let vsb = (c.u32()?, c.u32()?);
        let psb = (c.u32()?, c.u32()?);
        let n = c.u16()?;
        let mut params = Vec::with_capacity(n as usize);
        for _ in 0..n {
            params.push(ParamDef {
                name: c.str()?,
                buffer: c.u8()? as i8 as i32,
                offset: c.u16()? as u32,
                size: c.u16()? as u32,
                min: c.f32()?,
                max: c.f32()?,
                default: c.f32()?,
                step: c.f32()?,
                description: c.str()?,
            });
        }
        let n = c.u8()?;
        let samplers = (0..n).map(|_| Ok((c.str()?, c.u8()? as u32))).collect::<std::io::Result<Vec<_>>>()?;
        let (vs, ps) = if blobs { (self.read(vsb)?, self.read(psb)?) } else { (Vec::new(), Vec::new()) };
        Ok(ShaderData { name, format, vs, ps, params, samplers })
    }
}

// ---- writing ----

/// Builds a pack: blobs are appended as they come (shared shaders and textures once, by
/// the caller's key), the index at the end.
pub struct PackWriter<W: Write + Seek> {
    out: W,
    at: u32,
    shaders: std::collections::HashMap<String, (u32, u32)>,
    textures: std::collections::HashMap<String, (u32, u32)>,
    categories: Vec<String>,
    presets: Vec<(String, u16, (u32, u32))>,
    pub bytes_shaders: u64,
    pub bytes_textures: u64,
}

fn put_str(b: &mut Vec<u8>, s: &str) {
    let s = &s.as_bytes()[..s.len().min(u16::MAX as usize)];
    b.extend_from_slice(&(s.len() as u16).to_le_bytes());
    b.extend_from_slice(s);
}

fn put_kv(b: &mut Vec<u8>, kv: &[(String, String)]) {
    b.extend_from_slice(&(kv.len() as u16).to_le_bytes());
    for (k, v) in kv {
        put_str(b, k);
        put_str(b, v);
    }
}

fn put_blob(b: &mut Vec<u8>, (off, len): (u32, u32)) {
    b.extend_from_slice(&off.to_le_bytes());
    b.extend_from_slice(&len.to_le_bytes());
}

impl<W: Write + Seek> PackWriter<W> {
    pub fn new(mut out: W) -> std::io::Result<Self> {
        out.write_all(&[0u8; 24])?;
        Ok(PackWriter {
            out,
            at: 24,
            shaders: Default::default(),
            textures: Default::default(),
            categories: Vec::new(),
            presets: Vec::new(),
            bytes_shaders: 0,
            bytes_textures: 0,
        })
    }

    fn blob(&mut self, data: &[u8]) -> std::io::Result<(u32, u32)> {
        let off = self.at;
        self.out.write_all(data)?;
        self.at = self.at.checked_add(data.len() as u32).ok_or_else(|| bad("pack larger than 4 GB"))?;
        Ok((off, data.len() as u32))
    }

    /// The record of the shader written as `key`, if it was.
    pub fn written_shader(&self, key: &str) -> Option<(u32, u32)> {
        self.shaders.get(key).copied()
    }

    /// A shader record (with its bytecode), once per `key`.
    pub fn shader(&mut self, key: &str, s: &ShaderData) -> std::io::Result<(u32, u32)> {
        if let Some(&r) = self.shaders.get(key) {
            return Ok(r);
        }
        let vs = self.blob(&s.vs)?;
        let ps = self.blob(&s.ps)?;
        self.bytes_shaders += (s.vs.len() + s.ps.len()) as u64;
        let mut b = Vec::new();
        put_str(&mut b, &s.name);
        put_str(&mut b, &s.format);
        put_blob(&mut b, vs);
        put_blob(&mut b, ps);
        b.extend_from_slice(&(s.params.len() as u16).to_le_bytes());
        for p in &s.params {
            put_str(&mut b, &p.name);
            b.push(p.buffer as i8 as u8);
            b.extend_from_slice(&(p.offset as u16).to_le_bytes());
            b.extend_from_slice(&(p.size as u16).to_le_bytes());
            for v in [p.min, p.max, p.default, p.step] {
                b.extend_from_slice(&v.to_le_bytes());
            }
            put_str(&mut b, &p.description);
        }
        b.push(s.samplers.len() as u8);
        for (n, binding) in &s.samplers {
            put_str(&mut b, n);
            b.push(*binding as u8);
        }
        let r = self.blob(&b)?;
        self.shaders.insert(key.to_string(), r);
        Ok(r)
    }

    /// A texture's PNG bytes, once per `key`.
    pub fn texture(&mut self, key: &str, png: &[u8]) -> std::io::Result<(u32, u32)> {
        if let Some(&r) = self.textures.get(key) {
            return Ok(r);
        }
        let r = self.blob(png)?;
        self.bytes_textures += png.len() as u64;
        self.textures.insert(key.to_string(), r);
        Ok(r)
    }

    /// A preset: its passes (shader records from `shader`), textures (blobs from
    /// `texture`) and parameter overrides.
    pub fn preset(
        &mut self,
        name: &str,
        category: &str,
        passes: &[((u32, u32), Vec<(String, String)>)],
        textures: &[(String, (u32, u32), Vec<(String, String)>)],
        overrides: &[(String, f32)],
    ) -> std::io::Result<()> {
        let mut b = Vec::new();
        b.push(passes.len() as u8);
        for (rec, kv) in passes {
            put_blob(&mut b, *rec);
            put_kv(&mut b, kv);
        }
        b.push(textures.len() as u8);
        for (n, blob, kv) in textures {
            put_str(&mut b, n);
            put_blob(&mut b, *blob);
            put_kv(&mut b, kv);
        }
        b.extend_from_slice(&(overrides.len() as u16).to_le_bytes());
        for (n, v) in overrides {
            put_str(&mut b, n);
            b.extend_from_slice(&v.to_le_bytes());
        }
        let rec = self.blob(&b)?;
        let cat = match self.categories.iter().position(|c| c == category) {
            Some(i) => i,
            None => {
                self.categories.push(category.to_string());
                self.categories.len() - 1
            }
        };
        self.presets.push((name.to_string(), cat as u16, rec));
        Ok(())
    }

    pub fn preset_count(&self) -> usize {
        self.presets.len()
    }

    /// Write the index and the header; returns the file's size.
    pub fn finish(mut self, source: &str, community: &[u32], default_preset: Option<u32>) -> std::io::Result<u64> {
        let mut b = Vec::new();
        put_str(&mut b, source);
        b.extend_from_slice(&(self.categories.len() as u32).to_le_bytes());
        for c in &self.categories {
            put_str(&mut b, c);
        }
        b.extend_from_slice(&(self.presets.len() as u32).to_le_bytes());
        for (n, c, rec) in &self.presets {
            put_str(&mut b, n);
            b.extend_from_slice(&c.to_le_bytes());
            put_blob(&mut b, *rec);
        }
        b.extend_from_slice(&(community.len() as u32).to_le_bytes());
        for c in community {
            b.extend_from_slice(&c.to_le_bytes());
        }
        b.extend_from_slice(&default_preset.unwrap_or(u32::MAX).to_le_bytes());
        let (off, len) = self.blob(&b)?;
        self.out.seek(SeekFrom::Start(0))?;
        let mut head = Vec::with_capacity(24);
        head.extend_from_slice(MAGIC);
        head.extend_from_slice(&VERSION.to_le_bytes());
        head.extend_from_slice(&off.to_le_bytes());
        head.extend_from_slice(&len.to_le_bytes());
        head.extend_from_slice(&0u32.to_le_bytes());
        self.out.write_all(&head)?;
        self.out.flush()?;
        Ok(self.at as u64)
    }
}

/// Where the program looks for the pack: next to the executable, then the working folder.
pub fn default_path() -> PathBuf {
    let exe = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default();
    let p = exe.join(FILE_NAME);
    if p.exists() {
        return p;
    }
    let cwd = PathBuf::from(FILE_NAME);
    if cwd.exists() { cwd } else { p }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let path = std::env::temp_dir().join(format!("sr-crt-test-{}.pak", std::process::id()));
        let shader = ShaderData {
            name: "s".into(),
            format: "R16G16B16A16_SFLOAT".into(),
            vs: vec![1, 2, 3],
            ps: vec![4, 5],
            params: vec![
                ParamDef { name: "A".into(), buffer: -1, offset: 16, size: 4, min: 0.0, max: 2.0, default: 1.0, step: 0.1, description: "Alpha".into() },
                ParamDef { name: "MVP".into(), buffer: 0, offset: 0, size: 64, min: 0.0, max: 0.0, default: 0.0, step: 0.0, description: String::new() },
                ParamDef { name: "FrameCount".into(), buffer: -1, offset: 0, size: 4, min: 0.0, max: 0.0, default: 0.0, step: 0.0, description: String::new() },
            ],
            samplers: vec![("Source".into(), 2), ("LUT".into(), 3)],
        };
        {
            let f = std::fs::File::create(&path).unwrap();
            let mut w = PackWriter::new(std::io::BufWriter::new(f)).unwrap();
            let s = w.shader("s", &shader).unwrap();
            assert_eq!(w.shader("s", &shader).unwrap(), s, "written once");
            let t = w.texture("t", b"PNG").unwrap();
            w.preset("one", "crt", &[(s, vec![("alias".into(), "First".into())]), (s, vec![])], &[("LUT".into(), t, vec![("linear".into(), "true".into())])], &[("A".into(), 1.5)]).unwrap();
            w.preset("two", "misc", &[(s, vec![])], &[], &[]).unwrap();
            w.finish("test", &[1], Some(0)).unwrap();
        }
        let p = Pack::open(&path).unwrap();
        assert_eq!(p.index.source, "test");
        assert_eq!(p.index.categories, ["crt", "misc"]);
        assert_eq!((p.index.presets.len(), p.index.community.clone(), p.index.default_preset), (2, vec![1], Some(0)));
        assert_eq!(p.index.key(1), "misc/two");
        assert_eq!((p.index.find("misc/two"), p.index.find("one"), p.index.find("x")), (Some(1), Some(0), None));
        let d = p.load(0, true).unwrap();
        assert_eq!((d.name.as_str(), d.category.as_str(), d.passes.len()), ("one", "crt", 2));
        assert_eq!(d.passes[0].shader, shader);
        assert_eq!(option(&d.passes[0].options, "alias"), Some("First"));
        assert_eq!(d.textures[0].png, b"PNG");
        let meta = p.load(0, false).unwrap();
        assert!(meta.passes[0].shader.vs.is_empty() && meta.textures[0].png.is_empty());
        // one user parameter (MVP and FrameCount are not), with the preset's override
        let u = d.user_params();
        assert_eq!(u.len(), 1);
        assert_eq!((u[0].name.as_str(), u[0].default), ("A", 1.5));
        drop(p);
        let _ = std::fs::remove_file(&path);
    }
}
