// Hide the console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod complete;
mod editor;
mod filetree;
mod find;
mod highlight;
mod indent;
mod java;
mod lsp;
mod proc;
mod settings;
mod textops;

use eframe::egui;

fn main() -> eframe::Result {
    let settings = settings::Settings::load();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Java IDE")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([640.0, 400.0])
            .with_icon(std::sync::Arc::new(app_icon())),
        ..Default::default()
    };
    eframe::run_native(
        "Java IDE",
        options,
        Box::new(|cc| Ok(Box::new(app::IdeApp::new(cc, settings)))),
    )
}

/// A tiny procedurally drawn coffee-cup icon, so no image files are needed.
fn app_icon() -> egui::IconData {
    let size = 32u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            let (fx, fy) = (x as f32, y as f32);
            let cup = (7.0..=22.0).contains(&fx) && (12.0..=27.0).contains(&fy);
            let handle = {
                let d = ((fx - 23.0).powi(2) + (fy - 18.0).powi(2)).sqrt();
                fx > 22.0 && (3.0..=5.5).contains(&d)
            };
            let steam = (fy < 10.0 && fy > 3.0) && {
                let wave = ((fy * 0.9).sin() * 1.5) as i32;
                [11, 15, 19].iter().any(|&sx| (x as i32 - sx - wave).abs() <= 0)
            };
            let c: Option<[u8; 4]> = if cup || handle {
                Some([0xE7, 0x6F, 0x00, 0xFF])
            } else if steam {
                Some([0x5B, 0x9B, 0xD5, 0xFF])
            } else {
                None
            };
            if let Some(c) = c {
                rgba[i..i + 4].copy_from_slice(&c);
            }
        }
    }
    egui::IconData { rgba, width: size, height: size }
}
