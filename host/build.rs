// Gives the program file its icon (Explorer, Start, pinned taskbar buttons), drawn by the same
// code as the tray and window icons, and its details (name and version: Task Manager, startup
// apps, Properties), named by src/product.rs like everything else that shows the program.

mod icon {
    include!("src/icon.rs");
}

mod product {
    include!("src/product.rs");
}

/// A Windows .ico file with uncompressed 32-bit entries, one per size.
fn ico_file(sizes: &[u32]) -> Vec<u8> {
    let mut images = Vec::new();
    for &s in sizes {
        let px = icon::rgba(s, false);
        let mut dib = Vec::new();
        // BITMAPINFOHEADER: the height counts the colour rows and the mask rows.
        dib.extend_from_slice(&40u32.to_le_bytes());
        dib.extend_from_slice(&(s as i32).to_le_bytes());
        dib.extend_from_slice(&((s * 2) as i32).to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&32u16.to_le_bytes());
        dib.extend_from_slice(&[0u8; 24]);
        for y in (0..s).rev() {
            for x in 0..s {
                let o = ((y * s + x) * 4) as usize;
                dib.extend_from_slice(&[px[o + 2], px[o + 1], px[o], px[o + 3]]);
            }
        }
        // The AND mask is unused with an alpha channel, but must be there: rows padded to 4 bytes.
        let row = s.div_ceil(32) * 4;
        dib.extend(std::iter::repeat_n(0u8, (row * s) as usize));
        images.push((s, dib));
    }
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (s, dib) in &images {
        let dim = if *s >= 256 { 0 } else { *s as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(dib.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += dib.len() as u32;
    }
    for (_, dib) in images {
        out.extend_from_slice(&dib);
    }
    out
}

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=src/product.rs");
    println!("cargo:rerun-if-changed=build.rs");
    let os = std::env::var("CARGO_CFG_TARGET_OS");
    println!("cargo:rustc-check-cfg=cfg(desktop_host)");
    println!("cargo:rustc-check-cfg=cfg(browser_workspace)");
    if matches!(os.as_deref(), Ok("linux" | "macos" | "windows")) {
        println!("cargo:rustc-cfg=browser_workspace");
    }
    if matches!(os.as_deref(), Ok("windows" | "macos")) {
        println!("cargo:rustc-cfg=desktop_host");
    }
    if os.as_deref() != Ok("windows") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico = out.join("deskway.ico");
    std::fs::write(&ico, ico_file(&[16, 20, 24, 32, 40, 48, 64, 256])).unwrap();
    let rc = out.join("deskway.rc");
    let path = ico.display().to_string().replace('\\', "\\\\");
    let name = product::NAME;
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let numbers = version.replace('.', ",");
    std::fs::write(
        &rc,
        format!(
            "1 ICON \"{path}\"\n\
             1 VERSIONINFO\nFILEVERSION {numbers},0\nPRODUCTVERSION {numbers},0\nBEGIN\n\
             BLOCK \"StringFileInfo\"\nBEGIN\nBLOCK \"040904B0\"\nBEGIN\n\
             VALUE \"FileDescription\", \"{name}\"\nVALUE \"ProductName\", \"{name}\"\n\
             VALUE \"FileVersion\", \"{version}\"\nVALUE \"ProductVersion\", \"{version}\"\nEND\nEND\n\
             BLOCK \"VarFileInfo\"\nBEGIN\nVALUE \"Translation\", 0x409, 1200\nEND\nEND\n"
        ),
    )
    .unwrap();
    embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().unwrap();
}
