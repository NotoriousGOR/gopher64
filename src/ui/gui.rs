use crate::device;
use crate::retroachievements;
use crate::ui;
#[cfg(target_os = "android")]
use crate::ui::android;
use slint::Model;
#[cfg(not(target_os = "android"))]
use slint::winit_030::WinitWindowAccessor;

slint::include_modules!();

#[cfg(not(target_os = "android"))]
pub const N64_EXTENSIONS: [&str; 12] = [
    "n64", "v64", "z64", "7z", "zip", "bin", "N64", "V64", "Z64", "7Z", "ZIP", "BIN",
];

#[derive(serde::Deserialize)]
struct GithubData {
    tag_name: String,
}

pub struct NetplayDevice {
    pub server_addr: String,
    pub player_number: usize,
    pub number_of_players: usize,
    pub input_delay: usize,
}

fn check_latest_version(weak: slint::Weak<AppWindow>) {
    let task = ui::WEB_CLIENT
        .get("https://api.github.com/repos/gopher64/gopher64/releases/latest")
        .send();
    tokio::spawn(async move {
        let response = task.await;
        if let Ok(response) = response {
            let data: Result<GithubData, reqwest::Error> = response.json().await;

            let latest_version = match data {
                Ok(data) => match semver::Version::parse(&data.tag_name[1..]) {
                    Ok(github_version) => github_version,
                    Err(e) => {
                        eprintln!("Error parsing latest version from GitHub: {}", e);
                        semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap()
                    }
                },
                Err(e) => {
                    eprintln!("Error getting latest version from GitHub: {}", e);
                    semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap()
                }
            };
            let current_version = semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
            if current_version < latest_version {
                weak.upgrade_in_event_loop(move |handle| handle.set_has_update(true))
                    .unwrap();
            }
        }
    });
}

pub fn open_uri(path: impl AsRef<std::ffi::OsStr>) {
    #[cfg(target_os = "android")]
    return ui::android::open_uri(path.as_ref().to_str().unwrap());

    #[cfg(not(target_os = "android"))]
    if let Err(e) = open::that_detached(path) {
        eprintln!("Error opening path: {}", e);
    }
}

fn run_with_path(weak: slint::Weak<AppWindow>, path: std::path::PathBuf) {
    let weak2 = weak.clone();
    weak.upgrade_in_event_loop(move |handle| {
        if handle.get_game_running() {
            return;
        }
        save_settings(&handle);

        run_rom(
            path,
            ui::GameSettings {
                overclock: handle.get_overclock_n64_cpu(),
                disable_expansion_pak: handle.get_disable_expansion_pak(),
                cheats: rustc_hash::FxHashMap::default(), // will be filled in later
                load_savestate_slot: None,
            },
            None,
            show_touch_overlay(&handle),
            weak2,
        );
    })
    .unwrap();
}

#[cfg(not(target_os = "android"))]
fn file_dropped(app: &AppWindow) {
    let weak = app.as_weak();
    app.window()
        .on_winit_window_event(move |_winit_window, event| {
            if let slint::winit_030::winit::event::WindowEvent::DroppedFile(path) = event {
                run_with_path(weak.clone(), path.to_path_buf());
            }
            slint::winit_030::EventResult::Propagate
        });
}

fn rom_exists(path: &str) -> bool {
    #[cfg(not(target_os = "android"))]
    return std::fs::exists(path).unwrap_or(false);
    #[cfg(target_os = "android")]
    return android::rom_exists(path);
}

/// How many recently played cartridges to remember. Kept in one place because the GUI and
/// the CLI both trim the list.
pub const RECENT_LIMIT: usize = 20;

/// Build the display row for a cartridge path: the file name without its extension, and
/// the containing folder underneath so two copies of the same ROM stay distinguishable.
fn cartridge_entry(path: &str) -> Cartridge {
    let decoded = decode_path(path);
    let decoded_path = std::path::Path::new(&decoded);
    let name = decoded_path
        .file_stem()
        .or_else(|| decoded_path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| decoded.clone());
    let detail = decoded_path
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_default();

    Cartridge {
        path: path.into(),
        name: name.into(),
        detail: detail.into(),
    }
}

fn recent_model(paths: impl IntoIterator<Item = String>) -> slint::ModelRc<Cartridge> {
    slint::ModelRc::from(std::rc::Rc::new(slint::VecModel::from(
        paths
            .into_iter()
            .filter(|path| rom_exists(path))
            .take(RECENT_LIMIT)
            .map(|path| cartridge_entry(&path))
            .collect::<Vec<Cartridge>>(),
    )))
}

fn store_recent_roms(app: &AppWindow) {
    let mut config = ui::config::Config::new();
    config.ui.recent_roms = app
        .get_recent_roms()
        .iter()
        .map(|cartridge| cartridge.path.to_string())
        .collect();
}

fn local_game_window(app: &AppWindow, config: &ui::config::Config) {
    app.set_recent_roms(recent_model(config.ui.recent_roms.iter().cloned()));

    let weak = app.as_weak();
    app.on_open_rom_button_clicked(move || {
        weak.upgrade_in_event_loop(move |handle| {
            save_settings(&handle);
            open_rom(&handle)
        })
        .unwrap();
    });

    let weak = app.as_weak();
    app.on_recent_rom_button_clicked(move |rom| {
        weak.upgrade_in_event_loop(move |handle| {
            run_with_path(handle.as_weak(), std::path::PathBuf::from(rom.to_string()));
        })
        .unwrap();
    });

    let weak = app.as_weak();
    app.on_recent_rom_remove_clicked(move |rom| {
        weak.upgrade_in_event_loop(move |handle| {
            let remaining: Vec<String> = handle
                .get_recent_roms()
                .iter()
                .map(|cartridge| cartridge.path.to_string())
                .filter(|path| path.as_str() != rom.as_str())
                .collect();
            handle.set_recent_roms(recent_model(remaining));
            store_recent_roms(&handle);
        })
        .unwrap();
    });

    let weak = app.as_weak();
    app.on_recent_roms_clear_clicked(move || {
        weak.upgrade_in_event_loop(move |handle| {
            handle.set_recent_roms(recent_model(std::iter::empty()));
            store_recent_roms(&handle);
        })
        .unwrap();
    });

    #[cfg(not(target_os = "android"))]
    {
        let saves_path = ui::get_dirs().data_dir.join("saves");
        app.on_saves_folder_button_clicked(move || {
            open_uri(&saves_path);
        });

        file_dropped(app);
    }
}

fn input_profiles(config: &ui::config::Config) -> Vec<String> {
    let mut profiles = vec![];
    for key in config.input.input_profiles.keys() {
        profiles.push(key.clone())
    }

    // make sure default profile is always first
    if let Some(pos) = profiles.iter().position(|x| x == "default") {
        let default_profile = profiles.remove(pos);
        profiles.insert(0, default_profile);
    }
    profiles
}

fn settings_window(app: &AppWindow, config: &ui::config::Config) {
    app.set_integer_scaling(config.video.integer_scaling);
    app.set_ssaa(config.video.ssaa);
    app.set_fullscreen(config.video.fullscreen);
    app.set_widescreen(config.video.widescreen);
    app.set_vsync(config.video.vsync);
    app.set_apply_crt_shader(config.video.crt);
    app.set_theme(config.ui.theme);
    app.set_overclock_n64_cpu(config.emulation.overclock);
    app.set_disable_expansion_pak(config.emulation.disable_expansion_pak);
    app.set_emulate_usb(config.emulation.usb);
    app.set_rewind(config.emulation.rewind);
    let combobox_value = match config.video.upscale {
        1 => 0,
        2 => 1,
        4 => 2,
        8 => 3,
        _ => 0,
    };
    app.set_resolution(combobox_value);

    if let Some(rom_dir_str) = config.ui.rom_dir.to_str() {
        app.set_rom_dir(rom_dir_str.into());
    }
}

pub fn update_input_profiles(weak: &slint::Weak<AppWindow>, config: &ui::config::Config) {
    let profiles = input_profiles(config);
    let config_bindings = config.input.input_profile_binding.clone();
    weak.upgrade_in_event_loop(move |handle| {
        let profile_bindings = slint::VecModel::default();
        for (i, input_profile_binding) in handle.get_selected_profile_binding().iter().enumerate() {
            let currently_selected = handle
                .get_input_profiles()
                .row_data(input_profile_binding as usize)
                .unwrap_or(config_bindings[i].clone().into())
                .to_string();
            let position = profiles
                .iter()
                .position(|profile| *profile == currently_selected);
            profile_bindings.push(position.unwrap_or(0) as i32);
        }

        handle.set_input_profiles(slint::ModelRc::from(std::rc::Rc::new(
            slint::VecModel::from(
                profiles
                    .iter()
                    .map(|x| x.into())
                    .collect::<Vec<slint::SharedString>>(),
            ),
        )));

        handle
            .set_selected_profile_binding(slint::ModelRc::from(std::rc::Rc::new(profile_bindings)));
    })
    .unwrap();
}

fn clear_gb_paths(weak: &slint::Weak<AppWindow>, player: i32) {
    weak.upgrade_in_event_loop(move |handle| {
        let rom_paths = handle.get_gb_rom_paths();
        let ram_paths = handle.get_gb_ram_paths();
        rom_paths.set_row_data(player as usize, String::new().into());
        ram_paths.set_row_data(player as usize, String::new().into());
        handle.set_gb_rom_paths(rom_paths);
        handle.set_gb_ram_paths(ram_paths);
    })
    .unwrap();
}

fn controller_window(app: &AppWindow, config: &ui::config::Config) {
    #[cfg(not(target_os = "android"))]
    ui::sdl_init(sdl3_sys::init::SDL_INIT_GAMEPAD);

    app.set_emulate_vru(config.input.emulate_vru);

    app.set_controller_enabled(slint::ModelRc::from(std::rc::Rc::new(
        slint::VecModel::from(config.input.controller_enabled.to_vec()),
    )));

    app.set_transferpak(slint::ModelRc::from(std::rc::Rc::new(
        slint::VecModel::from(config.input.transfer_pak.to_vec()),
    )));

    app.set_gb_rom_paths(slint::ModelRc::from(std::rc::Rc::new(
        slint::VecModel::from(
            config
                .input
                .gb_rom_path
                .iter()
                .map(|x| x.into())
                .collect::<Vec<slint::SharedString>>(),
        ),
    )));

    app.set_gb_ram_paths(slint::ModelRc::from(std::rc::Rc::new(
        slint::VecModel::from(
            config
                .input
                .gb_ram_path
                .iter()
                .map(|x| x.into())
                .collect::<Vec<slint::SharedString>>(),
        ),
    )));

    update_input_profiles(&app.as_weak(), config);

    app.set_controller_changed(slint::ModelRc::from(std::rc::Rc::new(
        slint::VecModel::from(vec![false, false, false, false]),
    )));

    let config_controller_assignment = config.input.controller_assignment.clone();
    let weak_app = app.as_weak();
    app.on_controller_window_created(move || {
        let controller_assignment = config_controller_assignment.clone();
        weak_app
            .upgrade_in_event_loop(move |handle| {
                let mut current_selected_paths = vec![None; 4];
                for (i, selected_controller) in handle.get_selected_controller().iter().enumerate()
                {
                    current_selected_paths[i] = handle
                        .get_controller_paths()
                        .row_data(selected_controller as usize);
                }

                let controller_names = ui::input::get_controller_names();
                handle.set_controller_names(slint::ModelRc::from(std::rc::Rc::new(
                    slint::VecModel::from(
                        controller_names
                            .iter()
                            .map(|x| x.into())
                            .collect::<Vec<slint::SharedString>>(),
                    ),
                )));

                let controller_paths = ui::input::get_controller_paths();
                handle.set_controller_paths(slint::ModelRc::from(std::rc::Rc::new(
                    slint::VecModel::from(
                        controller_paths
                            .iter()
                            .map(|x| x.into())
                            .collect::<Vec<slint::SharedString>>(),
                    ),
                )));

                let selected_controllers = slint::VecModel::default();
                for i in 0..4 {
                    let assigned_path =
                        if let Some(current_selected_path) = &current_selected_paths[i] {
                            current_selected_path.to_string()
                        } else if let Some(config_assigned_path) = &controller_assignment[i] {
                            config_assigned_path.to_string()
                        } else {
                            String::new()
                        };
                    let selected_index = controller_paths
                        .iter()
                        .position(|controller_path| assigned_path == *controller_path)
                        .unwrap_or(0) as i32;
                    selected_controllers.push(selected_index);
                }
                handle.set_selected_controller(slint::ModelRc::from(std::rc::Rc::new(
                    selected_controllers,
                )));
            })
            .unwrap();
    });

    let weak_app = app.as_weak();
    app.on_input_profile_button_clicked(move || {
        weak_app
            .upgrade_in_event_loop(move |handle| {
                handle.set_input_deadzone(ui::input_profile::DEADZONE_DEFAULT);
                handle.set_input_range(ui::input_profile::RANGE_DEFAULT);
                handle.set_input_profile_name(String::new().into());
                handle.set_input_dinput(false);
                handle.set_show_input_profile(true);
            })
            .unwrap();
    });
    let weak_app = app.as_weak();
    app.on_input_profile_creation_button_clicked(move || {
        #[cfg(not(target_os = "android"))]
        let weak_app2 = weak_app.clone();
        weak_app
            .upgrade_in_event_loop(move |handle| {
                let profile_name = handle.get_input_profile_name();
                let dinput = handle.get_input_dinput();
                let deadzone = handle.get_input_deadzone();
                let range = handle.get_input_range();
                handle.set_show_input_profile(false);

                #[cfg(target_os = "android")]
                ui::android::spawn_configure_input_profile(profile_name, dinput, deadzone, range);

                #[cfg(not(target_os = "android"))]
                tokio::spawn(async move {
                    let cli_path = std::env::current_exe()
                        .unwrap()
                        .parent()
                        .unwrap()
                        .join(format!("{}-cli", env!("CARGO_PKG_NAME")));
                    let cmd_path = if cfg!(target_os = "macos") && cli_path.exists() {
                        cli_path
                    } else {
                        std::env::current_exe().unwrap()
                    };
                    let mut command = tokio::process::Command::new(cmd_path);
                    command.args([
                        "--configure-input-profile",
                        &profile_name,
                        "--deadzone",
                        &deadzone.to_string(),
                        "--range",
                        &range.to_string(),
                    ]);
                    if dinput {
                        command.arg("--use-dinput");
                    }
                    if !command.status().await.unwrap().success() {
                        eprintln!("Failed to configure input profile");
                    }
                    let config = ui::config::Config::new();
                    update_input_profiles(&weak_app2, &config);
                });
            })
            .unwrap();
    });

    let weak_app2 = app.as_weak();
    app.on_transferpak_toggled(move |player, enabled| {
        if enabled {
            let select_gb_rom = select_gb_rom(player);

            let weak_app3 = weak_app2.clone();
            tokio::spawn(async move {
                if let Some(gb_rom) = select_gb_rom.await {
                    let weak_app4 = weak_app3.clone();
                    weak_app3
                        .upgrade_in_event_loop(move |_handle| {
                            let select_gb_ram = select_gb_ram(player);

                            tokio::spawn(async move {
                                if let Some(gb_ram) = select_gb_ram.await {
                                    weak_app4
                                        .upgrade_in_event_loop(move |handle| {
                                            let rom_paths = handle.get_gb_rom_paths();
                                            let ram_paths = handle.get_gb_ram_paths();
                                            rom_paths.set_row_data(
                                                player as usize,
                                                gb_rom.to_str().unwrap().into(),
                                            );
                                            ram_paths.set_row_data(
                                                player as usize,
                                                gb_ram.to_str().unwrap().into(),
                                            );
                                            handle.set_gb_rom_paths(rom_paths);
                                            handle.set_gb_ram_paths(ram_paths);
                                        })
                                        .unwrap();
                                } else {
                                    clear_gb_paths(&weak_app4, player);
                                }
                            });
                        })
                        .unwrap();
                } else {
                    clear_gb_paths(&weak_app3, player);
                }
            });
        }
    });
}

pub fn save_settings(app: &AppWindow) {
    let mut config = ui::config::Config::new();
    config.ui.rom_dir = app.get_rom_dir().to_string().into();
    config.video.integer_scaling = app.get_integer_scaling();
    config.video.ssaa = app.get_ssaa();
    config.video.fullscreen = app.get_fullscreen();
    config.video.widescreen = app.get_widescreen();
    config.video.vsync = app.get_vsync();
    config.video.crt = app.get_apply_crt_shader();
    config.ui.theme = app.get_theme();
    config.emulation.overclock = app.get_overclock_n64_cpu();
    config.emulation.disable_expansion_pak = app.get_disable_expansion_pak();
    config.emulation.usb = app.get_emulate_usb();
    config.emulation.rewind = app.get_rewind();
    let upscale_values = [1, 2, 4, 8];
    config.video.upscale = upscale_values[app.get_resolution() as usize];

    config.input.emulate_vru = app.get_emulate_vru();
    for (i, controller_enabled) in app.get_controller_enabled().iter().enumerate() {
        config.input.controller_enabled[i] = controller_enabled;
    }
    for (i, transferpak_enabled) in app.get_transferpak().iter().enumerate() {
        config.input.transfer_pak[i] = transferpak_enabled;
        config.input.gb_rom_path[i] = app.get_gb_rom_paths().row_data(i).unwrap().to_string();
        config.input.gb_ram_path[i] = app.get_gb_ram_paths().row_data(i).unwrap().to_string();
    }
    for (i, input_profile_binding) in app.get_selected_profile_binding().iter().enumerate() {
        config.input.input_profile_binding[i] = app
            .get_input_profiles()
            .row_data(input_profile_binding as usize)
            .unwrap()
            .to_string();
    }

    for (i, selected_controller) in app.get_selected_controller().iter().enumerate() {
        if app.get_controller_changed().row_data(i).unwrap_or(false) {
            let controller_path = app
                .get_controller_paths()
                .row_data(selected_controller as usize)
                .unwrap()
                .to_string();
            if controller_path.is_empty() {
                config.input.controller_assignment[i] = None;
            } else {
                config.input.controller_assignment[i] = Some(controller_path);
            }
        }
    }
}

fn about_window(app: &AppWindow) {
    app.on_wiki_button_clicked(move || {
        open_uri("https://github.com/gopher64/gopher64/wiki");
    });
    app.on_discord_button_clicked(move || {
        open_uri("https://discord.gg/9RGXq8W8JQ");
    });
    app.on_patreon_button_clicked(move || {
        open_uri("https://patreon.com/loganmc10");
    });
    app.on_github_sponsors_button_clicked(move || {
        open_uri("https://github.com/sponsors/loganmc10");
    });
    app.on_source_code_button_clicked(move || {
        open_uri("https://github.com/gopher64/gopher64");
    });
    app.on_newversion_button_clicked(move || {
        open_uri("https://github.com/gopher64/gopher64/releases/latest");
    });
    app.set_version(format!("Version: {}", env!("GIT_DESCRIBE")).into());

    //flatpak, itch.io, and android have their own update checking mechanism
    if std::env::var("FLATPAK_ID").is_err()
        && std::env::var("ITCHIO_APP").is_err()
        && cfg!(not(target_os = "android"))
    {
        check_latest_version(app.as_weak());
    }
}

pub fn app_window(
    app: &AppWindow,
    is_android: bool,
    no_intro_map: std::sync::Arc<tokio::sync::Mutex<rustc_hash::FxHashMap<String, String>>>,
) {
    let no_intro_map_clone = no_intro_map.clone();
    tokio::spawn(async move {
        load_no_intro(no_intro_map_clone).await;
    });

    retroachievements::init_client(false, false, false);
    app.set_is_android(is_android);
    about_window(app);
    ui::retroachievements::ra_window(app);
    {
        let config = ui::config::Config::new();
        settings_window(app, &config);
        controller_window(app, &config);
        local_game_window(app, &config);
    }
    ui::netplay::netplay_window(app, no_intro_map.clone());
    ui::cheats::cheats_window(app, no_intro_map);

    #[cfg(not(target_os = "android"))]
    {
        let weak_app = app.as_weak();
        app.window().on_close_requested(move || {
            weak_app
                .upgrade_in_event_loop(move |handle| {
                    save_settings(&handle);
                    handle.invoke_netplay_close();
                })
                .unwrap();
            slint::CloseRequestResponse::HideWindow
        });
    }

    #[cfg(target_os = "android")]
    if let Some(path) = ui::android::get_launch_rom() {
        run_with_path(app.as_weak(), path);
    }

    // Launch ROMs opened via Finder or `open -a` (macOS "open documents" Apple Event)
    #[cfg(target_os = "macos")]
    {
        let weak = app.as_weak();
        ui::macos::register_open_documents_handler(move |path| run_with_path(weak.clone(), path));
    }

    app.run().unwrap();
    retroachievements::shutdown_client();
}

pub fn run_rom(
    file_path: std::path::PathBuf,
    game_settings: ui::GameSettings,
    netplay: Option<NetplayDevice>,
    _show_touch_overlay: bool,
    weak: slint::Weak<AppWindow>,
) {
    #[cfg(target_os = "android")]
    ui::android::run_rom(file_path, game_settings, netplay, _show_touch_overlay, weak);

    #[cfg(not(target_os = "android"))]
    tokio::spawn(async move {
        weak.upgrade_in_event_loop(move |handle| handle.set_game_running(true))
            .unwrap();

        let cli_path = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("{}-cli", env!("CARGO_PKG_NAME")));
        let cmd_path = if cfg!(target_os = "macos") && cli_path.exists() {
            cli_path
        } else {
            std::env::current_exe().unwrap()
        };
        let mut command = tokio::process::Command::new(cmd_path);
        command.args([
            "--overclock",
            &game_settings.overclock.to_string(),
            "--disable-expansion-pak",
            &game_settings.disable_expansion_pak.to_string(),
        ]);
        let cheats_path = ui::get_dirs().cache_dir.join("cheats.json");
        if let Some(netplay_device) = netplay {
            let f = std::fs::File::create(&cheats_path).unwrap();
            serde_json::to_writer_pretty(f, &game_settings.cheats).unwrap();

            command.args([
                "--netplay-server-addr",
                &netplay_device.server_addr,
                "--netplay-player-number",
                &netplay_device.player_number.to_string(),
                "--netplay-number-of-players",
                &netplay_device.number_of_players.to_string(),
                "--netplay-input-delay",
                &netplay_device.input_delay.to_string(),
                "--cheats",
                cheats_path.to_str().unwrap(),
            ]);
        }

        let success = command
            .arg(file_path.to_str().unwrap())
            .status()
            .await
            .unwrap()
            .success();

        if !success {
            eprintln!("Failed to run game");
        }

        let _ = std::fs::remove_file(cheats_path);

        weak.upgrade_in_event_loop(move |handle| {
            if let Some(rom_dir) = file_path.parent().unwrap().to_str() {
                handle.set_rom_dir(rom_dir.into());
            }
            if success {
                update_recent_roms(&handle, file_path);
            }
            handle.set_game_running(false);
        })
        .unwrap();
    });
}

fn decode_path(path: &str) -> String {
    #[cfg(target_os = "android")]
    return ui::android::decode_path(path);
    #[cfg(not(target_os = "android"))]
    return path.to_string();
}

pub fn update_recent_roms(app: &AppWindow, file_path: std::path::PathBuf) {
    let launched = file_path.to_str().unwrap().to_string();
    let mut ordered = vec![launched.clone()];
    ordered.extend(
        app.get_recent_roms()
            .iter()
            .map(|cartridge| cartridge.path.to_string())
            .filter(|path| *path != launched),
    );
    app.set_recent_roms(recent_model(ordered));
}

pub async fn get_nointro_name(
    rom: &[u8],
    no_intro_map: std::sync::Arc<tokio::sync::Mutex<rustc_hash::FxHashMap<String, String>>>,
) -> String {
    let hash = device::cart::rom::calculate_hash(rom).to_lowercase();
    if let Some(name) = no_intro_map.lock().await.get(&hash) {
        name.clone()
    } else {
        ui::storage::get_game_name(rom)
    }
}

async fn load_no_intro(
    no_intro_map: std::sync::Arc<tokio::sync::Mutex<rustc_hash::FxHashMap<String, String>>>,
) {
    let mut reader = quick_xml::Reader::from_str(include_str!(
        "../../data/ui/Nintendo - Nintendo 64 (DB Export) (20260609-194259).xml"
    ));
    let mut current_game = String::new();
    let mut map = no_intro_map.lock().await;
    let mut xml_version = quick_xml::XmlVersion::Implicit1_0;
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Decl(e)) => {
                if let Ok(version) = e.xml_version() {
                    xml_version = version;
                }
            }
            Ok(quick_xml::events::Event::Start(e)) => {
                if e.name().as_ref() == "game"
                    && let Ok(Some(name_attribute)) = e.try_get_attribute("name")
                    && let Ok(normalized_value) = name_attribute.normalized_value(xml_version)
                {
                    current_game = normalized_value.into_owned();
                }
            }
            Ok(quick_xml::events::Event::End(e)) => {
                if e.name().as_ref() == "game" {
                    current_game.clear();
                }
            }
            Ok(quick_xml::events::Event::Empty(e)) => {
                if e.name().as_ref() == "file"
                    && let Ok(Some(format_attribute)) = e.try_get_attribute("format")
                    && let Ok(normalized_value) = format_attribute.normalized_value(xml_version)
                    && normalized_value.as_ref() == "BigEndian"
                    && let Ok(Some(sha256_attribute)) = e.try_get_attribute("sha256")
                    && let Ok(normalized_sha256) = sha256_attribute.normalized_value(xml_version)
                    && !current_game.is_empty()
                {
                    map.insert(normalized_sha256.to_lowercase(), current_game.clone());
                }
            }
            Err(e) => panic!("Error at position {}: {:?}", reader.error_position(), e),
            Ok(quick_xml::events::Event::Eof) => break,
            _ => (),
        }
    }
}

pub async fn select_rom(rom_dir: slint::SharedString) -> Option<std::path::PathBuf> {
    #[cfg(target_os = "android")]
    return ui::android::select_rom(rom_dir).await;

    #[cfg(not(target_os = "android"))]
    {
        if !rom_dir.is_empty() && std::fs::exists(&rom_dir).unwrap_or(false) {
            rfd::AsyncFileDialog::new().set_directory(rom_dir)
        } else {
            rfd::AsyncFileDialog::new()
        }
        .set_title("Select ROM")
        .add_filter("ROM files", &N64_EXTENSIONS)
        .pick_file()
        .await
        .map(|file| file.path().to_path_buf())
    }
}

pub async fn select_gb_rom(player: i32) -> Option<std::path::PathBuf> {
    #[cfg(target_os = "android")]
    return ui::android::select_gb_rom(player).await;

    #[cfg(not(target_os = "android"))]
    {
        rfd::AsyncFileDialog::new()
            .set_title(format!("GB ROM P{}", player + 1))
            .add_filter("GB ROM files", &["gb", "gbc", "GB", "GBC"])
            .pick_file()
            .await
            .map(|file| file.path().to_path_buf())
    }
}

pub async fn select_gb_ram(player: i32) -> Option<std::path::PathBuf> {
    #[cfg(target_os = "android")]
    return ui::android::select_gb_ram(player).await;

    #[cfg(not(target_os = "android"))]
    {
        rfd::AsyncFileDialog::new()
            .set_title(format!("GB RAM P{}", player + 1))
            .add_filter("GB RAM files", &["sav", "ram", "srm", "SAV", "RAM", "SRM"])
            .pick_file()
            .await
            .map(|file| file.path().to_path_buf())
    }
}

fn open_rom(app: &AppWindow) {
    let select_rom = select_rom(app.get_rom_dir());

    let overclock = app.get_overclock_n64_cpu();
    let disable_expansion_pak = app.get_disable_expansion_pak();
    let show_touch_overlay = show_touch_overlay(app);

    let weak = app.as_weak();
    tokio::spawn(async move {
        if let Some(file) = select_rom.await {
            run_rom(
                file,
                ui::GameSettings {
                    overclock,
                    disable_expansion_pak,
                    cheats: rustc_hash::FxHashMap::default(), // will be filled in later
                    load_savestate_slot: None,
                },
                None,
                show_touch_overlay,
                weak,
            );
        }
    });
}

pub fn show_touch_overlay(app: &AppWindow) -> bool {
    let selected = app.get_selected_controller().row_data(0).unwrap_or(0);
    app.get_controller_paths()
        .row_data(selected as usize)
        .map(|p| p.is_empty())
        .unwrap_or(true)
}
