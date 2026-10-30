//! The tray icon: status, pause, settings and quit.

use ghostcaret_runtime::pause::{PauseFor, Scope};
use ghostcaret_runtime::{Command, StatusKind, StatusView};
use tauri::menu::{IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};

use crate::AppState;

const TRAY: &str = "main";
const LENGTHS: [(&str, &str); 3] = [
    ("15", "For 15 minutes"),
    ("60", "For 1 hour"),
    ("0", "Until resumed"),
];

pub fn create(
    app: &AppHandle,
    status: &StatusView,
    settings_error: Option<&str>,
) -> tauri::Result<()> {
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    TrayIconBuilder::with_id(TRAY)
        .icon(icon)
        .tooltip(tooltip(status))
        .menu(&menu(app, status, settings_error)?)
        .show_menu_on_left_click(true)
        .on_menu_event(on_menu)
        .build(app)?;
    Ok(())
}

/// Rebuilds the menu for a new status. Runs on the main thread.
pub fn update(app: &AppHandle, status: &StatusView, settings_error: Option<&str>) {
    let Some(tray) = app.tray_by_id(TRAY) else {
        return;
    };
    let result = menu(app, status, settings_error)
        .and_then(|m| tray.set_menu(Some(m)))
        .and_then(|()| tray.set_tooltip(Some(tooltip(status))));
    if let Err(e) = result {
        log::warn!("tray update failed: {e}");
    }
}

fn tooltip(status: &StatusView) -> String {
    format!("Ghostcaret: {}", status.text)
}

fn menu(
    app: &AppHandle,
    status: &StatusView,
    settings_error: Option<&str>,
) -> tauri::Result<Menu<Wry>> {
    let mut items: Vec<Box<dyn IsMenuItem<Wry>>> = Vec::new();
    items.push(Box::new(MenuItem::with_id(
        app,
        "status",
        &status.text,
        false,
        None::<&str>,
    )?));
    if let Some(e) = settings_error {
        items.push(Box::new(MenuItem::with_id(
            app,
            "settings",
            format!("Settings problem: {e}"),
            true,
            None::<&str>,
        )?));
    }
    items.push(Box::new(PredefinedMenuItem::separator(app)?));

    let paused_all = status.pauses.iter().any(|p| p.app.is_none());
    if paused_all {
        items.push(Box::new(MenuItem::with_id(
            app,
            "resume:all",
            "Resume",
            true,
            None::<&str>,
        )?));
    } else {
        items.push(Box::new(pause_menu(app, "all", "Pause")?));
    }
    if let Some(current) = &status.app
        && !paused_all
    {
        let paused_here = status
            .pauses
            .iter()
            .any(|p| p.app.as_deref() == Some(&current.id));
        if paused_here {
            items.push(Box::new(MenuItem::with_id(
                app,
                "resume:app",
                format!("Resume in {}", current.name),
                true,
                None::<&str>,
            )?));
        } else if status.kind != StatusKind::OffInApp {
            items.push(Box::new(pause_menu(
                app,
                "app",
                &format!("Pause in {}", current.name),
            )?));
        }
    }
    items.push(Box::new(PredefinedMenuItem::separator(app)?));
    items.push(Box::new(MenuItem::with_id(
        app,
        "settings",
        "Settings…",
        true,
        None::<&str>,
    )?));
    items.push(Box::new(MenuItem::with_id(
        app,
        "quit",
        "Quit Ghostcaret",
        true,
        None::<&str>,
    )?));
    let refs: Vec<&dyn IsMenuItem<Wry>> = items.iter().map(|i| i.as_ref()).collect();
    Menu::with_items(app, &refs)
}

fn pause_menu(app: &AppHandle, scope: &str, label: &str) -> tauri::Result<Submenu<Wry>> {
    let items = LENGTHS
        .iter()
        .map(|(minutes, text)| {
            MenuItem::with_id(
                app,
                format!("pause:{scope}:{minutes}"),
                *text,
                true,
                None::<&str>,
            )
        })
        .collect::<tauri::Result<Vec<_>>>()?;
    let refs: Vec<&dyn IsMenuItem<Wry>> = items.iter().map(|i| i as &dyn IsMenuItem<Wry>).collect();
    Submenu::with_id_and_items(app, format!("pause:{scope}"), label, true, &refs)
}

fn on_menu(app: &AppHandle, event: MenuEvent) {
    let id = event.id().as_ref();
    let Some(state) = app.try_state::<std::sync::Arc<AppState>>() else {
        return;
    };
    match id {
        "settings" => crate::open_main(app),
        "quit" => app.exit(0),
        "resume:all" => state.runtime.send(Command::Resume(None)),
        "resume:app" => {
            let current = state.runtime.status().app;
            if let Some(current) = current {
                state.runtime.send(Command::Resume(Some(current.id)));
            }
        }
        _ => {
            let Some(rest) = id.strip_prefix("pause:") else {
                return;
            };
            let Some((scope, minutes)) = rest.split_once(':') else {
                return;
            };
            let Ok(minutes) = minutes.parse::<u32>() else {
                return;
            };
            let length = if minutes == 0 {
                PauseFor::UntilResumed
            } else {
                PauseFor::Minutes(minutes)
            };
            let scope = match scope {
                "all" => Scope::All,
                _ => {
                    let current = state.runtime.status().app;
                    let Some(current) = current else { return };
                    Scope::App {
                        id: current.id,
                        name: current.name,
                    }
                }
            };
            state.runtime.send(Command::Pause(scope, length));
        }
    }
}
