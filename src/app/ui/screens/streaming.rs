use crate::App;
use crate::app::ui::theme::Theme;
use crate::app::ui::widgets::draw_hold_progress_ring;
use crate::i18n::I18n;
use egui::{Area, Frame, RichText, Align2};

/// Fullscreen video view with a one-time "Hold Back to pause" hint, fading out over `HINT_FADE`.
pub(crate) fn show(ctx: &egui::Context, app: &App, hold_progress: Option<f32>) {
    const HINT_VISIBLE: std::time::Duration = std::time::Duration::from_secs(2);
    const HINT_FADE: std::time::Duration = std::time::Duration::from_secs(1);

    let theme = Theme::dark();
    let i18n = I18n::new(app.settings.locale);
    let streaming = match &app.state {
        crate::AppState::Streaming(streaming) => streaming,
        _ => return,
    };
    let mut frame = egui::Frame::central_panel(&ctx.style());
    frame.fill = egui::Color32::TRANSPARENT;
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        let elapsed = streaming.hint_started_at.elapsed();
        let alpha = if elapsed < HINT_VISIBLE {
            1.0
        } else if elapsed < HINT_VISIBLE + HINT_FADE {
            1.0 - (elapsed - HINT_VISIBLE).as_secs_f32() / HINT_FADE.as_secs_f32()
        } else {
            0.0
        };

        if alpha > 0.0 {
            ui.vertical_centered(|ui| {
                ui.add_space(16.0);
                ui.colored_label(
                    theme.text.gamma_multiply(alpha),
                    i18n.text("streaming-hold-back"),
                );
            });
        }

        if app.settings.show_stream_debug_info {
            // Split the two-line status text at the newline separator.
            // Status format from media.rs: "enc:XXX srcfps:YY line1\nline2 wait:Z drop:W err:X"
            let (line1, line2) = streaming.status.split_once('\n').unwrap_or((&streaming.status, ""));

            // Improved HUD: monospace text with translucent dark background, bottom-left anchored.
            Area::new(egui::Id::new("stream_debug_hud"))
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::BOTTOM_LEFT, egui::vec2(12.0, 12.0))
                .fixed_size(egui::Vec2::new(280.0, 36.0))
                .frame(Frame::none().fill(egui::Color32::from_black_alpha(200)))
                .show(ctx, |ui| {
                    ui.label(RichText::new(line1).monospace().size(10.0));
                    ui.add_space(2.0);
                    ui.label(RichText::new(line2).monospace().size(10.0));
                });
        }
    });

    if let Some(progress) = hold_progress {
        egui::Area::new(egui::Id::new("pause_hold_indicator"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::LEFT_TOP, egui::vec2(16.0, 16.0))
            .show(ctx, |ui| {
                draw_hold_progress_ring(ui, progress);
            });
    }
}
