use std::env;
use std::path::Path;

use winres::WindowsResource;

fn main() {
    if env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let ico = Path::new("assets/glacia-term-icon.ico");
        if ico.exists() {
            let mut res = WindowsResource::new();
            res.set_icon(ico.to_str().expect("non-UTF-8 path"));
            if let Err(e) = res.compile() {
                panic!("winres: {e}");
            }
        } else {
            // ICO not yet generated — run `python tools/generate_logo.py` to
            // produce it. Build proceeds without embedded icon.
            println!("cargo:warning=assets/glacia-term-icon.ico not found; binary will have no embedded icon. Run tools/generate_logo.py to fix.");
        }
    }
}
