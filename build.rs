use std::env;

use winres::WindowsResource;

fn main() {
    if env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let mut res = WindowsResource::new();
        res.set_icon("assets/glacia-term-icon.ico");
        if let Err(e) = res.compile() {
            panic!("winres: {e}");
        }
    }
}
