//! Embeds the community XRDB scheme pack: generates $OUT_DIR/schemes.rs with the
//! (normalized name, file contents) pairs consumed by src/themes.rs.
use std::fmt::Write as _;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=assets/schemes");

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/schemes");
    let mut entries: Vec<(String, String)> = std::fs::read_dir(&dir)
        .expect("assets/schemes missing")
        .map(|e| e.expect("read_dir entry").path())
        .filter(|p| p.is_file())
        .map(|p| {
            let stem = p.file_stem().expect("file stem").to_str().expect("utf-8 filename");
            let name = stem.to_ascii_lowercase().replace([' ', '_'], "-");
            let content = std::fs::read_to_string(&p).expect("scheme not utf-8");
            (name, content)
        })
        .collect();
    entries.sort(); // deterministic output regardless of read_dir order

    let mut out = String::from("pub(crate) static SCHEMES: &[(&str, &str)] = &[\n");
    for (name, content) in &entries {
        // {:?} yields a valid Rust string literal with proper escaping.
        writeln!(out, "    ({name:?}, {content:?}),").unwrap();
    }
    out.push_str("];\n");

    let dest = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("schemes.rs");
    std::fs::write(dest, out).expect("write schemes.rs");
}
