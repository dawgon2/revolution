//! FilmCraft desktop app.
//!
//! Usage: `filmcraft [--control <port>] [--demo|--empty] [--recover|--no-recover] [--data-dir <dir>]
//! [project.fcproj | media files…]`; `--help` prints the options, `--version` the version. An
//! unknown option is an error (exit code 2), not a file to open.
//!
//! Without a project, `--demo` or `--empty`, Settings ▸ General ▸ At startup show decides: the
//! Project Manager home screen, the last project or a new empty project; its "workflow default"
//! is the Project Manager in the Resolve workflow and At Startup (Show Home = the demo project,
//! Open Most Recent, or an empty project) in the Premiere workflow.
//!
//! `--control <port>` (or `FILMCRAFT_CONTROL_PORT`) starts a localhost JSON-lines control server;
//! see `filmcraft_ui_egui::control` for the methods. A port that is not a number is an error from
//! either source.
//!
//! Auto-save and the crash-recovery journal run in every session (data in `--data-dir`, else
//! `FILMCRAFT_DATA_DIR`, else the per-user application data folder). If a previous session died
//! with unsaved changes the app asks to recover them; `--recover` recovers the newest without
//! asking, `--no-recover` starts without asking (the changes stay available via File ▸ Recover
//! Unsaved Changes…).
//!
//! `log` records go to standard error and `<data dir>/Logs/filmcraft.log` (`RUST_LOG` sets the
//! levels); see [`logging`].
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod app_nap;
mod appearance;
mod args;
mod audio;
mod audio_in;
#[cfg(test)]
mod audio_soak;
#[cfg(target_os = "linux")]
mod cards;
mod control_server;
mod desktop;
#[cfg(target_os = "linux")]
mod dev_icon;
mod file_filters;
#[cfg(target_os = "linux")]
mod global_menu;
#[cfg(target_os = "linux")]
mod global_shortcuts;
#[cfg(any(target_os = "windows", test))]
mod graphics;
mod icon_style;
#[cfg(target_os = "linux")]
mod journal;
#[cfg(target_os = "linux")]
mod kdeconnect;
mod keyring;
mod logging;
#[cfg(target_os = "linux")]
mod mpris;
#[cfg(target_os = "macos")]
mod native_menu;
#[cfg(target_os = "linux")]
mod recent_files;
mod single_instance;
#[cfg(target_os = "linux")]
mod vk_select;
mod window_memory;
mod window_raise;

use args::{Cli, Launch};
use chrono::TimeZone;
use filmcraft_engine::Session;
use filmcraft_engine::autosave::{AutosaveConfig, default_data_dir};
use filmcraft_engine::window_state::WindowState;
use filmcraft_ui_egui::FilmcraftApp;
use serde_json::json;

/// Reverse-DNS app id: Linux `.desktop` file, hicolor icon name and Wayland app id.
const APP_ID: &str = "io.github.dawgon2.Revolution"; // fork rebrand, FORK-PLAN §1a.103

/// Print `text` (stdout, or stderr for an error). A Windows release build has no console
/// (`windows_subsystem = "windows"`), so there it is also shown in a message box: otherwise `--help`
/// would print nothing and a bad option would quit without a word.
fn tell(text: &str, error: bool) {
    if error {
        eprint!("{text}");
    } else {
        print!("{text}");
    }
    #[cfg(all(target_os = "windows", not(debug_assertions)))]
    {
        let level = if error { rfd::MessageLevel::Error } else { rfd::MessageLevel::Info };
        let _ = rfd::MessageDialog::new().set_level(level).set_title("Revolution").set_description(text).show();
    }
}

fn main() -> eframe::Result {
    // First, so the panic hook and every start-up record are logged (`logging`).
    let logger = logging::install();
    // lossy, so a value that is not Unicode is reported like any other bad port instead of ignored
    let env_port = std::env::var_os("FILMCRAFT_CONTROL_PORT").map(|p| p.to_string_lossy().into_owned());
    let Launch { control_port, control_input_only, files, demo, startup_flag, recover, data_dir, new_instance } =
        match args::parse(env_port, std::env::args().skip(1)) {
            Ok(Cli::Run(l)) => l,
            Ok(Cli::Help) => {
                tell(args::USAGE, false);
                return Ok(());
            }
            Ok(Cli::Version) => {
                tell(&format!("filmcraft {}\n", env!("CARGO_PKG_VERSION")), false);
                return Ok(());
            }
            Err(e) => {
                tell(&format!("filmcraft: {e}\n\n{}", args::USAGE), true);
                std::process::exit(2);
            }
        };
    // fork §1a.105 (b): on the Intel GPU, start again without NVIDIA's Vulkan driver (which woke a
    // sleeping RTX 4050 for ~20 s); first, before anything is opened
    #[cfg(target_os = "linux")]
    {
        let prefs = data_dir.clone().or_else(default_data_dir).map(|d| filmcraft_engine::autosave::Preferences::load(&d.join("preferences.json")));
        vk_select::restart_on_the_intel_driver_if_wanted(&prefs.unwrap_or_default().playback.graphics_device);
    }
    // the whole process uses `--data-dir` (models are found without the launch arguments)
    if let Some(d) = &data_dir {
        filmcraft_engine::autosave::set_launch_data_dir(d);
    }
    // remote locations' passwords (STORAGE-PLAN S2): the system keyring, connected on first use
    keyring::install(data_dir.clone().or_else(default_data_dir));
    // One FilmCraft per data folder (L1.7): a second launch hands its files to the running one
    // and exits here, before the log file is opened (opening it rotates the running instance's
    // log) and before any window or decoder starts. Agents (`--control`) and `--new-instance`
    // always start their own.
    let desktop_shared = std::sync::Arc::new(desktop::Shared::default());
    if control_port.is_none() && !new_instance {
        let new_project = startup_flag && !demo;
        match single_instance::claim(data_dir.clone().or_else(default_data_dir).as_deref(), &files, new_project, &desktop_shared) {
            single_instance::Start::HandedOff => return Ok(()),
            // kept until the app closes (`DesktopHost::shutdown` releases the name)
            single_instance::Start::Run(holder) => desktop_shared.keep(holder),
        }
    }
    // `<state dir>/Logs`: `$XDG_STATE_HOME/filmcraft/Logs` for a default Linux install, else
    // `<data dir>/Logs` (an explicit `--data-dir` keeps everything in one folder; `dirs`). An
    // older version's `<data dir>/Logs` and `Media Cache` move there first.
    let dirs = data_dir.clone().or_else(default_data_dir).map(|d| filmcraft_engine::dirs::AppDirs::for_data_dir(&d));
    let moved = dirs.as_ref().map(filmcraft_engine::dirs::AppDirs::migrate).unwrap_or_default();
    let log_dir = dirs.as_ref().map(filmcraft_engine::dirs::AppDirs::logs);
    log::info!("Revolution {} ({} {})", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH);
    for line in moved {
        log::info!("{line}");
    }
    // The log file lives next to the crash logs; opened after the arguments, so `--help`,
    // `--version` and usage errors leave no file behind. Records logged until now go in first.
    if let Some(logger) = logger {
        match log_dir.as_deref().map(|d| logger.attach_dir(d)) {
            Some(Ok(path)) => log::info!("log file {}", path.display()),
            Some(Err(e)) => log::warn!("no log file: {e}"),
            None => {
                logger.stderr_only();
                log::warn!("no log file: no data directory (set FILMCRAFT_DATA_DIR or --data-dir)");
            }
        }
    }
    app_nap::disable();
    #[cfg(target_os = "linux")]
    dev_icon::ensure_dev_desktop_entry();
    // Panics anywhere go to <state dir>/Logs/crash-<day>.log with a backtrace; the UI pass and
    // frame workers catch them and keep running (see filmcraft_ui_egui::crash).
    filmcraft_ui_egui::crash::install(log_dir);
    // Taskbar progress, notifications, idle / suspend inhibit (L1.7). Off for agents unless
    // FILMCRAFT_DESKTOP_INTEGRATION=1 (a scripted run shouldn't pop notifications).
    let desktop_integration = control_port.is_none() || std::env::var_os("FILMCRAFT_DESKTOP_INTEGRATION").is_some_and(|v| v == "1");
    // The preferences as saved (the session loads them again; these choose how the window opens)
    let prefs = data_dir.clone().or_else(default_data_dir).map(|d| filmcraft_engine::autosave::Preferences::load(&d.join("preferences.json")));
    let prefs = prefs.unwrap_or_default();
    // Settings ▸ Playback ▸ Graphics device (L2.6): VA-API decodes on the chosen GPU first, so
    // decoded frames are drawn in place on it (a frame from another GPU is copied instead); export
    // devices follow the same choice (fork XPS a)
    filmcraft_gpu::adapter_pick::set_preference(filmcraft_ui_egui::graphics_device::preference(&prefs.playback.graphics_device));
    // Auto on a hybrid Intel + NVIDIA laptop draws on the Intel GPU (fork XPS item 17)
    filmcraft_gpu::adapter_pick::set_battery_powered(filmcraft_engine::power::has_battery());
    #[cfg(target_os = "linux")]
    if let Some((vendor, device)) = filmcraft_ui_egui::graphics_device::parse(&prefs.playback.graphics_device) {
        filmcraft_platform::va::prefer_gpu(vendor, device);
    }
    // OS hardware video decoders (VideoToolbox on macOS) in front of our own; Settings ▸ Playback ▸
    // Hardware decoding switches them off. Unsupported streams and failures use our decoders.
    register_hardware_decoders();
    register_gpu_frame_renderer();
    // The window's size and maximised state from the last run (L1.3; `<state dir>/window.json`).
    // Agents (`--control`) always get the default size, so their screenshots stay comparable.
    let window_file = dirs.as_ref().filter(|_| control_port.is_none()).map(|d| WindowState::path(&d.state));
    let window = window_file.as_deref().and_then(WindowState::load).unwrap_or_default();
    // Settings ▸ General ▸ Window title bar (fork): open without the desktop's title bar when
    // FilmCraft draws its own, so it doesn't flash up first (the app follows later changes)
    let own_title_bar = filmcraft_ui_egui::window_chrome::wants_own(&prefs.general.title_bar);
    // Settings ▸ General ▸ Full screen / Project Manager full screen: open the way the first
    // screen wants it (the Project Manager when no project is named and the startup setting shows
    // it), never full screen for an agent
    let opens_project_manager = !files.iter().any(|f| f.ends_with(".fcproj"))
        && filmcraft_engine::projects::startup_action(&prefs, startup_flag, demo) == filmcraft_engine::projects::StartupAction::ProjectManager;
    // the Project Manager opens in a plain window, never full screen or maximised (owner,
    // 2026-10-20, FORK-PLAN §1a.108); the editor's state comes back when a project opens
    let pm_windowed = opens_project_manager && control_port.is_none();
    let wanted = if opens_project_manager { false } else { prefs.general.full_screen };
    let full_screen = wanted && control_port.is_none();
    let (start_size, start_maximized) =
        if pm_windowed { (filmcraft_engine::window_state::PROJECT_MANAGER_SIZE, false) } else { (window.inner_size, window.maximized) };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Revolution")
            .with_inner_size(start_size)
            .with_maximized(start_maximized)
            .with_min_inner_size(filmcraft_engine::window_state::MIN_SIZE)
            .with_drag_and_drop(true)
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false)
            .with_decorations(!own_title_bar)
            .with_fullscreen(full_screen)
            // Started for an agent (`--control`): open without taking the user's keyboard focus.
            .with_active(control_port.is_none())
            .with_icon(icon_style::window_icon(&prefs.appearance.app_icon))
            // Wayland matches the window to `packaging/linux/io.github.dawgon2.Revolution.desktop`.
            .with_app_id(APP_ID),
        event_loop_builder: agent_event_loop(control_port.is_some()),
        // Settings ▸ Playback ▸ Graphics device (L2.6)
        wgpu_options: filmcraft_ui_egui::wgpu_configuration_on(&prefs.playback.graphics_device),
        ..Default::default()
    };
    // Before eframe creates the wgpu instance: leave OpenGL out on Windows (see graphics.rs).
    #[cfg(target_os = "windows")]
    let options = {
        let mut options = options;
        graphics::configure(&mut options, eframe::wgpu::Backends::from_env());
        options
    };
    // a lost window GPU device: start again, recovering the session (egui-wgpu can't draw on it)
    #[cfg(unix)]
    filmcraft_ui_egui::set_window_gpu_lost_hook(Box::new(restart_after_gpu_loss));
    let started = eframe::run_native(
        "Revolution",
        options,
        Box::new(move |cc| {
            let mut session = Session::default();
            // automatic proxies (fork 6.2) only in the desktop app: a CLI / headless session that
            // exits would leave half-written proxies in the media folders
            session.auto_proxies.enabled = true;
            if let Some(dir) = data_dir.clone().or_else(default_data_dir) {
                let mut cfg = AutosaveConfig::new(dir);
                cfg.local_offset = local_offset;
                if let Err(e) = session.start_autosave(cfg) {
                    log::warn!("auto-save and crash recovery unavailable: {e}");
                }
            }
            // FORK-ONLY (RS1): without a data directory the default workflow (Resolve) gets its keys and look
            filmcraft_engine::fork_resolve::without_data_dir(&mut session);
            // voice-over recording reads the microphone through cpal
            session.voiceover.input = Some(Box::new(audio_in::CpalIn::new(&session.prefs.audio_hardware.device_class)));
            let project = files.iter().find(|f| f.ends_with(".fcproj")).cloned();
            // the named project, else --demo / --empty, else Settings ▸ General ▸ At startup show
            // (the Project Manager home screen is shown once the app exists, below)
            let startup = filmcraft_ui_egui::launch_project(&mut session, project.as_deref(), startup_flag, demo);
            let media: Vec<String> = files.iter().filter(|f| !f.ends_with(".fcproj")).cloned().collect();
            if !media.is_empty() {
                let _ = session.execute("file.import", json!({"paths": media}));
            }
            if recover == Some(true) && !session.recovery_candidates().is_empty() {
                let id = session.recovery_candidates()[0].id.clone();
                match session.execute("file.recover", json!({"id": id})) {
                    Ok(r) => log::info!("recovered {r}"),
                    Err(e) => log::error!("recovery failed: {e}"),
                }
            }
            let mut app = FilmcraftApp::new(session);
            app.finish_launch(startup, !media.is_empty());
            // Settings ▸ Appearance ▸ App icon (fork §1a.108)
            app.hooks.app_icon_changed = Some(Box::new(icon_style::apply));
            if pm_windowed {
                app.launch_project_manager_windowed(window);
            }
            // the window opened in the first screen's full-screen state already
            app.full_screen_at_start = control_port.is_none();
            if recover == Some(false) {
                app.dialog = None;
            }
            app.integrated_titlebar = cfg!(target_os = "macos");
            if let Some(rs) = cc.wgpu_render_state.clone()
                && std::env::var_os("FILMCRAFT_CPU_COMPOSITE").is_none()
            {
                app.set_wgpu(rs);
            }
            // Keep device selection available even when the default device is unavailable.
            // Settings ▸ Audio Hardware is applied on the first frame (`apply_prefs`).
            app.audio = Some(Box::new(audio::CpalOut::new()));
            app.hooks.pick_files = Some(Box::new(|exts: &[&str]| {
                rfd::FileDialog::new()
                    .add_filter(filmcraft_ui_egui::i18n::t("Media"), &file_filters::extensions(exts))
                    .pick_files()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|p| p.to_string_lossy().to_string())
                    .collect()
            }));
            // Link Media ▸ Locate…, Attach Proxies, Reconnect Full Resolution: one path, not imported.
            app.hooks.pick_file_for_relink = Some(Box::new(|exts: &[&str], _hint| {
                rfd::FileDialog::new()
                    .add_filter(filmcraft_ui_egui::i18n::t("Media"), &file_filters::extensions(exts))
                    .pick_file()
                    .map(|p| p.to_string_lossy().to_string())
            }));
            app.hooks.pick_save = Some(Box::new(|name: &str| {
                rfd::FileDialog::new()
                    .add_filter(filmcraft_ui_egui::i18n::t("Revolution Project"), &file_filters::extensions(&["fcproj"]))
                    .set_file_name(name)
                    .save_file()
                    .map(|p| p.to_string_lossy().to_string())
            }));
            app.hooks.pick_save_as = Some(Box::new(|filter: &str, exts: &[&str], name: &str| {
                rfd::FileDialog::new()
                    .add_filter(filmcraft_ui_egui::i18n::t(filter), &file_filters::extensions(exts))
                    .set_file_name(name)
                    .save_file()
                    .map(|p| p.to_string_lossy().to_string())
            }));
            app.hooks.pick_folder = Some(Box::new(|| rfd::FileDialog::new().pick_folder().map(|p| p.to_string_lossy().to_string())));
            app.hooks.pick_folder_at =
                Some(Box::new(|dir: &str| rfd::FileDialog::new().set_directory(dir).pick_folder().map(|p| p.to_string_lossy().into_owned())));
            // Linux: the XDG portal call blocks until the user chooses, which would stop the frame
            // loop (GNOME offers to force-quit the window after about five seconds, #260). Show the
            // dialog on its own thread instead; the app keeps running and acts on the answer.
            // Fork L1.5: the dialog is a child of this window (the portal stacks it over the
            // window and keeps it modal; on Wayland through xdg-foreign) and starts in the folder
            // that kind of dialog was last used in.
            #[cfg(target_os = "linux")]
            {
                let parent = rfd::FileDialog::new().set_parent(cc);
                app.hooks.run_dialog_from = Some(Box::new(move |dialog, start| run_dialog_off_thread(parent.clone(), dialog, start)));
            }
            // listed on a thread now; Settings shows what's listed (never waits for a driver)
            filmcraft_ui_egui::graphics_device::warm();
            app.hooks.graphics_devices = Some(Box::new(filmcraft_ui_egui::graphics_device::listed));
            app.hooks.open_path = Some(Box::new(open_path));
            // Help menu, About and Discord links (#642): eframe is built without its `links`
            // feature, so `ctx.open_url` does nothing on the desktop. Detached, so a launcher
            // that waits for the browser never freezes the UI.
            app.hooks.open_url = Some(Box::new(|url: &str| open::that_detached(url).map_err(|e| e.to_string())));
            app.hooks.clipboard_media = Some(Box::new(clipboard_media));
            #[cfg(target_os = "linux")]
            if desktop_integration {
                app.hooks.desktop = Some(Box::new(desktop::LinuxDesktop::start(desktop_shared.clone())));
            }
            #[cfg(not(target_os = "linux"))]
            let _ = (desktop_integration, &desktop_shared);
            // Settings ▸ General ▸ Interface Language ▸ System Language (#218).
            app.hooks.cursor_screen_position = Some(Box::new(filmcraft_platform::cursor::cursor_screen_position));
            app.hooks.system_languages = Some(Box::new(|| sys_locale::get_locales().collect()));
            // Settings ▸ Appearance ▸ Appearance Mode ▸ Sync with system on Linux desktops whose
            // compositor reports no theme to winit (no polling: see appearance.rs).
            // ▸ Use the system accent color reads the desktop's accent colour the same way (L1.4),
            // ▸ Match the desktop's text size its font size.
            if let Some(service) = appearance::service() {
                app.hooks.system_theme = Some(service.theme);
                app.hooks.system_accent = Some(service.accent);
                app.hooks.system_reduced_motion = Some(service.reduced_motion);
                app.hooks.system_text_scale = Some(service.text_scale);
            }
            app.hooks.raise_without_focus = Some(Box::new(|| {
                window_raise::raise_without_focus();
            }));
            app.hooks.pick_open_file = Some(Box::new(|filter: &str, exts: &[&str]| {
                rfd::FileDialog::new()
                    .add_filter(filmcraft_ui_egui::i18n::t(filter), &file_filters::extensions(exts))
                    .pick_file()
                    .map(|p| p.to_string_lossy().to_string())
            }));
            app.hooks.pick_open_project = Some(Box::new(|| {
                rfd::FileDialog::new()
                    .add_filter(filmcraft_ui_egui::i18n::t("Revolution Project"), &file_filters::extensions(&["fcproj"]))
                    .pick_file()
                    .map(|p| p.to_string_lossy().to_string())
            }));
            #[cfg(target_os = "macos")]
            {
                let (rx, update) = native_menu::install(&app, cc.egui_ctx.clone());
                app.command_inbox = Some(rx);
                app.hooks.shortcuts_changed = Some(update);
                app.ui.show_menu_bar = false;
            }
            // Linux: the menus in Plasma's Global Menu / macOS-style top panel (Settings ▸ General ▸
            // Menu bar); the window's own menu bar stays where there is none
            #[cfg(target_os = "linux")]
            if desktop_integration && let Some(g) = global_menu::install(cc) {
                app.command_inbox = Some(g.commands);
                app.hooks.menu_changed = Some(g.changed);
                app.global_menu = Some(g.shown);
                // lives as long as the process (the window closes only at exit)
                Box::leak(Box::new(g.menu));
            }
            if let Some(port) = control_port {
                let rx = control_server::start(port, cc.egui_ctx.clone());
                app = if control_input_only { app.with_control_input_only(rx) } else { app.with_control(rx) };
            }
            Ok(match window_file {
                // the Project Manager's window is not the editor's: only the editor's is remembered
                Some(path) => Box::new(window_memory::WindowMemory::new(app, path, window, |a: &FilmcraftApp| !a.window_is_editors())) as Box<dyn eframe::App>,
                None => Box::new(app),
            })
        }),
    );
    // The window could not be created (no usable GPU / graphics driver, no display): say so
    // instead of exiting silently, and keep the reason in the crash log.
    if let Err(e) = &started {
        let msg = format!("Revolution could not start its window: {e}\n\nUpdating the graphics driver usually fixes this.");
        log::error!("{msg}");
        filmcraft_ui_egui::crash::record(&msg);
        let _ = rfd::MessageDialog::new().set_title("Revolution").set_description(&msg).set_level(rfd::MessageLevel::Error).show();
    }
    // background hardware probes finish before the drivers' exit handlers run (U0.2.4)
    filmcraft_platform::quiesce();
    started
}

/// Files copied in the file manager, else an image, on the system clipboard (Paste, #611).
fn clipboard_media() -> Option<filmcraft_ui_egui::ClipboardMedia> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    if let Ok(files) = clipboard.get().file_list()
        && !files.is_empty()
    {
        return Some(filmcraft_ui_egui::ClipboardMedia::Files(files.iter().map(|p| p.to_string_lossy().into_owned()).collect()));
    }
    let image = clipboard.get_image().ok()?;
    Some(filmcraft_ui_egui::ClipboardMedia::Image { width: image.width, height: image.height, rgba: image.bytes.into_owned() })
}

/// Open a file in its default application, or reveal it in the file manager (Edit Original,
/// Reveal Log Files).
fn open_path(path: &str, reveal: bool) -> Result<(), String> {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        if reveal {
            c.arg("-R");
        }
        c.arg(path);
        c
    } else if cfg!(target_os = "windows") {
        let mut c = std::process::Command::new("explorer");
        if reveal {
            c.arg(format!("/select,{path}"));
        } else {
            c.arg(path);
        }
        c
    } else {
        // the file manager selects the file (FileManager1 over D-Bus), else its folder opens
        #[cfg(target_os = "linux")]
        if reveal {
            return desktop::reveal(path);
        }
        let mut c = std::process::Command::new("xdg-open");
        let p = std::path::Path::new(path);
        c.arg(if reveal { p.parent().unwrap_or(p) } else { p });
        c
    };
    cmd.spawn().map(|_| ()).map_err(|e| format!("can't open {path}: {e}"))
}

/// Set on a process started again after its window's GPU was lost (how many times).
const GPU_LOST_RESTARTS: &str = "FILMCRAFT_GPU_LOST_RESTARTS";

/// The window's GPU device was lost (`why`): replace this process with a fresh start that
/// recovers the session from the crash-recovery journal (`--recover`), instead of the panic
/// egui-wgpu meets drawing on the lost device. A second loss starts on the CPU compositor; a
/// third doesn't restart.
#[cfg(unix)]
fn restart_after_gpu_loss(why: &str) {
    use std::os::unix::process::CommandExt;
    let restarts: u32 = std::env::var(GPU_LOST_RESTARTS).ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let Some((args, env)) = gpu_loss_restart(std::env::args_os().skip(1).collect(), restarts) else {
        log::error!("the GPU was lost again ({why}); not restarting a third time");
        return;
    };
    let msg = format!("the GPU stopped ({why}); Revolution restarts and recovers the open work");
    log::error!("{msg}");
    filmcraft_ui_egui::crash::record(&msg);
    let Ok(exe) = std::env::current_exe() else { return };
    let err = std::process::Command::new(exe).args(args).envs(env).exec();
    log::error!("restarting after the GPU loss failed: {err}");
}

/// The arguments and environment of the restart after `restarts` earlier ones: the launch
/// arguments with `--recover` (and without `--no-recover`), the count, and from the second one
/// the CPU compositor. None: no more restarts.
fn gpu_loss_restart(mut args: Vec<std::ffi::OsString>, restarts: u32) -> Option<(Vec<std::ffi::OsString>, Vec<(&'static str, String)>)> {
    if restarts >= 2 {
        return None;
    }
    args.retain(|a| a != "--recover" && a != "--no-recover");
    args.push("--recover".into());
    let mut env = vec![(GPU_LOST_RESTARTS, (restarts + 1).to_string())];
    if restarts >= 1 {
        env.push(("FILMCRAFT_CPU_COMPOSITE", "1".to_string()));
    }
    Some((args, env))
}

/// The GPU export frame renderer (filmcraft-gpu's off-screen compositor behind filmcraft-export's
/// frame-renderer hook): exports with GPU rendering Auto composite on the GPU and fall back to the
/// CPU reference renderer wherever it cannot.
struct GpuFrameRenderer(filmcraft_gpu::ExportRenderer);

impl filmcraft_export::FrameRenderer for GpuFrameRenderer {
    fn render(
        &mut self,
        project: &filmcraft_project::Project,
        seq: filmcraft_project::ItemId,
        t: filmcraft_time::Tick,
        opts: filmcraft_render::RenderOptions,
        sources: &dyn filmcraft_render::SourceProvider,
    ) -> Option<filmcraft_render::Image> {
        self.0.render(project, seq, t, opts, sources)
    }
}

fn register_gpu_frame_renderer() {
    filmcraft_export::register_frame_renderer(|| {
        filmcraft_gpu::ExportRenderer::new().map(|r| Box::new(GpuFrameRenderer(r)) as Box<dyn filmcraft_export::FrameRenderer>)
    });
}

/// Put the OS hardware video decoders in front of our own. Registered in a statement of its own:
/// a log macro does not evaluate its arguments while no logger takes its level, which left the
/// hardware decoders out of every run.
fn register_hardware_decoders() -> filmcraft_platform::Availability {
    let hardware = filmcraft_platform::register();
    log::info!("hardware decoding: {hardware:?}");
    hardware
}

/// Local UTC offset at a unix time (auto-save file names and recovery times use local time).
fn local_offset(unix: i64) -> i32 {
    chrono::Local.timestamp_opt(unix, 0).single().map(|d| d.offset().local_minus_utc()).unwrap_or(0)
}

/// Driven by an agent (`--control`): don't activate the app on launch, so the user's keyboard
/// focus stays where it is (macOS; winit activates ignoring other apps by default).
fn agent_event_loop(agent: bool) -> Option<eframe::EventLoopBuilderHook> {
    #[cfg(target_os = "macos")]
    if agent {
        return Some(Box::new(|b| {
            use winit::platform::macos::EventLoopBuilderExtMacOS;
            b.with_activate_ignoring_other_apps(false);
        }));
    }
    let _ = agent;
    None
}

/// Show `dialog` with rfd on a worker thread and answer on the returned channel (#260). `parent`
/// is an empty rfd dialog that knows the app's window (fork L1.5); `start` the folder it opens in.
/// Filter names are translated here, on the UI thread (the interface language is per thread);
/// extensions get their upper-case forms (`file_filters`). A dialog thread that can't start or
/// panics drops the sender: the app takes that as cancelled.
#[cfg(target_os = "linux")]
fn run_dialog_off_thread(
    parent: rfd::FileDialog,
    dialog: filmcraft_ui_egui::native_dialogs::FileDialog,
    start: Option<String>,
) -> std::sync::mpsc::Receiver<Vec<String>> {
    use filmcraft_ui_egui::native_dialogs::FileDialog;
    let t = |s: &str| filmcraft_ui_egui::i18n::t(s).to_string();
    let exts = |e: &[String]| file_filters::extensions(&e.iter().map(String::as_str).collect::<Vec<_>>());
    let media = t("Media");
    let project = t("Revolution Project");
    let dialog = match dialog {
        FileDialog::OpenFiles { exts: e } => FileDialog::OpenFiles { exts: exts(&e) },
        FileDialog::Relink { exts: e, hint } => FileDialog::Relink { exts: exts(&e), hint },
        FileDialog::OpenFile { filter, exts: e } => FileDialog::OpenFile { filter: t(&filter), exts: exts(&e) },
        FileDialog::SaveAs { filter, exts: e, name } => FileDialog::SaveAs { filter: t(&filter), exts: exts(&e), name },
        other => other,
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("file-dialog".into()).spawn(move || {
        let shown = std::panic::catch_unwind(move || {
            let path = |p: std::path::PathBuf| p.to_string_lossy().into_owned();
            // a remembered folder that's gone (an ejected card) or unreachable: the default place
            let d = match start {
                Some(dir) if !dir.is_empty() && std::path::Path::new(&dir).is_dir() => parent.set_directory(dir),
                _ => parent,
            };
            let project_exts = file_filters::extensions(&["fcproj"]);
            match dialog {
                FileDialog::OpenFiles { exts } => d.add_filter(&media, &exts).pick_files().unwrap_or_default().into_iter().map(path).collect(),
                FileDialog::OpenFile { filter, exts } => d.add_filter(&filter, &exts).pick_file().map(path).into_iter().collect(),
                FileDialog::Relink { exts, .. } => d.add_filter(&media, &exts).pick_file().map(path).into_iter().collect(),
                FileDialog::OpenProject => d.add_filter(&project, &project_exts).pick_file().map(path).into_iter().collect(),
                FileDialog::SaveProject { name } => d.add_filter(&project, &project_exts).set_file_name(name).save_file().map(path).into_iter().collect(),
                FileDialog::SaveAs { filter, exts, name } => d.add_filter(&filter, &exts).set_file_name(name).save_file().map(path).into_iter().collect(),
                FileDialog::Folder { at } => {
                    let d = match at {
                        Some(dir) if !dir.is_empty() => d.set_directory(dir),
                        _ => d,
                    };
                    d.pick_folder().map(path).into_iter().collect::<Vec<_>>()
                }
            }
        });
        match shown {
            Ok(paths) => {
                let _ = tx.send(paths);
            }
            Err(_) => log::warn!("the file dialog thread panicked; taken as cancelled"),
        }
    });
    if let Err(e) = spawned {
        log::warn!("could not start the file dialog thread: {e}");
    }
    rx
}

#[cfg(test)]
mod tests {
    /// A second launch that hands its files over must not open (and so rotate) the running
    /// instance's log file: the single-instance claim comes first in `main` (L1.7 review).
    #[test]
    fn single_instance_claim_comes_before_the_log_file() {
        let src = include_str!("main.rs");
        let main = src.find("fn main()").unwrap_or(0);
        let claim = src[main..].find("single_instance::claim(").expect("main claims the instance");
        let attach = src[main..].find("logger.attach_dir(").expect("main opens the log file");
        assert!(claim < attach, "hand-off launches would rotate the running instance's log");
    }

    /// Fork XPS sign-off: losing the window's GPU restarts with `--recover`, then on the CPU
    /// compositor, then not again (no restart loop).
    #[test]
    fn a_lost_gpu_restarts_recovering_then_on_the_cpu_then_stops() {
        let args: Vec<std::ffi::OsString> = ["--control", "9876", "--no-recover", "clip.mov"].iter().map(Into::into).collect();
        let (a, env) = super::gpu_loss_restart(args.clone(), 0).expect("first restart");
        assert_eq!(a, ["--control", "9876", "clip.mov", "--recover"].map(std::ffi::OsString::from).to_vec());
        assert_eq!(env, vec![(super::GPU_LOST_RESTARTS, "1".to_string())]);
        let (_, env) = super::gpu_loss_restart(args.clone(), 1).expect("second restart");
        assert!(env.contains(&("FILMCRAFT_CPU_COMPOSITE", "1".to_string())), "{env:?}");
        assert!(super::gpu_loss_restart(args, 2).is_none(), "no third restart");
    }

    /// Start-up registers the hardware decoders whether or not anything is logged (no logger is
    /// installed here, as in a normal run).
    #[test]
    fn startup_registers_the_hardware_decoders_without_a_logger() {
        assert!(!log::log_enabled!(log::Level::Info));
        let hardware = super::register_hardware_decoders();
        // always on macOS and Windows; on Linux when a VA-API driver or NVIDIA's driver (NVDEC) is there
        let expected = cfg!(any(target_os = "macos", target_os = "windows"))
            || (cfg!(target_os = "linux") && matches!(hardware, filmcraft_platform::Availability::Available(_)));
        assert_eq!(filmcraft_platform::registered(), expected, "{hardware:?}");
    }
}
