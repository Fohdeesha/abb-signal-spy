#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod browser;
mod channels;
mod charts;
mod compare;
mod dashboard;
mod derived_view;
mod export;
mod fields;
mod net;
mod notes;
mod oom;
mod paths;
mod phone;
mod record;
mod review_view;
mod rws_view;
mod sets;
mod settings;
mod status;
mod theme;
mod view;
mod xy;

#[cfg(test)]
mod shots;
#[cfg(test)]
mod ui_tests;

use std::path::PathBuf;

use eframe::egui;

#[global_allocator]
static ALLOCATOR: oom::NoteOnRefusal<std::alloc::System> = oom::NoteOnRefusal(std::alloc::System);

fn install_crash_file(dir: PathBuf) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let text = format!(
            "ABB Signal Spy {} crashed at {}\n{info}\n\n{}\n",
            env!("CARGO_PKG_VERSION"),
            spy_core::util::wall_iso(std::time::SystemTime::now()),
            std::backtrace::Backtrace::force_capture()
        );
        let _ = std::fs::write(dir.join("crash.txt"), text);
        default(info);
    }));
}

fn options(renderer: eframe::Renderer) -> eframe::NativeOptions {
    eframe::NativeOptions {
        renderer,
        viewport: egui::ViewportBuilder::default().with_title("ABB Signal Spy").with_inner_size([1400.0, 860.0]).with_min_inner_size([900.0, 560.0]),
        persist_window: true,
        ..Default::default()
    }
}

fn connect_arg() -> Option<spy_core::session::Target> {
    let args: Vec<String> = std::env::args().collect();
    let i = args.iter().position(|a| a == "--connect")?;
    let spec = args.get(i + 1)?;
    let (host, port) = match spec.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h.to_string(), p.parse().ok()?),
        _ => (spec.clone(), spy_core::session::ROBAPI_PORT),
    };
    Some(spy_core::session::Target { host, port })
}

fn review_arg() -> Option<PathBuf> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut skip_next = false;
    for a in &args {
        if std::mem::take(&mut skip_next) {
            continue;
        }
        if a == "--connect" {
            skip_next = true;
            continue;
        }
        if let Some(d) = review_view::recording_dir(std::path::Path::new(a)) {
            return Some(d);
        }
    }
    None
}

fn main() {
    let data_dir = paths::data_dir();
    install_crash_file(data_dir.clone());
    oom::prepare(&data_dir);
    if std::env::var_os("ABB_SIGNAL_SPY_TEST_OUT_OF_MEMORY").is_some() {
        let p = std::hint::black_box(unsafe { std::alloc::alloc(std::alloc::Layout::from_size_align(1 << 50, 8).expect("a valid layout")) });
        std::process::exit(if p.is_null() { 3 } else { 4 });
    }
    let connect = connect_arg();
    let review = review_arg();

    let mut errors = Vec::new();
    #[cfg(feature = "dx12")]
    let renderers = [eframe::Renderer::Wgpu, eframe::Renderer::Glow];
    #[cfg(not(feature = "dx12"))]
    let renderers = [eframe::Renderer::Glow];
    for renderer in renderers {
        let dir = data_dir.clone();
        let connect = connect.clone();
        let review = review.clone();
        let result = eframe::run_native(
            "ABB Signal Spy",
            options(renderer),
            Box::new(move |cc| {
                let mut a = app::SpyApp::new(cc, dir, net::Windows::join(net::WINDOW_SLOTS));
                if let Some(t) = connect {
                    a.host_input = t.host;
                    a.port_input = t.port.to_string();
                    a.connect();
                }
                if let Some(d) = review {
                    a.open_recording(d);
                }
                Ok(Box::new(a))
            }),
        );
        match result {
            Ok(()) => {
                oom::survived(&data_dir, oom::noted());
                return;
            }
            Err(e) => errors.push(format!("{renderer:?}: {e}")),
        }
    }
    net::fatal_box(&format!(
        "ABB Signal Spy could not open its window.\n\n{}\n\nThe graphics driver may be missing or too old. Details are in {}.",
        errors.join("\n"),
        data_dir.display()
    ));
    let _ = std::fs::write(data_dir.join("startup-error.txt"), errors.join("\n"));
    std::process::exit(1);
}
