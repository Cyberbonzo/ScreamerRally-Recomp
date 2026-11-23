use std::path::{Path, PathBuf};

fn main() {
    // A large stack reserve keeps Windows from placing the main thread's stack in the bottom
    // megabyte, so the launcher can reserve real conventional memory for the game there.
    println!("cargo:rustc-link-arg-bins=/STACK:33554432");
    // No address randomisation for the program: Windows loads it at its own base (4 MB),
    // above the conventional memory region. Randomised, it was sometimes loaded at 20000h
    // (the base is chosen per file and boot, so every start of that file got it): over the
    // region the launcher reserves (0C0000h-0FFFFFh), DOS memory fell back to virtual, and
    // the 2D pictures' hooks crashed on the game's sprite tables there (2026-11-23).
    println!("cargo:rustc-link-arg-bins=/DYNAMICBASE:NO");
    // The program's icon (Explorer, the taskbar, the windows: `sr_render::window::set_icons`):
    // assets/icon.ico as icon group 1, in a resource file written here (no resource
    // compiler needed) and given to the linker.
    let ico = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/icon.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    println!("cargo:rerun-if-changed=build.rs");
    let bytes = std::fs::read(&ico).unwrap_or_else(|e| panic!("{}: {e}", ico.display()));
    let res = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("icon.res");
    std::fs::write(&res, icon_res(&bytes)).unwrap();
    println!("cargo:rustc-link-arg-bins={}", res.display());
}

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;

/// A `.res` file holding the icons of an `.ico` file as `RT_ICON` 1..n and the
/// `RT_GROUP_ICON` 1 that lists them.
fn icon_res(ico: &[u8]) -> Vec<u8> {
    let u16_at = |o: usize| u16::from_le_bytes([ico[o], ico[o + 1]]);
    let u32_at = |o: usize| u32::from_le_bytes(ico[o..o + 4].try_into().unwrap());
    assert!(ico.len() >= 6 && u16_at(0) == 0 && u16_at(2) == 1, "not an .ico file");
    let count = u16_at(4) as usize;
    let mut out = Vec::new();
    // the empty resource every .res file starts with
    resource(&mut out, 0, 0, 0, &[]);
    let mut group = Vec::new();
    group.extend_from_slice(&[0, 0, 1, 0]);
    group.extend_from_slice(&(count as u16).to_le_bytes());
    for i in 0..count {
        let e = 6 + 16 * i;
        let (size, offset) = (u32_at(e + 8) as usize, u32_at(e + 12) as usize);
        let id = (i + 1) as u16;
        resource(&mut out, RT_ICON, id, 0x1010, &ico[offset..offset + size]);
        // the directory entry with the resource id in place of the file offset
        group.extend_from_slice(&ico[e..e + 12]);
        group.extend_from_slice(&id.to_le_bytes());
    }
    resource(&mut out, RT_GROUP_ICON, 1, 0x1030, &group);
    out
}

/// One resource with an ordinal type and name, language neutral.
fn resource(out: &mut Vec<u8>, kind: u16, name: u16, flags: u16, data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&32u32.to_le_bytes());
    out.extend_from_slice(&[0xff, 0xff]);
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&[0xff, 0xff]);
    out.extend_from_slice(&name.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // data version
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // language
    out.extend_from_slice(&0u32.to_le_bytes()); // version
    out.extend_from_slice(&0u32.to_le_bytes()); // characteristics
    out.extend_from_slice(data);
    while out.len() % 4 != 0 {
        out.push(0);
    }
}
