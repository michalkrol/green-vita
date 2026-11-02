//! Cap-advisor banner (Phase 2 of network validation, TODO 7b): shows the
//! probe-derived cap suggestion on the pre-stream list screens. Strictly
//! ADVISORY — nothing auto-applies; [Apply cap] writes the suggestion into
//! settings (persisted via the existing settings save path).

use crate::app::command::ScreenCommand;
use crate::app::ui::theme::Theme;
use crate::i18n::{I18n, arg_string};
use crate::{App, AppCommand, AppState};
use fluent_bundle::FluentArgs;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Command {
    ApplyCap(u32),
    Dismiss,
}

/// Renders the suggestion strip on the pre-stream list screens. Call BEFORE
/// the screens' CentralPanel (egui panel ordering).
pub(crate) fn show_banner(ctx: &egui::Context, app: &App, commands: &mut Vec<AppCommand>) {
    let Some(advice) = &app.cap_advice else { return };
    if app.cap_advice_dismissed || advice.suggested_kbps == 0 {
        return;
    }
    if !matches!(
        app.state,
        AppState::TitleList { .. } | AppState::ConsoleList { .. }
    ) {
        return;
    }
    let theme = Theme::dark();
    let i18n = I18n::new(app.settings.locale);
    egui::TopBottomPanel::top("cap-advice-banner").show(ctx, |ui| {
        ui.horizontal_wrapped(|ui| {
            let mut args = FluentArgs::new();
            args.set(
                "mbps",
                arg_string(advice.downlink_mbps.map_or_else(
                    || "n/a".to_string(),
                    |v| format!("{v:.1}"),
                )),
            );
            args.set(
                "rtt",
                arg_string(advice.rtt_ms.map_or_else(
                    || "n/a".to_string(),
                    |v| format!("{v:.0}"),
                )),
            );
            args.set("kbps", arg_string(advice.suggested_kbps.to_string()));
            ui.label(
                egui::RichText::new(i18n.text_with("net-cap-advice", args)).color(theme.text),
            );
            if ui.button(i18n.text("net-cap-apply")).clicked() {
                commands.push(
                    AppCommand::Screen(ScreenCommand::CapAdvice(Command::ApplyCap(
                        advice.suggested_kbps,
                    ))),
                );
            }
            if ui.button(i18n.text("net-cap-dismiss")).clicked() {
                commands
                    .push(AppCommand::Screen(ScreenCommand::CapAdvice(Command::Dismiss)));
            }
        });
    });
}
