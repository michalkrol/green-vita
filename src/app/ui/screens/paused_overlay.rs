use crate::app::ui::header::show_header_row;
use crate::app::ui::theme::Theme;
use crate::i18n::I18n;
use crate::{App, AppCommand, AppState, InputCommand};
use anyhow::Result;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Resume,
    /// Send the Xbox guide (nexus pulse) to the console.
    PressGuideButton,
    /// One-button reconnect for a stalled stream. Index 2 — the stall
    /// detector opens this overlay focused here.
    ReconnectStream,
    Settings,
    /// Tear the stream down and return to the in-app list; the app keeps running.
    ExitGame,
    /// Tear the stream down (same as `ExitGame`), then exit the process to LiveArea.
    QuitApp,
}

pub const MENU_ITEMS: [Command; 6] = [
    Command::Resume,
    Command::PressGuideButton,
    Command::ReconnectStream,
    Command::Settings,
    Command::ExitGame,
    Command::QuitApp,
];

/// Row the stall detector focuses when it auto-opens this overlay (Reconnect).
pub(crate) const STALL_FOCUS_INDEX: usize = 2;

impl Command {
    fn icon(self) -> &'static str {
        match self {
            Self::Resume => "\u{25b6}",
            Self::PressGuideButton => "\u{2302}",
            Self::ReconnectStream => "\u{27f3}",
            Self::Settings => "\u{2699}",
            Self::ExitGame => "\u{2715}",
            Self::QuitApp => "\u{2716}",
        }
    }

    /// Row label, read fresh each frame (i18n lookup).
    fn label(self, i18n: &I18n) -> String {
        match self {
            Self::Resume => i18n.text("paused-resume"),
            Self::PressGuideButton => i18n.text("paused-xbox-guide"),
            Self::ReconnectStream => i18n.text("paused-reconnect"),
            Self::Settings => i18n.text("menu-settings"),
            Self::ExitGame => i18n.text("paused-exit-stream"),
            Self::QuitApp => i18n.text("paused-quit-app"),
        }
    }
}

pub(crate) fn show(ctx: &egui::Context, app: &App, commands: &mut Vec<AppCommand>) {
    let theme = Theme::dark();
    let i18n = I18n::new(app.settings.locale);
    let mut frame = egui::Frame::central_panel(&ctx.style());
    frame.fill = egui::Color32::from_rgba_unmultiplied(0x2a, 0x2a, 0x2e, 190);
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        show_header_row(ui, app, theme, &i18n, Some(commands));
        ui.separator();
        ui.vertical_centered(|ui| {
            ui.add_space(8.0);
            match app.active_title_name() {
                Some(title_name) => {
                    ui.heading(egui::RichText::new(title_name).color(theme.text_bright));
                    ui.colored_label(theme.accent, i18n.text("screen-paused"));
                }
                None => {
                    ui.heading(egui::RichText::new(i18n.text("screen-paused")).color(theme.accent));
                }
            }
            // Stall banner: the connection went silent (console idle/screensaver);
            // ReconnectStream below is the recommended recovery.
            let stalled = matches!(&app.state, AppState::Streaming(streaming) if streaming.stalled);
            if stalled {
                ui.add_space(8.0);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(i18n.text("stalled-banner"))
                            .color(egui::Color32::from_rgb(0xff, 0xb4, 0x5c)),
                    );
                });
                ui.add_space(8.0);
            }
            ui.add_space(12.0);
            ui.set_max_width(240.0);

            // Six rows fit the Vita screen height; the D-pad moves
            // `pause_selected` through them (see `handle_paused_overlay_input`).
            let selected_index = match &app.state {
                AppState::Streaming(streaming) => streaming.pause_selected,
                _ => 0,
            };
            for (index, item) in MENU_ITEMS.iter().copied().enumerate() {
                if index > 0 {
                    ui.add_space(6.0);
                }
                let response = overlay_row(
                    ui,
                    theme,
                    item.icon(),
                    &item.label(&i18n),
                    index == selected_index,
                );
                if response.clicked() {
                    commands.push(item.into());
                }
            }

            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(footer_hint(&i18n, app))
                    .size(13.0)
                    .color(egui::Color32::from_rgb(0x9e, 0x9e, 0xa6)),
            );
        });
    });
}

/// One menu row, painted to match `widgets::menu_item` (which is shared with
/// the hamburger menu), returning the response so clicks and the D-pad
/// selection highlight work.
fn overlay_row(
    ui: &mut egui::Ui,
    theme: Theme,
    icon: &str,
    label: &str,
    selected: bool,
) -> egui::Response {
    const HEIGHT: f32 = 44.0;
    const CORNER_RADIUS: f32 = 8.0;
    const ICON_COLUMN_WIDTH: f32 = 40.0;

    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, HEIGHT), egui::Sense::click());

    if ui.is_rect_visible(rect) {
        let (fill, text_color) = if selected {
            (Some(theme.accent), egui::Color32::WHITE)
        } else if response.hovered() {
            (
                Some(egui::Color32::from_rgb(0x2c, 0x2d, 0x33)),
                theme.text_bright,
            )
        } else {
            (None, theme.text_bright)
        };
        if let Some(fill) = fill {
            ui.painter().rect_filled(rect, CORNER_RADIUS, fill);
        }

        let font_id = egui::FontId::proportional(16.0);
        ui.painter().text(
            egui::pos2(rect.min.x + ICON_COLUMN_WIDTH / 2.0, rect.center().y),
            egui::Align2::CENTER_CENTER,
            icon,
            font_id.clone(),
            text_color,
        );
        ui.painter().text(
            egui::pos2(rect.min.x + ICON_COLUMN_WIDTH, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            font_id,
            text_color,
        );
    }

    response
}

/// One dim hint line under the menu: the gestures that open this menu and
/// reach the host guide, depending on the in-stream control settings.
fn footer_hint(i18n: &I18n, app: &App) -> String {
    let mut hint = i18n.text("paused-hint-view");
    if app.settings.ps_button_capture {
        hint.push_str(" · ");
        hint.push_str(&i18n.text("paused-hint-ps"));
    }
    hint
}

impl App {
    pub(crate) async fn handle_paused_overlay_input(
        &mut self,
        command: InputCommand,
    ) -> Result<()> {
        let Some(selected) = (match &self.state {
            AppState::Streaming(streaming) => Some(streaming.pause_selected),
            _ => None,
        }) else {
            return Ok(());
        };
        match command {
            InputCommand::MoveUp => {
                if let AppState::Streaming(streaming) = &mut self.state {
                    streaming.pause_selected =
                        crate::app::command::move_prev(selected, MENU_ITEMS.len());
                }
            }
            InputCommand::MoveDown => {
                if let AppState::Streaming(streaming) = &mut self.state {
                    streaming.pause_selected =
                        crate::app::command::move_next(selected, MENU_ITEMS.len());
                }
            }
            InputCommand::MoveLeft | InputCommand::MoveRight => {}
            InputCommand::Confirm => {
                let command = MENU_ITEMS
                    .get(selected)
                    .copied()
                    .unwrap_or(Command::ExitGame);
                self.handle_paused_overlay_command(command).await?;
            }
            // Only the Resume row / pause toggle resumes; Back is sent through to the game while
            // streaming and does not close this overlay.
            InputCommand::Back => {}
        }

        Ok(())
    }

    pub(crate) async fn handle_paused_overlay_command(&mut self, command: Command) -> Result<()> {
        match command {
            Command::Resume => {
                if let Some(streaming) = self.state.streaming_mut() {
                    streaming.set_paused(false);
                }
                self.menu.open = false;
            }
            Command::ReconnectStream => {
                self.reconnect_stream().await;
            }
            Command::Settings => {
                self.open_settings();
            }
            Command::PressGuideButton => {
                if let Some(streaming) = self.state.streaming_mut() {
                    streaming.set_paused(false);
                    streaming.press_guide_button();
                }
                self.menu.open = false;
            }
            Command::ExitGame => {
                self.exit_stream().await;
            }
            Command::QuitApp => {
                // Same teardown as ExitGame (metrics flush + backend stop)
                // when a stream is live.
                if matches!(self.state, AppState::Streaming(_)) {
                    self.exit_stream().await;
                }
                // ORDER IS CRITICAL (HOME-BUTTON-BRAINSTORM.md §6 row 1):
                // release the PS-button shell lock BEFORE killing the process —
                // on Enso 3.60/3.65, exiting while locked can leave the PS
                // button unresponsive in LiveArea until a screen sleep/wake.
                crate::ps_button::force_unlock();
                // Intentional immediate exit to LiveArea (§8a) — no
                // confirmation dialog, matches the design doc.
                unsafe {
                    vitasdk_sys::sceKernelExitProcess(0);
                }
            }
        }

        Ok(())
    }
}
