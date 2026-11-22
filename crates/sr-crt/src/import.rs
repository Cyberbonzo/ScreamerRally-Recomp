//! Pack generator: reads ShaderGlass's generated library headers (the RetroArch slang
//! shaders as precompiled DXBC, `ShaderGlass/Shaders/`) and writes `crt-shaders.pak`.
//!
//! Only the generated data is read (`RetroArch.h` for the preset order and the header
//! files, `*PresetDef.h`, `*ShaderDef.h`, `*TextureDef.h`, and `Options.h` for the community
//! favourites and the default preset); nothing in the checkout is built or run.
//!
//! The headers are ShaderGlass's (GPL-3.0, Copyright mausimus) generated from the
//! libretro slang-shaders, each under its authors' licence (see the header comments).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::pack::{ParamDef, ShaderData};

fn err(s: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, s)
}

/// What was imported.
#[derive(Debug, Default)]
pub struct Report {
    pub presets: usize,
    pub shaders: usize,
    pub textures: usize,
    pub bytes: u64,
    pub shader_bytes: u64,
    pub texture_bytes: u64,
    pub skipped: Vec<String>,
}

/// A C string literal starting at `s[0] == '"'`: its value and the bytes it took.
fn c_string(s: &[u8]) -> Option<(String, usize)> {
    if s.first() != Some(&b'"') {
        return None;
    }
    let mut out = Vec::new();
    let mut i = 1;
    while i < s.len() {
        match s[i] {
            b'"' => return Some((String::from_utf8_lossy(&out).into_owned(), i + 1)),
            b'\\' if i + 1 < s.len() => {
                i += 1;
                out.push(match s[i] {
                    b'n' => b'\n',
                    b't' => b'\t',
                    b'r' => b'\r',
                    b'0' => 0,
                    c => c,
                });
            }
            c => out.push(c),
        }
        i += 1;
    }
    None
}

/// The arguments of a call whose text starts after its `(`: split at top-level commas up
/// to the closing `)`, strings kept as written. Returns them and the bytes taken.
fn call_args(s: &[u8]) -> Option<(Vec<String>, usize)> {
    let mut args = Vec::new();
    let mut cur = Vec::new();
    let mut depth = 0;
    let mut i = 0;
    while i < s.len() {
        match s[i] {
            b'"' => {
                let (_, n) = c_string(&s[i..])?;
                cur.extend_from_slice(&s[i..i + n]);
                i += n;
                continue;
            }
            b'(' => {
                depth += 1;
                cur.push(b'(');
            }
            b')' if depth == 0 => {
                args.push(String::from_utf8_lossy(&cur).trim().to_string());
                return Some((args, i + 1));
            }
            b')' => {
                depth -= 1;
                cur.push(b')');
            }
            b',' if depth == 0 => {
                args.push(String::from_utf8_lossy(&cur).trim().to_string());
                cur.clear();
            }
            c => cur.push(c),
        }
        i += 1;
    }
    None
}

fn unquote(a: &str) -> String {
    c_string(a.trim().as_bytes()).map(|v| v.0).unwrap_or_else(|| a.trim().to_string())
}

/// `0.100000f`, `(float)25.000000`, `-1`.
fn number(a: &str) -> f32 {
    let a = a.trim().trim_start_matches("(float)").trim().trim_end_matches('f');
    a.parse::<f32>().unwrap_or_else(|_| if a.contains("inf") { f32::MAX } else { 0.0 })
}

/// The bytes of the array initialiser `{ 1, 2, ... }` after `marker`.
fn byte_array(text: &[u8], marker: &str) -> Option<Vec<u8>> {
    let at = find(text, marker.as_bytes(), 0)?;
    let open = text[at..].iter().position(|&c| c == b'{')? + at;
    let mut out = Vec::with_capacity(64 * 1024);
    let mut v: u32 = 0;
    let mut digits = false;
    for &c in &text[open + 1..] {
        match c {
            b'0'..=b'9' => {
                v = v * 10 + (c - b'0') as u32;
                digits = true;
            }
            b'}' => {
                if digits {
                    out.push(v as u8);
                }
                return Some(out);
            }
            _ => {
                if digits {
                    out.push(v as u8);
                    v = 0;
                    digits = false;
                }
            }
        }
    }
    None
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= hay.len() {
        return None;
    }
    let first = needle[0];
    let mut i = from;
    while i + needle.len() <= hay.len() {
        match hay[i..].iter().position(|&c| c == first) {
            Some(p) => i += p,
            None => return None,
        }
        if i + needle.len() <= hay.len() && &hay[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// DXBC containers are written with zero padding after them: keep the container's size.
fn trim_dxbc(mut b: Vec<u8>) -> Vec<u8> {
    if b.len() >= 32 && &b[0..4] == b"DXBC" {
        let size = u32::from_le_bytes(b[24..28].try_into().unwrap()) as usize;
        if size >= 32 && size <= b.len() {
            b.truncate(size);
        }
    }
    b
}

/// The value of `Name = "..."` (or another field) after `from`.
fn field(text: &[u8], name: &str, from: usize) -> Option<String> {
    let at = find(text, name.as_bytes(), from)?;
    let q = text[at..].iter().position(|&c| c == b'"')? + at;
    c_string(&text[q..]).map(|v| v.0)
}

/// A `*ShaderDef.h`.
pub fn parse_shader(text: &[u8]) -> Result<ShaderData, String> {
    let vs = trim_dxbc(byte_array(text, "sVertexByteCode[] =").ok_or("no vertex bytecode")?);
    let ps = trim_dxbc(byte_array(text, "sFragmentByteCode[] =").ok_or("no fragment bytecode")?);
    let class = find(text, b"public ShaderDef", 0).ok_or("no ShaderDef class")?;
    let end = find(text, b"VertexSource", class).unwrap_or(text.len());
    let body = &text[class..end];
    let name = field(body, "Name =", 0).unwrap_or_default();
    let format = field(body, "Format =", 0).unwrap_or_default();
    let mut params = Vec::new();
    let mut samplers = Vec::new();
    let mut i = 0;
    while let Some(at) = find(body, b"Add", i) {
        i = at + 3;
        let rest = &body[at..];
        if rest.starts_with(b"AddParam(") {
            let (a, n) = call_args(&rest[9..]).ok_or("bad AddParam")?;
            i = at + 9 + n;
            if a.len() < 7 {
                return Err(format!("AddParam with {} arguments", a.len()));
            }
            params.push(ParamDef {
                name: unquote(&a[0]),
                buffer: number(&a[1]) as i32,
                offset: number(&a[2]) as u32,
                size: number(&a[3]) as u32,
                min: number(&a[4]),
                max: number(&a[5]),
                default: number(&a[6]),
                step: a.get(7).map_or(0.0, |s| number(s)),
                description: a.get(8).map(|s| unquote(s)).unwrap_or_default(),
            });
        } else if rest.starts_with(b"AddSampler(") {
            let (a, n) = call_args(&rest[11..]).ok_or("bad AddSampler")?;
            i = at + 11 + n;
            if a.len() == 2 {
                samplers.push((unquote(&a[0]), number(&a[1]) as u32));
            }
        }
    }
    Ok(ShaderData { name, format, vs, ps, params, samplers })
}

/// A `*TextureDef.h`: the PNG file's bytes.
pub fn parse_texture(text: &[u8]) -> Result<Vec<u8>, String> {
    byte_array(text, "sData[] =").ok_or_else(|| "no texture data".into())
}

/// One `X.push_back(YDef()` with its `.Param("k", "v")` chain.
#[derive(Debug, PartialEq)]
pub struct DefRef {
    pub class: String,
    pub params: Vec<(String, String)>,
}

#[derive(Debug, Default, PartialEq)]
pub struct PresetSource {
    pub name: String,
    pub category: String,
    pub shaders: Vec<DefRef>,
    pub textures: Vec<DefRef>,
    pub overrides: Vec<(String, f32)>,
}

fn def_refs(body: &[u8], call: &str) -> Result<Vec<DefRef>, String> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(at) = find(body, call.as_bytes(), i) {
        let mut j = at + call.len();
        let ident_end = body[j..].iter().position(|&c| c == b'(').ok_or("bad push_back")? + j;
        let class = String::from_utf8_lossy(&body[j..ident_end]).trim().to_string();
        j = ident_end;
        // the constructor's "()"
        let (_, n) = call_args(&body[j + 1..]).ok_or("bad constructor")?;
        j += 1 + n;
        let mut params = Vec::new();
        loop {
            while j < body.len() && body[j].is_ascii_whitespace() {
                j += 1;
            }
            if body[j..].starts_with(b".Param(") {
                let (a, n) = call_args(&body[j + 7..]).ok_or("bad Param")?;
                if a.len() == 2 {
                    params.push((unquote(&a[0]), unquote(&a[1])));
                }
                j += 7 + n;
            } else {
                break;
            }
        }
        out.push(DefRef { class, params });
        i = j;
    }
    Ok(out)
}

/// A `*PresetDef.h`.
pub fn parse_preset(text: &[u8]) -> Result<PresetSource, String> {
    let class = find(text, b"public PresetDef", 0).ok_or("no PresetDef class")?;
    let mut p = PresetSource {
        name: field(text, "Name =", class).ok_or("no name")?,
        category: field(text, "Category =", class).unwrap_or_default(),
        ..Default::default()
    };
    let build = find(text, b"Build()", class).ok_or("no Build()")?;
    let body = &text[build..];
    p.shaders = def_refs(body, "ShaderDefs.push_back(")?;
    p.textures = def_refs(body, "TextureDefs.push_back(")?;
    let mut i = 0;
    while let Some(at) = find(body, b"OverrideParam(", i) {
        let (a, n) = call_args(&body[at + 14..]).ok_or("bad OverrideParam")?;
        i = at + 14 + n;
        if a.len() == 2 {
            p.overrides.push((unquote(&a[0]), number(&a[1])));
        }
    }
    Ok(p)
}

/// `favoritePresets[]` and `defaultPreset` of ShaderGlass's `Options.h`.
pub fn parse_options(text: &[u8]) -> (Vec<String>, Option<String>) {
    let mut favs = Vec::new();
    if let Some(at) = find(text, b"favoritePresets[]", 0) {
        if let Some(open) = text[at..].iter().position(|&c| c == b'{') {
            let mut i = at + open;
            while i < text.len() && text[i] != b'}' {
                if text[i] == b'"' {
                    if let Some((s, n)) = c_string(&text[i..]) {
                        favs.push(s);
                        i += n;
                        continue;
                    }
                }
                i += 1;
            }
        }
    }
    let default = find(text, b"defaultPreset", 0).and_then(|at| field(text, "=", at));
    (favs, default)
}

/// Version and commit of the checkout, for the pack's index (best effort).
fn source_name(root: &Path) -> String {
    let mut s = String::from("ShaderGlass");
    if let Ok(b) = std::fs::read(root.join("ShaderGlass").join("ShaderGlass.rc")) {
        // UTF-16 resource script
        let text: String = if b.starts_with(&[0xff, 0xfe]) {
            String::from_utf16_lossy(&b[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>())
        } else {
            String::from_utf8_lossy(&b).into_owned()
        };
        if let Some(at) = text.find("ShaderGlass v") {
            let v: String = text[at + 12..].chars().take_while(|c| !c.is_whitespace() && *c != '"').collect();
            s.push(' ');
            s.push_str(&v);
        }
    }
    let git = root.join(".git");
    let head = std::fs::read_to_string(git.join("HEAD")).unwrap_or_default();
    let sha = match head.trim().strip_prefix("ref: ") {
        Some(r) => std::fs::read_to_string(git.join(r)).unwrap_or_default(),
        None => head.clone(),
    };
    let sha = sha.trim();
    if sha.len() >= 7 {
        s.push_str(&format!(" ({})", &sha[..7]));
    }
    s
}

/// Read the library of the ShaderGlass checkout at `root` and write the pack to `out`: the
/// presets of the top categories in `only` ("crt" for `crt/...`), or all when it is empty.
/// `progress` is told about every 200th preset.
pub fn build_pack(root: &Path, out: &Path, only: &[&str], progress: &mut dyn FnMut(&str)) -> std::io::Result<Report> {
    let shaders_dir = root.join("ShaderGlass").join("Shaders");
    let list = std::fs::read(shaders_dir.join("RetroArch.h")).map_err(|e| err(format!("{}: {e}", shaders_dir.join("RetroArch.h").display())))?;
    // class name -> header file (from the #include lines)
    let mut files: HashMap<String, PathBuf> = HashMap::new();
    let mut order = Vec::new();
    for line in String::from_utf8_lossy(&list).lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("#include \"") {
            let rel = rest.trim_end_matches('"').replace('\\', "/");
            let class = Path::new(&rel).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            files.insert(class, shaders_dir.join(rel));
        } else if let Some(rest) = line.strip_prefix("new ") {
            let class = rest.trim_end_matches(',').trim_end_matches("()").trim().to_string();
            order.push(class);
        }
    }
    if order.is_empty() {
        return Err(err("RetroArch.h lists no presets".into()));
    }
    let (favs, default) = std::fs::read(root.join("ShaderGlass").join("Options.h")).map(|t| parse_options(&t)).unwrap_or_default();
    let tmp = out.with_extension("pak.tmp");
    let f = std::fs::File::create(&tmp)?;
    let mut w = crate::pack::PackWriter::new(std::io::BufWriter::with_capacity(1 << 20, f))?;
    let mut report = Report::default();
    let mut names: Vec<String> = Vec::new();
    let open = |class: &str| -> Result<Vec<u8>, String> {
        let p = files.get(class).ok_or_else(|| format!("{class}: not in RetroArch.h"))?;
        std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))
    };
    for (n, class) in order.iter().enumerate() {
        if n % 200 == 0 {
            progress(&format!("{n} of {} presets", order.len()));
        }
        let result = (|| -> Result<(), String> {
            let p = parse_preset(&open(class)?).map_err(|e| format!("{class}: {e}"))?;
            if !only.is_empty() && !only.contains(&p.category.split('/').next().unwrap_or("")) {
                return Ok(());
            }
            let mut passes = Vec::new();
            for s in &p.shaders {
                let rec = match w.written_shader(&s.class) {
                    Some(r) => r,
                    None => {
                        let data = parse_shader(&open(&s.class)?).map_err(|e| format!("{}: {e}", s.class))?;
                        report.shaders += 1;
                        w.shader(&s.class, &data).map_err(|e| e.to_string())?
                    }
                };
                passes.push((rec, s.params.clone()));
            }
            if passes.is_empty() {
                return Err(format!("{class}: no passes"));
            }
            let mut textures = Vec::new();
            for t in &p.textures {
                let png = parse_texture(&open(&t.class)?).map_err(|e| format!("{}: {e}", t.class))?;
                let before = w.bytes_textures;
                let blob = w.texture(&t.class, &png).map_err(|e| e.to_string())?;
                if w.bytes_textures != before {
                    report.textures += 1;
                }
                let name = t.params.iter().find(|(k, _)| k == "name").map(|(_, v)| v.clone()).unwrap_or_default();
                let opts: Vec<(String, String)> = t.params.iter().filter(|(k, _)| k != "name").cloned().collect();
                textures.push((name, blob, opts));
            }
            w.preset(&p.name, &p.category, &passes, &textures, &p.overrides).map_err(|e| e.to_string())?;
            names.push(p.name.clone());
            Ok(())
        })();
        if let Err(e) = result {
            report.skipped.push(e);
        }
    }
    let community: Vec<u32> = favs.iter().filter_map(|f| names.iter().position(|n| n == f).map(|i| i as u32)).collect();
    let default_preset = default.and_then(|d| names.iter().position(|n| *n == d)).map(|i| i as u32);
    report.presets = w.preset_count();
    report.shader_bytes = w.bytes_shaders;
    report.texture_bytes = w.bytes_textures;
    report.bytes = w.finish(&source_name(root), &community, default_preset)?;
    std::fs::rename(&tmp, out)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_and_calls() {
        assert_eq!(c_string(br#""a\"b" x"#), Some(("a\"b".to_string(), 6)));
        let (a, n) = call_args(br#""CRTgamma", -1, 4, 4, 0.100000f, 5.000000f, 2.400000f, 0.100000f, "CRTGeom, (Target) Gamma");"#).unwrap();
        assert_eq!(a.len(), 9);
        assert_eq!(unquote(&a[8]), "CRTGeom, (Target) Gamma");
        assert_eq!((number(&a[1]), number(&a[6])), (-1.0, 2.4));
        assert_eq!(&br#""CRTgamma", -1, 4, 4, 0.100000f, 5.000000f, 2.400000f, 0.100000f, "CRTGeom, (Target) Gamma");"#[n..], b";");
        assert_eq!(number("(float)25.000000"), 25.0);
    }

    #[test]
    fn preset_header() {
        let text = br#"
namespace RetroArch
{
class CrtCrtRoyalePresetDef : public PresetDef
{
public:
	CrtCrtRoyalePresetDef() : PresetDef{}
	{
		Name = "crt-royale";
		Category = "crt";
	}

	void Build() {
         	ShaderDefs.push_back(CrtShadersCrtRoyaleSrc_fastCrtRoyaleFirstPassLinearizeCrtGammaBobFieldsShaderDef()
.Param("alias", "ORIG_LINEARIZED")
.Param("filter_linear", "false")
.Param("scale_type", "source")
.Param("scale", "1.0")
.Param("srgb_framebuffer", "true"));
         	ShaderDefs.push_back(XShaderDef());
            TextureDefs.push_back(CrtShadersCrtRoyaleTileableLinearApertureGrille15Wide8And5d5SpacingResizeTo64TextureDef()
.Param("name", "mask_grille_texture_small")
.Param("wrap_mode", "repeat"));
            OverrideParam("SMAA_EDT", (float)0.500000);
	}
};
}"#;
        let p = parse_preset(text).unwrap();
        assert_eq!((p.name.as_str(), p.category.as_str()), ("crt-royale", "crt"));
        assert_eq!(p.shaders.len(), 2);
        assert_eq!(p.shaders[0].class, "CrtShadersCrtRoyaleSrc_fastCrtRoyaleFirstPassLinearizeCrtGammaBobFieldsShaderDef");
        assert_eq!(p.shaders[0].params.len(), 5);
        assert_eq!(p.shaders[0].params[4], ("srgb_framebuffer".to_string(), "true".to_string()));
        assert!(p.shaders[1].params.is_empty());
        assert_eq!(p.textures[0].params[0], ("name".to_string(), "mask_grille_texture_small".to_string()));
        assert_eq!(p.overrides, [("SMAA_EDT".to_string(), 0.5)]);
    }

    #[test]
    fn shader_header() {
        let text = br#"
/* AddParam("comment", 0, 0, 4, 0f, 0f, 0f, 0f, "not this"); */
namespace XShaderDefs
{
static const BYTE sVertexByteCode[] =
{
68,88,66,67,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,1,0,0,0,32,0,0,0,0,0,0,0,0,0,0,0
};
static const BYTE sFragmentByteCode[] =
{
1,2,3
};
}
namespace RetroArch
{
class XShaderDef : public ShaderDef
{
public:
	XShaderDef() : ShaderDef{}
	{
		Name = "crt-geom";
		Format = "R16G16B16A16_SFLOAT";
		AddParam("CRTgamma", -1, 4, 4, 0.100000f, 5.000000f, 2.400000f, 0.100000f, "CRTGeom Target Gamma");
		AddParam("MVP", 0, 0, 64, 0.000000f, 0.000000f, 0.000000f, 0.000000f, "");
		AddSampler("Source", 2);
/*
VertexSource = %*VERTEX_SOURCE*%; AddParam("x", 0, 0, 4, 0f, 0f, 0f, 0f, "");
*/
	}
};
}"#;
        let s = parse_shader(text).unwrap();
        assert_eq!(s.vs.len(), 32, "trimmed to the container size");
        assert_eq!(s.ps, [1, 2, 3]);
        assert_eq!((s.name.as_str(), s.format.as_str()), ("crt-geom", "R16G16B16A16_SFLOAT"));
        assert_eq!(s.params.len(), 2);
        assert_eq!(s.params[0], ParamDef { name: "CRTgamma".into(), buffer: -1, offset: 4, size: 4, min: 0.1, max: 5.0, default: 2.4, step: 0.1, description: "CRTGeom Target Gamma".into() });
        assert_eq!(s.samplers, [("Source".to_string(), 2)]);
    }

    #[test]
    fn options_header() {
        let (f, d) = parse_options(b"static const char* defaultPreset     = \"newpixie-crt\";\nstatic const char* favoritePresets[] = {\"bayer_4x4\",\n \"crt-geom\"};");
        assert_eq!(f, ["bayer_4x4", "crt-geom"]);
        assert_eq!(d.as_deref(), Some("newpixie-crt"));
    }
}
