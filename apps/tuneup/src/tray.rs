use std::sync::mpsc::Sender;

use eframe::egui;
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem},
};

#[derive(Debug, Clone, Copy)]
pub enum TrayAction {
    OpenMainWindow,
    ExitApplication,
}

pub struct TrayController {
    _icon: TrayIcon,
}

impl TrayController {
    pub fn new(action_tx: Sender<TrayAction>, context: egui::Context) -> Result<Self, String> {
        let menu = Menu::new();
        let open_item = MenuItem::with_id("open", "Открыть", true, None);
        let exit_item = MenuItem::with_id("exit", "Выйти", true, None);
        menu.append(&open_item).map_err(|error| error.to_string())?;
        menu.append(&exit_item).map_err(|error| error.to_string())?;

        let open_id = open_item.id().clone();
        let exit_id = exit_item.id().clone();
        let menu_tx = action_tx.clone();
        let menu_context = context.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let action = if event.id == open_id {
                Some(TrayAction::OpenMainWindow)
            } else if event.id == exit_id {
                Some(TrayAction::ExitApplication)
            } else {
                None
            };
            if let Some(action) = action {
                let _ = menu_tx.send(action);
                menu_context.request_repaint();
            }
        }));

        let tray_context = context;
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            let should_open = matches!(
                event,
                TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                } | TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            );
            if should_open {
                let _ = action_tx.send(TrayAction::OpenMainWindow);
                tray_context.request_repaint();
            }
        }));

        let icon = TrayIconBuilder::new()
            .with_tooltip("TuneUp — монитор процессов")
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_icon_as_template(cfg!(target_os = "macos"))
            .with_icon(create_icon()?)
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self { _icon: icon })
    }
}

fn create_icon() -> Result<Icon, String> {
    const SIDE: u32 = 32;
    let mut rgba = Vec::with_capacity((SIDE * SIDE * 4) as usize);
    for y in 0..SIDE {
        for x in 0..SIDE {
            let dx = x as i32 - 15;
            let dy = y as i32 - 15;
            let distance_squared = dx * dx + dy * dy;
            let visible = (81..=196).contains(&distance_squared)
                || ((13..=18).contains(&x) && (8..=23).contains(&y));
            rgba.extend_from_slice(&[49, 130, 246, if visible { 255 } else { 0 }]);
        }
    }
    Icon::from_rgba(rgba, SIDE, SIDE).map_err(|error| error.to_string())
}
