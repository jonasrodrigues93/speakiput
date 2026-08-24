use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use slint::winit_030::{WinitWindowAccessor, winit};
use slint::{CloseRequestResponse, ComponentHandle};
use speakiput::{
    AppTray, RecordingOverlay, SettingsWindow,
    controller::GuiController,
    presentation::{apply_model, credential_secret_from_window, settings_from_window},
    view_model::GuiViewModel,
};
use speakiput_client::{BackendClient, UnixBackendClient};
use speakiput_contract::Settings;
use tokio::sync::mpsc;

enum UiCommand {
    StartOrStop,
    SaveSettings(Box<Settings>, Option<String>),
    ReloadSettings,
    ClearHistory,
    RefreshDiagnostics,
}

type ProjectedSettings = Arc<Mutex<Option<(Option<uuid::Uuid>, u64)>>>;

const APP_ID: &str = "io.github.jonas.speakiput";

fn main() -> Result<(), slint::PlatformError> {
    let start_hidden = std::env::args_os().any(|argument| argument == "--background");
    let window_icon = application_icon();
    speakiput_platform_linux_gui::configure_backend(
        slint::BackendSelector::new(),
        window_icon,
        APP_ID,
    )
    .select()?;
    slint::set_xdg_app_id(APP_ID)?;
    // Slint may realize every top-level window when the event loop starts.
    // Construct the overlay first so the Linux backend can identify its native
    // window before either top-level is created.
    speakiput_platform_linux_gui::prepare_overlay_creation();
    let overlay = RecordingOverlay::new()?;
    let settings = SettingsWindow::new()?;
    let tray = AppTray::new()?;

    settings
        .window()
        .on_close_requested(|| CloseRequestResponse::HideWindow);

    let (commands, command_rx) = mpsc::unbounded_channel();
    let projected_settings: ProjectedSettings = Arc::new(Mutex::new(None));

    let command_tx = commands.clone();
    settings.on_record_requested(move || {
        let _ = command_tx.send(UiCommand::StartOrStop);
    });
    let settings_weak_for_save = settings.as_weak();
    let command_tx = commands.clone();
    settings.on_save_requested(move || {
        if let Some(window) = settings_weak_for_save.upgrade() {
            let _ = command_tx.send(UiCommand::SaveSettings(
                Box::new(settings_from_window(&window)),
                credential_secret_from_window(&window),
            ));
        }
    });
    let command_tx = commands.clone();
    settings.on_reload_settings_requested(move || {
        let _ = command_tx.send(UiCommand::ReloadSettings);
    });
    let command_tx = commands.clone();
    settings.on_history_clear_requested(move || {
        let _ = command_tx.send(UiCommand::ClearHistory);
    });
    let command_tx = commands.clone();
    settings.on_diagnostics_refresh_requested(move || {
        let _ = command_tx.send(UiCommand::RefreshDiagnostics);
    });

    let settings_weak = settings.as_weak();
    tray.on_open_settings(move || {
        if let Some(settings) = settings_weak.upgrade() {
            eprintln!("speakiput: opening Settings from tray");
            present_settings(&settings);
        }
    });
    tray.on_quit_requested(|| {
        let _ = slint::quit_event_loop();
    });
    let command_tx = commands;
    tray.on_record_requested(move || {
        let _ = command_tx.send(UiCommand::StartOrStop);
    });

    spawn_backend_session(
        settings.as_weak(),
        overlay.as_weak(),
        tray.as_weak(),
        command_rx,
        projected_settings,
    );

    tray.show()?;
    if !start_hidden {
        settings.show()?;
    }
    slint::run_event_loop()
}

fn present_settings(settings: &SettingsWindow) {
    settings.window().set_minimized(false);
    let _ = settings.show();
    let _ = settings.window().with_winit_window(|window| {
        window.request_user_attention(Some(winit::window::UserAttentionType::Informational));
        window.focus_window();
    });
    settings.window().request_redraw();
}

fn application_icon() -> Option<winit::window::Icon> {
    const SIDE: usize = 64;
    const SIDE_U32: u32 = 64;
    let mut rgba = vec![0_u8; SIDE * SIDE * 4];
    for y in 6_usize..58 {
        for x in 6_usize..58 {
            let corner = !(14..50).contains(&x) && !(14..50).contains(&y);
            let corner_center_x = if x < 14 { 14 } else { 49 };
            let corner_center_y = if y < 14 { 14 } else { 49 };
            let dx = x.abs_diff(corner_center_x);
            let dy = y.abs_diff(corner_center_y);
            if corner && dx * dx + dy * dy > 64 {
                continue;
            }
            let offset = (y * SIDE + x) * 4;
            rgba[offset..offset + 4].copy_from_slice(&[32, 37, 34, 255]);
        }
    }
    for (x, top, bottom) in [
        (19, 27, 37),
        (24, 22, 42),
        (29, 17, 47),
        (34, 21, 43),
        (39, 25, 39),
        (44, 28, 36),
    ] {
        for y in top..bottom {
            for column in x..x + 3 {
                let offset = (y * SIDE + column) * 4;
                rgba[offset..offset + 4].copy_from_slice(&[243, 246, 242, 255]);
            }
        }
    }
    winit::window::Icon::from_rgba(rgba, SIDE_U32, SIDE_U32).ok()
}

#[allow(clippy::too_many_lines)]
fn spawn_backend_session(
    settings: slint::Weak<SettingsWindow>,
    overlay: slint::Weak<RecordingOverlay>,
    tray: slint::Weak<AppTray>,
    mut commands: mpsc::UnboundedReceiver<UiCommand>,
    projected_settings: ProjectedSettings,
) {
    std::thread::Builder::new()
        .name("speakiput-backend-client".into())
        .spawn(move || {
            let runtime = tokio::runtime::Runtime::new().expect("failed to start backend runtime");
            runtime.block_on(async move {
                loop {
                    let client =
                        match UnixBackendClient::connect(speakiput_client::default_socket_path())
                            .await
                        {
                            Ok(client) => client,
                            Err(error) => {
                                let model = GuiViewModel {
                                    error: Some(format!("Backend unavailable: {error}")),
                                    ..GuiViewModel::default()
                                };
                                schedule_model(
                                    &settings,
                                    &overlay,
                                    &tray,
                                    &projected_settings,
                                    model,
                                );
                                tokio::time::sleep(Duration::from_secs(1)).await;
                                continue;
                            }
                        };
                    let client: Arc<dyn BackendClient> = Arc::new(client);
                    let mut controller = GuiController::new(client);
                    if let Err(error) = controller.bootstrap().await {
                        controller.model.error = Some(error.to_string());
                        schedule_model(
                            &settings,
                            &overlay,
                            &tray,
                            &projected_settings,
                            controller.model.clone(),
                        );
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                    schedule_model(
                        &settings,
                        &overlay,
                        &tray,
                        &projected_settings,
                        controller.model.clone(),
                    );

                    let reconnect = loop {
                        while let Ok(command) = commands.try_recv() {
                            let result = match command {
                                UiCommand::StartOrStop => controller.start_or_stop().await,
                                UiCommand::SaveSettings(value, secret) => {
                                    controller
                                        .save_settings_with_credential(*value, secret)
                                        .await
                                }
                                UiCommand::ReloadSettings => controller.reload_settings().await,
                                UiCommand::ClearHistory => controller.clear_history().await,
                                UiCommand::RefreshDiagnostics => {
                                    controller.refresh_diagnostics().await
                                }
                            };
                            if let Err(error) = result {
                                controller.model.error = Some(error.to_string());
                            }
                            schedule_model(
                                &settings,
                                &overlay,
                                &tray,
                                &projected_settings,
                                controller.model.clone(),
                            );
                        }

                        match tokio::time::timeout(
                            Duration::from_millis(100),
                            controller.next_event(),
                        )
                        .await
                        {
                            Ok(Ok(())) => {
                                schedule_model(
                                    &settings,
                                    &overlay,
                                    &tray,
                                    &projected_settings,
                                    controller.model.clone(),
                                );
                            }
                            Ok(Err(error)) => {
                                controller.model.error = Some(error.to_string());
                                schedule_model(
                                    &settings,
                                    &overlay,
                                    &tray,
                                    &projected_settings,
                                    controller.model.clone(),
                                );
                                break true;
                            }
                            Err(_) => {}
                        }
                        if commands.is_closed() {
                            break false;
                        }
                    };
                    if !reconnect {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(400)).await;
                }
            });
        })
        .expect("failed to start backend client thread");
}

fn schedule_model(
    settings: &slint::Weak<SettingsWindow>,
    overlay: &slint::Weak<RecordingOverlay>,
    tray: &slint::Weak<AppTray>,
    projected_settings: &ProjectedSettings,
    model: GuiViewModel,
) {
    let settings = settings.clone();
    let overlay = overlay.clone();
    let tray = tray.clone();
    let projected_settings = Arc::clone(projected_settings);
    let _ = slint::invoke_from_event_loop(move || {
        if let (Some(settings), Some(overlay), Some(tray)) =
            (settings.upgrade(), overlay.upgrade(), tray.upgrade())
        {
            let settings_key = (model.instance_id, model.settings_revision);
            let apply_settings = projected_settings.lock().map_or(true, |mut projected| {
                if projected.as_ref() == Some(&settings_key) {
                    false
                } else {
                    *projected = Some(settings_key);
                    true
                }
            });
            let _ = apply_model(&settings, &overlay, &tray, &model, apply_settings);
        }
    });
}
