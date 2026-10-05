//! ABB Signal Spy: reads, charts and records the motion test signals an ABB IRC5
//! controller streams over RobAPI InfoStream. Read-only against controllers.
//!
//! Not affiliated with or endorsed by ABB. ABB and IRC5 are trademarks of ABB.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod browser;
mod channels;
mod charts;
mod compare;
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
mod theme;
mod view;
mod xy;

#[cfg(test)]
mod ui_tests;

use std::path::PathBuf;

use eframe::egui;

/// The system's allocator, leaving a note when memory is refused (oom.rs).
#[global_allocator]
static ALLOCATOR: oom::NoteOnRefusal<std::alloc::System> = oom::NoteOnRefusal(std::alloc::System);

fn install_crash_file(dir: PathBuf) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Written first, in case what follows fails too.
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
        // Window position and size are remembered; everything else lives in our
        // own settings file.
        persist_window: true,
        ..Default::default()
    }
}

/// `--connect HOST[:PORT]`: connect at startup (a desktop shortcut to one controller).
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

/// A recording folder (or a file in one) given as an argument: what Windows passes
/// when a recording's folder is dropped onto the program's icon.
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
    // For tests/out_of_memory.rs: a request no PC can grant, through the global
    // allocator, then stop (with an exit code: Rust's own stop would leave a crash dump
    // behind at every test run).
    if std::env::var_os("ABB_SIGNAL_SPY_TEST_OUT_OF_MEMORY").is_some() {
        // SAFETY: a valid layout of non-zero size; nothing is written to the result.
        // Through `black_box`: an allocation whose pointer is only compared with null is
        // one the optimiser may assume succeeds and remove (the release build did).
        let p = std::hint::black_box(unsafe { std::alloc::alloc(std::alloc::Layout::from_size_align(1 << 50, 8).expect("a valid layout")) });
        std::process::exit(if p.is_null() { 3 } else { 4 });
    }
    let another = net::another_instance();
    let connect = connect_arg();
    let review = review_arg();

    // wgpu (DirectX 12) first; if it cannot start, OpenGL: a remote desktop session
    // or a PC without a usable graphics driver should still get a window. The
    // Windows 7 build has OpenGL only.
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
                let mut a = app::SpyApp::new(cc, dir, another);
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
            Ok(()) => return,
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
