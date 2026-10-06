use std::path::PathBuf;

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const ICON_FLAGS: u16 = 0x1010;
const GROUP_FLAGS: u16 = 0x1030;
const LANGUAGE_EN_US: u16 = 0x0409;
const APP_ICON_ID: u16 = 1;

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os != "windows" || env != "msvc" {
        return;
    }
    let ico = std::fs::read("assets/icon.ico").expect("cannot read assets/icon.ico");
    let res = icon_resource(&ico).expect("assets/icon.ico is not a well-formed icon file");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is not set")).join("icon.res");
    std::fs::write(&out, res).expect("cannot write icon.res");
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

fn entry(out: &mut Vec<u8>, kind: u16, id: u16, data: &[u8], flags: u16, language: u16) {
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&32u32.to_le_bytes());
    out.extend_from_slice(&0xffffu16.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&0xffffu16.to_le_bytes());
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&language.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(data);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

fn icon_resource(ico: &[u8]) -> Option<Vec<u8>> {
    if u16_at(ico, 0)? != 0 || u16_at(ico, 2)? != 1 {
        return None;
    }
    let count = u16_at(ico, 4)?;
    if count == 0 {
        return None;
    }
    let mut out = Vec::new();
    entry(&mut out, 0, 0, &[], 0, 0);
    let mut group = Vec::new();
    group.extend_from_slice(&0u16.to_le_bytes());
    group.extend_from_slice(&1u16.to_le_bytes());
    group.extend_from_slice(&count.to_le_bytes());
    for i in 0..usize::from(count) {
        let at = 6 + 16 * i;
        let size = u32_at(ico, at + 8)? as usize;
        let offset = u32_at(ico, at + 12)? as usize;
        let image = ico.get(offset..offset.checked_add(size)?)?;
        let id = u16::try_from(i + 1).ok()?;
        entry(&mut out, RT_ICON, id, image, ICON_FLAGS, LANGUAGE_EN_US);
        group.extend_from_slice(ico.get(at..at + 4)?);
        group.extend_from_slice(ico.get(at + 4..at + 8)?);
        group.extend_from_slice(&(size as u32).to_le_bytes());
        group.extend_from_slice(&id.to_le_bytes());
    }
    entry(&mut out, RT_GROUP_ICON, APP_ICON_ID, &group, GROUP_FLAGS, LANGUAGE_EN_US);
    Some(out)
}
