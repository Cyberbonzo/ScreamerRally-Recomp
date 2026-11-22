//! The CRT tab of the settings overlay: ShaderGlass's options for the game (see `sr_crt`).
//!
//! * Shader: on / off ("Active"), the library (ShaderGlass's browser: the CRT shaders,
//!   personal and community favourites, recent ones, search results; the package's pack
//!   holds RetroArch's crt category alone, a pack with more lists its categories too), the
//!   preset (Left / Right: previous / next, ShaderGlass's
//!   "Next"), search, favourite, random.
//! * Picture: pixel size (the shader's input), aspect ratio correction (presets and a
//!   custom ratio), output scale, flip, orientation; and in a group the hold-to-compare key,
//!   snapshots and profiles.
//! * Parameters: every parameter of the preset with its range and step (Enter / A: back
//!   to its default), saved per preset, paged when the panel cannot show them all.

use std::path::PathBuf;
use std::sync::Mutex;

use sr_crt::config::{ASPECT_RATIOS, CUSTOM_ASPECT, HOLD_KEYS, OUTPUT_SCALES, PIXEL_SIZES};
use sr_crt::pack::UserParam;
use sr_render::overlay::{self, Item, Menu, Row, Tab, Value};

use crate::menu::Context;
use crate::settings::Settings;

pub const TAB_TITLE: &str = "CRT";

/// Libraries before the categories.
const ALL: usize = 0;
const FAVORITES: usize = 1;
const COMMUNITY: usize = 2;
const RECENT: usize = 3;
const SEARCH: usize = 4;
const FIRST_CATEGORY: usize = 5;
const RECENT_MAX: usize = 10;
const PROFILES: usize = 8;

/// The tab's state that is not a setting.
#[derive(Default)]
struct Ui {
    library: usize,
    search: String,
    /// The presets listed by the Shader item (pack indexes).
    list: Vec<usize>,
    /// The chosen preset's parameters.
    params: Vec<UserParam>,
    profile: usize,
    rng: u64,
}

static UI: Mutex<Option<Ui>> = Mutex::new(None);

fn with_ui<T>(f: impl FnOnce(&mut Ui) -> T) -> T {
    let mut g = UI.lock().unwrap();
    f(g.get_or_insert_with(Ui::default))
}

/// The RetroArch categories' first part ("crt", "ntsc", ...), sorted.
fn top_categories(pack: &sr_crt::Pack) -> Vec<String> {
    let mut v: Vec<String> = pack.index.categories.iter().map(|c| c.split('/').next().unwrap_or(c).to_string()).collect();
    v.sort();
    v.dedup();
    v
}

/// The presets of library `lib`.
fn library_list(pack: &sr_crt::Pack, s: &Settings, lib: usize, search: &str) -> Vec<usize> {
    let ix = &pack.index;
    let keys = |list: &[String]| list.iter().filter_map(|k| ix.find(k)).collect::<Vec<_>>();
    match lib {
        ALL => (0..ix.presets.len()).collect(),
        FAVORITES => keys(&s.crt_favorites),
        COMMUNITY => ix.community.iter().map(|&i| i as usize).collect(),
        RECENT => keys(&s.crt_recent),
        SEARCH => {
            let q = search.trim().to_lowercase();
            if q.is_empty() {
                return Vec::new();
            }
            (0..ix.presets.len()).filter(|&i| ix.key(i).to_lowercase().contains(&q)).collect()
        }
        _ => {
            let cats = top_categories(pack);
            let Some(top) = cats.get(lib - FIRST_CATEGORY) else { return Vec::new() };
            (0..ix.presets.len()).filter(|&i| ix.category(i).split('/').next() == Some(top.as_str())).collect()
        }
    }
}

fn library_names(pack: &sr_crt::Pack, s: &Settings, search: &str) -> Vec<String> {
    let mut v = vec![
        format!("CRT shaders ({})", pack.index.presets.len()),
        format!("Personal favorites ({})", library_list(pack, s, FAVORITES, search).len()),
        format!("Community favorites ({})", pack.index.community.len()),
        format!("Recent ({})", library_list(pack, s, RECENT, search).len()),
        format!("Search results ({})", library_list(pack, s, SEARCH, search).len()),
    ];
    // the package's library is the CRT category alone: no category entries repeating it
    let cats = top_categories(pack);
    if cats.len() > 1 {
        for (k, c) in cats.iter().enumerate() {
            v.push(format!("{c} ({})", library_list(pack, s, FIRST_CATEGORY + k, search).len()));
        }
    }
    v
}

/// A preset's name as the Shader item shows it in library `lib`: the path below the
/// category, or the whole key; cut at the front to fit.
fn shown_name(pack: &sr_crt::Pack, i: usize, lib: usize, cols: usize) -> String {
    let key = pack.index.key(i);
    let name = if lib >= FIRST_CATEGORY { key.split_once('/').map_or(key.clone(), |(_, r)| r.to_string()) } else { key };
    let name = overlay::printable(&name);
    let n = name.chars().count();
    if n <= cols { name } else { format!("\u{2026}{}", name.chars().skip(n - cols + 1).collect::<String>()) }
}

/// The chosen preset (its pack index): the setting's, or ShaderGlass's default.
fn current(pack: &sr_crt::Pack, s: &Settings) -> Option<usize> {
    pack.index.find(&s.crt.preset).or(pack.index.default_preset.map(|d| d as usize)).or((!pack.index.presets.is_empty()).then_some(0))
}

fn choice(id: &str, label: &str, options: Vec<String>, index: usize, hint: &str) -> Item {
    let cols = overlay::value_columns(label, 0);
    let options = options.into_iter().map(|o| overlay::fit(&overlay::printable(&o), cols)).collect();
    Item { id: id.into(), label: label.into(), value: Value::Choice { options, index }, hint: hint.into(), inactive: false }
}

fn names(table: &[(impl Copy, &str, &str)]) -> Vec<String> {
    table.iter().map(|t| t.1.to_string()).collect()
}

fn action(id: &str, label: &str, text: &str, hint: &str) -> Item {
    Item { id: id.into(), label: label.into(), value: Value::Action(text.into()), hint: hint.into(), inactive: false }
}

fn top(item: Item) -> Row {
    Row::Item { item, depth: 0 }
}

fn sub(item: Item) -> Row {
    Row::Item { item, depth: 1 }
}

/// The parameter rows of the paged list.
fn param_rows(s: &Settings, params: &[UserParam]) -> Vec<Row> {
    let changed = s.crt_params.get(&s.crt.preset);
    let mut rows = vec![top(action("crt_reset", "Reset all to defaults", "ENTER", "Every parameter of this shader back to the preset's value (ShaderGlass's Reset)."))];
    for p in params {
        let value = changed.and_then(|l| l.iter().find(|(n, _)| *n == p.name)).map_or(p.default, |v| v.1);
        let v = Value::Float { value: value.clamp(p.min.min(p.max), p.max.max(p.min)), min: p.min, max: p.max, step: p.step, default: p.default };
        let widest = match &v {
            Value::Float { min, max, step, default, .. } => [*min, *max, *default, -max.abs().max(min.abs())].iter().map(|x| overlay::float_text(*x, *step).chars().count()).max().unwrap_or(4),
            _ => 4,
        };
        let label_cols = 56usize.saturating_sub(2 + 1 + widest + 4);
        let desc = if p.description.trim().is_empty() { p.name.clone() } else { p.description.trim().to_string() };
        let label = overlay::fit(&overlay::printable(&desc), label_cols);
        let hint = overlay::printable(&format!(
            "{}: {} to {}, step {}, default {}. {}",
            p.name,
            overlay::float_text(p.min, p.step),
            overlay::float_text(p.max, p.step),
            if p.step > 0.0 { overlay::float_text(p.step, p.step) } else { "1/10".into() },
            overlay::float_text(p.default, p.step),
            if desc.chars().count() > label_cols { desc.as_str() } else { "Enter / A: default." }
        ));
        rows.push(top(Item { id: format!("crt_param:{}", p.name), label, value: v, hint: overlay::fit(&hint, 112), inactive: false }));
    }
    rows
}

/// The CRT tab for these settings.
pub fn tab(s: &Settings, crt: &sr_crt::SharedCrt) -> Tab {
    let Some(pack) = crt.pack.clone() else {
        return Tab {
            title: TAB_TITLE.into(),
            page: 0,
            rows: vec![
                Row::Section("Shader library not found".into()),
                top(Item {
                    id: "crt_active".into(),
                    label: "CRT emulation".into(),
                    value: Value::Choice { options: vec!["Not available".into()], index: 0 },
                    hint: "crt-shaders.pak is missing next to the program: srtool crt-pack makes it from ShaderGlass (README).".into(),
                    inactive: true,
                }),
            ],
        };
    };
    let cur = current(&pack, s);
    let (library, search, profile) = with_ui(|u| (u.library, u.search.clone(), u.profile));
    let mut list = library_list(&pack, s, library, &search);
    if let Some(c) = cur {
        if !list.contains(&c) {
            list.insert(0, c);
        }
    }
    let params = cur.and_then(|i| pack.load(i, false).ok()).map(|d| d.user_params()).unwrap_or_default();
    let cols = overlay::value_columns("Shader", 0);
    let shader_names: Vec<String> = list.iter().map(|&i| shown_name(&pack, i, library, cols)).collect();
    let shader_index = cur.and_then(|c| list.iter().position(|&i| i == c)).unwrap_or(0);
    let key = cur.map(|c| pack.index.key(c)).unwrap_or_default();
    let favorite = s.crt_favorites.contains(&key);
    let c = &s.crt;
    let mut rows = vec![
        Row::Section("Shader".into()),
        top(choice(
            "crt_active",
            "CRT emulation",
            vec!["Off".into(), "On".into()],
            c.active as usize,
            "A port of ShaderGlass: RetroArch's CRT shaders on the game's own 480 lines. Off: the picture as without it.",
        )),
        top(choice("crt_library", "Library", library_names(&pack, s, &search), library, "Which shaders the Shader option steps through: all CRT shaders, favourites, recent or search results.")),
        top(choice(
            "crt_shader",
            "Shader",
            shader_names,
            shader_index,
            &overlay::printable(&format!("{key}. Left / Right: previous / next in the library (ShaderGlass's Next).")),
        )),
        top(Item { id: "crt_search".into(), label: "Search".into(), value: Value::Text(search.clone()), hint: "Type part of a name: the matches become the library's search results.".into(), inactive: false }),
        top(choice("crt_favorite", "Favorite", vec!["No".into(), "Yes".into()], favorite as usize, "Adds the shader to (or removes it from) your personal favourites.")),
        top(action("crt_random", "Random shader", "ENTER", "Picks a shader at random from the library shown.")),
        Row::Section("Picture".into()),
        top(choice(
            "crt_pixel",
            "Pixel size",
            names(PIXEL_SIZES),
            c.pixel_size,
            "The picture the shader gets: the game draws at it. 480 lines is the game's own; xN: the screen divided by N.",
        )),
        top(choice("crt_aspect", "Aspect correction", names(ASPECT_RATIOS), c.aspect, "Pixels taller (1.2: DOS, NTSC) or wider than square; Stretch fills the window.")),
        sub(Item {
            id: "crt_custom".into(),
            label: "Custom ratio".into(),
            value: Value::Float { value: c.custom_aspect, min: 0.25, max: 4.0, step: 0.01, default: 1.0 },
            hint: "The height of a pixel relative to its width, for Aspect correction: Custom.".into(),
            inactive: c.aspect != CUSTOM_ASPECT,
        }),
        top(choice("crt_scale", "Scale", names(OUTPUT_SCALES), c.scale, "Free: the largest picture that fits. A percentage: of the game's picture height, centred.")),
        top(choice(
            "crt_flip",
            "Flip",
            vec!["None".into(), "Horizontal".into(), "Vertical".into(), "Both".into()],
            c.flip_h as usize + 2 * c.flip_v as usize,
            "Mirrors the picture before the shader.",
        )),
        top(choice("crt_orient", "Orientation", vec!["Horizontal".into(), "Vertical".into()], c.vertical as usize, "Vertical: the shader works on the picture turned a quarter (scanlines run down).")),
        Row::Group { title: "Keys, snapshot & profiles".into(), hint: "The hold-to-compare key, snapshots of the shader's picture, saved CRT setups.".into(), depth: 0, expanded: false },
        sub(choice(
            "crt_hold",
            "Hold to compare",
            HOLD_KEYS.iter().map(|k| k.1.to_string()).collect(),
            c.hold_key,
            "While this key is held the game is shown without the shader (ShaderGlass: hold Tab).",
        )),
        sub(action("crt_snapshot", "Take snapshot", "ENTER", "Saves the shader's picture as PNG in the data folder's snapshots folder.")),
        sub(choice("crt_profile", "Profile", (1..=PROFILES).map(|n| n.to_string()).collect(), profile, &profile_hint(s, profile))),
        sub(action("crt_save", "Save profile", "ENTER", "Stores this CRT setup (shader, picture options, parameters) in the profile above.")),
        sub(action("crt_load", "Load profile", "ENTER", "Brings back the CRT setup stored in the profile above.")),
        Row::Pager { label: "Parameters".into(), hint: format!("{} parameters of this shader. Left / Right: pages.", params.len()) },
    ];
    rows.extend(param_rows(s, &params));
    with_ui(|u| {
        u.list = list;
        u.params = params;
    });
    Tab { title: TAB_TITLE.into(), rows, page: 0 }
}

fn profiles_dir(data: &std::path::Path) -> PathBuf {
    data.join("crt-profiles")
}

fn profile_path(data: &std::path::Path, slot: usize) -> PathBuf {
    profiles_dir(data).join(format!("profile{}.ini", slot + 1))
}

/// What a profile holds, for the hint.
fn profile_hint(_s: &Settings, slot: usize) -> String {
    let what = PROFILE_DATA.lock().unwrap().clone().and_then(|d| std::fs::read_to_string(profile_path(&d, slot)).ok()).map(|t| {
        let (p, _) = Settings::parse(&t);
        if p.crt.preset.is_empty() { "ShaderGlass's default".to_string() } else { p.crt.preset }
    });
    match what {
        Some(w) => overlay::fit(&overlay::printable(&format!("Profile {}: {w}.", slot + 1)), 112),
        None => format!("Profile {} is empty.", slot + 1),
    }
}

/// The data folder, for the profiles' hints.
static PROFILE_DATA: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn set_data(data: &std::path::Path) {
    *PROFILE_DATA.lock().unwrap() = Some(data.to_path_buf());
}

fn index(menu: &Menu, id: &str) -> usize {
    match menu.item(id).map(|i| &i.value) {
        Some(Value::Choice { index, .. }) => *index,
        _ => 0,
    }
}

/// Rebuild the tab in place, keeping the selected option, the open groups and the page.
fn rebuild(menu: &mut Menu, s: &Settings, crt: &sr_crt::SharedCrt) {
    let Some(t) = menu.tabs.iter().position(|t| t.title == TAB_TITLE) else { return };
    let selected = match menu.tabs[t].rows.get(menu.row) {
        Some(Row::Item { item, .. }) if menu.tab == t => Some(item.id.clone()),
        _ => None,
    };
    let expanded: Vec<String> = menu.tabs[t].rows.iter().filter_map(|r| if let Row::Group { title, expanded: true, .. } = r { Some(title.clone()) } else { None }).collect();
    let page = menu.tabs[t].page;
    let mut new = tab(s, crt);
    for r in &mut new.rows {
        if let Row::Group { title, expanded: e, .. } = r {
            *e = expanded.contains(title);
        }
    }
    new.page = page;
    menu.tabs[t] = new;
    if menu.tab == t {
        if let Some(id) = selected {
            if let Some(i) = menu.tabs[t].rows.iter().position(|r| matches!(r, Row::Item { item, .. } if item.id == id)) {
                menu.row = i;
            } else if id.starts_with("crt_param:") {
                // the parameter is gone (another shader): the pager
                menu.row = menu.tabs[t].pager().unwrap_or(0);
            }
        }
    }
}

fn push_recent(s: &mut Settings, key: &str) {
    if key.is_empty() {
        return;
    }
    s.crt_recent.retain(|k| k != key);
    s.crt_recent.insert(0, key.to_string());
    s.crt_recent.truncate(RECENT_MAX);
}

/// Tell the renderer about the settings.
fn publish(ctx: &Context, s: &Settings) {
    let live = s.crt_live();
    ctx.crt.update(|c| *c = live);
}

/// Choose preset `i`: the setting, the parameters' rows follow.
fn choose(pack: &sr_crt::Pack, s: &mut Settings, i: usize) {
    s.crt.preset = pack.index.key(i);
}

/// The overlay was closed: the shader in use goes to the top of Recent.
pub fn closed(ctx: &Context, menu: &mut Menu) {
    let mut s = ctx.settings.lock().unwrap();
    if !s.crt.active {
        return;
    }
    let Some(pack) = ctx.crt.pack.clone() else { return };
    let Some(cur) = current(&pack, &s) else { return };
    let key = pack.index.key(cur);
    if s.crt_recent.first() == Some(&key) {
        return;
    }
    push_recent(&mut s, &key);
    let _ = std::fs::write(&ctx.path, s.to_ini());
    let snapshot = s.clone();
    drop(s);
    rebuild(menu, &snapshot, &ctx.crt);
}

/// Apply a change of the CRT tab's item `id`. Returns a note for the status line.
pub fn apply(ctx: &Context, menu: &mut Menu, s: &mut Settings, id: &str) -> Option<String> {
    let Some(pack) = ctx.crt.pack.clone() else { return None };
    let mut note = None;
    let mut again = false;
    match id {
        "crt_active" => s.crt.active = index(menu, id) == 1,
        "crt_library" => {
            with_ui(|u| u.library = index(menu, id));
            again = true;
        }
        "crt_shader" => {
            let i = index(menu, id);
            if let Some(&p) = with_ui(|u| u.list.get(i).copied()).as_ref() {
                choose(&pack, s, p);
            }
            again = true;
        }
        "crt_search" => {
            let text = match menu.item(id).map(|i| &i.value) {
                Some(Value::Text(t)) => t.clone(),
                _ => String::new(),
            };
            with_ui(|u| {
                u.search = text;
                u.library = SEARCH;
            });
            again = true;
        }
        "crt_favorite" => {
            if let Some(cur) = current(&pack, s) {
                let key = pack.index.key(cur);
                s.crt_favorites.retain(|k| *k != key);
                if index(menu, id) == 1 {
                    s.crt_favorites.insert(0, key);
                }
            }
            again = true;
        }
        "crt_random" => {
            let list = with_ui(|u| u.list.clone());
            let cur = current(&pack, s);
            let pick = with_ui(|u| {
                if u.rng == 0 {
                    u.rng = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0x9e37_79b9, |d| d.as_nanos() as u64) | 1;
                }
                u.rng ^= u.rng << 13;
                u.rng ^= u.rng >> 7;
                u.rng ^= u.rng << 17;
                u.rng
            });
            let pool: Vec<usize> = if list.len() > 1 { list.into_iter().filter(|&i| Some(i) != cur).collect() } else { (0..pack.index.presets.len()).collect() };
            if !pool.is_empty() {
                choose(&pack, s, pool[(pick % pool.len() as u64) as usize]);
            }
            again = true;
        }
        "crt_pixel" => s.crt.pixel_size = index(menu, id).min(PIXEL_SIZES.len() - 1),
        "crt_aspect" => {
            s.crt.aspect = index(menu, id).min(ASPECT_RATIOS.len() - 1);
            if let Some(i) = menu.item_mut("crt_custom") {
                i.inactive = s.crt.aspect != CUSTOM_ASPECT;
            }
        }
        "crt_custom" => {
            if let Some(Value::Float { value, .. }) = menu.item(id).map(|i| &i.value) {
                s.crt.custom_aspect = *value;
            }
        }
        "crt_scale" => s.crt.scale = index(menu, id).min(OUTPUT_SCALES.len() - 1),
        "crt_flip" => {
            let f = index(menu, id);
            s.crt.flip_h = f & 1 != 0;
            s.crt.flip_v = f & 2 != 0;
        }
        "crt_orient" => s.crt.vertical = index(menu, id) == 1,
        "crt_hold" => s.crt.hold_key = index(menu, id).min(HOLD_KEYS.len() - 1),
        "crt_snapshot" => {
            if s.crt.active {
                *ctx.crt.snapshot.lock().unwrap() = Some(ctx.data.join("snapshots"));
                note = Some("Snapshot: taking it".to_string());
            } else {
                note = Some("Snapshot: CRT emulation is off".to_string());
            }
        }
        "crt_profile" => {
            let slot = index(menu, id);
            with_ui(|u| u.profile = slot);
            if let Some(i) = menu.item_mut(id) {
                i.hint = profile_hint(s, slot);
            }
        }
        "crt_save" => {
            let slot = with_ui(|u| u.profile);
            let path = profile_path(&ctx.data, slot);
            let r = std::fs::create_dir_all(profiles_dir(&ctx.data)).and_then(|_| std::fs::write(&path, s.crt_ini(false)));
            note = Some(match r {
                Ok(()) => format!("Saved profile {}", slot + 1),
                Err(e) => format!("Profile not saved: {e}"),
            });
            again = true;
        }
        "crt_load" => {
            let slot = with_ui(|u| u.profile);
            match std::fs::read_to_string(profile_path(&ctx.data, slot)) {
                Ok(t) => {
                    let (p, _) = Settings::parse(&t);
                    s.crt = p.crt.clone();
                    let params = p.crt_params.get(&p.crt.preset).cloned().unwrap_or_default();
                    if params.is_empty() {
                        s.crt_params.remove(&p.crt.preset);
                    } else {
                        s.crt_params.insert(p.crt.preset.clone(), params);
                    }
                    note = Some(format!("Loaded profile {}", slot + 1));
                }
                Err(_) => note = Some(format!("Profile {} is empty", slot + 1)),
            }
            again = true;
        }
        "crt_reset" => {
            s.crt_params.remove(&s.crt.preset.clone());
            again = true;
        }
        _ => {
            if let Some(name) = id.strip_prefix("crt_param:") {
                let value = match menu.item(id).map(|i| &i.value) {
                    Some(Value::Float { value, .. }) => Some(*value),
                    _ => None,
                };
                let default = with_ui(|u| u.params.iter().find(|p| p.name == name).map(|p| p.default));
                if let (Some(v), Some(d)) = (value, default) {
                    let preset = s.crt.preset.clone();
                    let preset = if preset.is_empty() { current(&pack, s).map(|c| pack.index.key(c)).unwrap_or_default() } else { preset };
                    s.crt.preset = preset.clone();
                    let list = s.crt_params.entry(preset.clone()).or_default();
                    list.retain(|(n, _)| n != name);
                    if (v - d).abs() > f32::EPSILON * 4.0 * d.abs().max(1.0) {
                        list.push((name.to_string(), v));
                    }
                    if list.is_empty() {
                        s.crt_params.remove(&preset);
                    }
                }
            }
        }
    }
    publish(ctx, s);
    if again {
        rebuild(menu, s, &ctx.crt);
    }
    note
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_fit() {
        assert_eq!(overlay::printable("a\u{263a}b"), "a?b");
        let cols = overlay::value_columns("Shader", 0);
        assert!(cols >= 40, "{cols}");
    }

    /// A small library: presets in two categories, one with many long parameters.
    fn test_pack() -> std::sync::Arc<sr_crt::Pack> {
        use sr_crt::pack::{PackWriter, ParamDef, ShaderData};
        let path = std::env::temp_dir().join(format!("sr-crt-menu-{}.pak", std::process::id()));
        let params = (0..40)
            .map(|i| ParamDef {
                name: format!("P{i}"),
                buffer: -1,
                offset: 16 + 4 * i,
                size: 4,
                min: -125.0,
                max: 125.0,
                default: 0.5,
                step: 0.005,
                description: format!("A rather long parameter description number {i} with words"),
            })
            .collect();
        let shader = ShaderData { name: "s".into(), format: String::new(), vs: vec![0], ps: vec![0], params, samplers: vec![("Source".into(), 2)] };
        {
            let mut w = PackWriter::new(std::io::BufWriter::new(std::fs::File::create(&path).unwrap())).unwrap();
            let r = w.shader("s", &shader).unwrap();
            w.preset("crt-geom", "crt", &[(r, vec![])], &[], &[]).unwrap();
            w.preset("a-very-long-preset-name-that-does-not-fit-the-value-column-at-all", "sonkun/slot-mask/flat-screen/4k", &[(r, vec![])], &[], &[]).unwrap();
            w.finish("test", &[0], Some(0)).unwrap();
        }
        std::sync::Arc::new(sr_crt::Pack::open(&path).unwrap())
    }

    #[test]
    fn tab_fits_the_panel() {
        let pack = test_pack();
        let crt = sr_crt::CrtLive::new(Default::default(), Some(pack.clone()));
        let s = Settings::default();
        let mut m = crate::menu::build(&s, &crt);
        assert!(m.layout_problems().is_empty(), "{:#?}", m.layout_problems());
        let t = m.tabs.iter().position(|t| t.title == TAB_TITLE).unwrap();
        // every option of the tab is there, the parameters after the pager
        for id in ["crt_active", "crt_library", "crt_shader", "crt_search", "crt_favorite", "crt_random", "crt_pixel", "crt_aspect", "crt_custom", "crt_scale", "crt_flip", "crt_orient", "crt_hold", "crt_snapshot", "crt_profile", "crt_save", "crt_load", "crt_reset", "crt_param:P39"] {
            assert!(m.item(id).is_some(), "{id} missing");
        }
        assert!(m.tabs[t].pager().is_some());
        // without scrolling at 720p, 1080p and 2160p (the parameters paged)
        for (w, h, scale) in [(1280, 720, 2), (960, 720, 2), (1920, 1080, 2), (2560, 1080, 2), (3840, 2160, 4)] {
            let l = Layout::new(&m, w, h);
            assert_eq!((l.scale, l.scrolls), (scale, false), "{w} x {h}");
        }
        // the long preset's name is cut to fit
        m.tab = t;
        let lib = m.tabs[t].rows.iter().find_map(|r| if let Row::Item { item, .. } = r { (item.id == "crt_shader").then(|| item.value.clone()) } else { None }).unwrap();
        if let Value::Choice { options, .. } = lib {
            assert_eq!(options.len(), 2);
            assert!(options.iter().all(|o| o.chars().count() <= overlay::value_columns("Shader", 0)));
        }
        let _ = std::fs::remove_file(&pack.path);
    }
}

#[cfg(test)]
use sr_render::overlay::Layout;
