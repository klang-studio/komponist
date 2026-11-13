//! `komponist-gallery`: every komponist UI component in every state, like
//! `Designsystem.dc.html`.
//!
//! ```text
//! komponist-gallery [--scale <pixels_per_point>]
//! ```
//!
//! With `EGUI_INSPECTION=1` the gallery serves the `egui_inspection` protocol on 127.0.0.1:5719
//! for `egui-mcp`.

use komponist_ui::RunOptions;
use komponist_ui::gallery_shell::Gallery;

fn scale_arg() -> Result<Option<f32>, String> {
    let mut args = std::env::args().skip(1);
    let mut scale = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scale" => {
                let v = args.next().ok_or("--scale needs a value")?;
                let v: f32 = v.parse().map_err(|_| format!("bad scale {v:?}"))?;
                scale = Some(v);
            }
            "-h" | "--help" => {
                println!("usage: komponist-gallery [--scale <pixels_per_point>]");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(scale)
}

fn main() -> eframe::Result {
    let scale = match scale_arg() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("komponist-gallery: {e}");
            std::process::exit(2);
        }
    };
    let mut options = RunOptions::new("komponist-gallery", "komponist · Galerie");
    options.pixels_per_point = scale;
    komponist_ui::run(options, |_cc| Gallery::designsystem())
}
