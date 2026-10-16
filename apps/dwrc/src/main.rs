//! Digimon Worlds Re:Compiled – one launcher for Digimon World 1, 2 and 3.
//!
//! Usage: `DigimonWorldsReCompiled [--play dw1|dw2|dw3] [mod.zip | disc.cue ...]`

#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod app;
mod audio;
mod crt;
mod emu;
mod footer;
mod input;
mod mods_page;

use eframe::egui;

fn icon() -> egui::IconData {
    // A small procedurally drawn icon: an orange rounded square with a
    // stylised "digivice" screen.
    let n = 64u32;
    let mut rgba = vec![0u8; (n * n * 4) as usize];
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f32 - 31.5, y as f32 - 31.5);
            let r = 26.0f32;
            let d = (fx.abs() - r).max(0.0).hypot((fy.abs() - r).max(0.0));
            let i = ((y * n + x) * 4) as usize;
            if d < 4.0 {
                let screen = fx.abs() < 16.0 && fy.abs() < 12.0;
                let c = if screen {
                    [40, 180, 255]
                } else {
                    [255, 140, 26]
                };
                rgba[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
    }
    egui::IconData { rgba, width: n, height: n }
}

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,wgpu=warn,naga=warn,eframe=warn")).init();
    let args: Vec<String> = std::env::args().collect();
    let play_at = args.iter().position(|a| a == "--play");
    let autostart = play_at.and_then(|i| args.get(i + 1)).and_then(|s| dwrc_disc::GameId::from_slug(s));
    // Files passed on the command line (or dropped on the program's icon):
    // mod .zip files are installed, disc images imported.
    let open_files: Vec<std::path::PathBuf> = args
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(i, a)| !a.starts_with("--") && play_at.is_none_or(|p| *i != p + 1))
        .map(|(_, a)| std::path::PathBuf::from(a))
        .collect();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Digimon Worlds Re:Compiled")
            .with_inner_size([1280.0, 860.0])
            .with_min_inner_size([640.0, 480.0])
            .with_icon(icon()),
        vsync: true,
        ..Default::default()
    };
    eframe::run_native("Digimon Worlds Re:Compiled", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, autostart, open_files)))))
}
