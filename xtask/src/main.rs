//! Repository chores, run with `cargo xtask <task>` (an alias in `.cargo/config.toml`).

mod hero;

use resvg::{tiny_skia, usvg};
use sha2::{Digest, Sha256};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Cascadia Code (SIL Open Font License), as Google Fonts ships it: one variable-weight TTF.
/// Pinned to a google/fonts commit and checked against its SHA-256.
const FONT_URL: &str = "https://raw.githubusercontent.com/google/fonts/3dd78844021e948ceb633d1dcee3f7885561b5d9/ofl/cascadiacode/CascadiaCode%5Bwght%5D.ttf";
const FONT_SHA256: &str = "30f2e14a5c389b346b1fce110c4ddcf0dc5ce8265faf5eca2ab7323f49dba590";

const HELP: &str = "\
Usage: cargo xtask <task>

Tasks:
  hero    Render assets/hero.png (README banner, 1280×480 at 2×) and assets/social.png
          (GitHub social preview, 1280×640: upload it in Settings → General). The font is
          downloaded on first use and cached in target/xtask.
  help    Show this message.";

fn main() -> ExitCode {
    let task = std::env::args().nth(1);
    let result = match task.as_deref() {
        Some("hero") => hero(),
        None | Some("help" | "-h" | "--help") => {
            println!("{HELP}");
            Ok(())
        }
        Some(other) => Err(format!("unknown task `{other}`\n\n{HELP}").into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask is inside the workspace")
        .to_path_buf()
}

fn hero() -> Result<()> {
    let root = root();
    let mut opts = usvg::Options::default();
    // Only the downloaded font, never system fonts: the images come out the same everywhere.
    opts.fontdb_mut().load_font_data(font(&root)?);
    opts.font_family = "Cascadia Code".into();
    let assets = root.join("assets");
    std::fs::create_dir_all(&assets)?;
    for (name, w, h, zoom) in [("hero.png", 1280, 480, 2), ("social.png", 1280, 640, 1)] {
        let path = assets.join(name);
        render(&hero::svg(w, h), &opts, zoom, &path)?;
        println!("wrote {} ({}×{})", path.display(), w * zoom, h * zoom);
    }
    Ok(())
}

/// The font's bytes: from the cache in target/xtask, else downloaded (and then cached).
fn font(root: &Path) -> Result<Vec<u8>> {
    let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from);
    let cached = target.join("xtask").join("CascadiaCode[wght].ttf");
    if let Ok(bytes) = std::fs::read(&cached)
        && sha256(&bytes) == FONT_SHA256
    {
        return Ok(bytes);
    }
    println!("downloading Cascadia Code from {FONT_URL}");
    let bytes = ureq::get(FONT_URL).call()?.body_mut().read_to_vec()?;
    let got = sha256(&bytes);
    if got != FONT_SHA256 {
        return Err(format!("font download has SHA-256 {got}, expected {FONT_SHA256}").into());
    }
    std::fs::create_dir_all(cached.parent().expect("cache path has a parent"))?;
    std::fs::write(&cached, &bytes)?;
    Ok(bytes)
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn render(svg: &str, opts: &usvg::Options, zoom: u32, path: &Path) -> Result<()> {
    let tree = usvg::Tree::from_str(svg, opts)?;
    let size = tree.size().to_int_size();
    let mut pixmap = tiny_skia::Pixmap::new(size.width() * zoom, size.height() * zoom).ok_or("image size is zero")?;
    let z = zoom as f32;
    resvg::render(&tree, tiny_skia::Transform::from_scale(z, z), &mut pixmap.as_mut());
    pixmap.save_png(path)?;
    Ok(())
}
