use crate::App;
use crate::app::ui::theme::Theme;
use crate::app::ui::widgets::draw_hold_progress_ring;
use crate::i18n::I18n;

/// Fullscreen video view with debug overlays.
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

        // Top-right FPS + bandwidth overlay (light green, no background)
        if app.settings.show_fps_overlay && !streaming.status.is_empty() {
            let bw  = extract_field(&streaming.status, "bw:");
            let dp_raw = extract_field(&streaming.status, "d/p:");
            let presented = dp_raw.split('/').nth(1).unwrap_or("").to_owned();
            let mut parts = Vec::new();
            if !bw.is_empty() { parts.push(format!("{}kbps", bw)); }
            if !presented.is_empty() { parts.push(format!("FPS:{}", presented)); }
            let bits = parts.join("  ");

            if !bits.is_empty() {
egui::Area::new(egui::Id::new("fps_overlay"))
                    .order(egui::Order::Foreground)
                    .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-12.0, 12.0))
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(bits)
                                    .color(egui::Color32::from_rgb(144, 238, 144))
                                    .size(14.0),
                            );
                        });
                    });
            }
        }

        // Full debug overlay (left side, one field per line, semi-transparent bg)
        if app.settings.show_stream_debug_info && !streaming.status.is_empty() {
            let mut lines = split_status_lines(&streaming.status);
            use crate::settings::H264Profile;
            let profile_label = match app.settings.video_h264_profile {
                H264Profile::Baseline => "Base",
                H264Profile::Main => "Main",
            };
            lines.push(format!("profile:{}", profile_label));
            if !lines.is_empty() {
                let line_height = 13.0;
                let panel_height = line_height * lines.len() as f32 + 16.0;
                let panel_width = lines.iter().map(|l| l.len()).max().unwrap_or(0) as f32 * 5.5 + 16.0;

                egui::Area::new(egui::Id::new("debug_overlay"))
                    .order(egui::Order::Foreground)
                    .anchor(egui::Align2::LEFT_TOP, egui::vec2(8.0, 8.0))
                    .show(ctx, |ui| {
                        // Semi-transparent background
                        let painter = ui.painter();
                        let rect = egui::Rect::from_min_size(
                            egui::pos2(0.0, 0.0),
                            egui::vec2(panel_width.min(ui.available_width()), panel_height),
                        );
                        painter.rect_filled(rect, 4.0, egui::Color32::from_black_alpha(140));

                        let mut y = 8.0;
                        for line in &lines {
                            painter.text(
                                egui::pos2(8.0, y),
                                egui::Align2::LEFT_TOP,
                                line,
                                egui::FontId::monospace(11.0),
                                theme.text.gamma_multiply(0.85),
                            );
                            y += line_height;
                        }
                    });
            }
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

/// Extract a value after `key` in the status string (e.g. `srcfps:60` → "60").
fn extract_field(status: &str, key: &str) -> String {
    if let Some(pos) = status.find(key) {
        let start = pos + key.len();
        let mut val = String::new();
        for ch in status[start..].chars() {
            if ch.is_ascii_alphanumeric() || ch == '/' || ch == '.' || ch == '%' {
                val.push(ch);
            } else {
                break;
            }
        }
        val
    } else {
        String::new()
    }
}

/// Split the multi-line status string into individual key:value lines for the debug panel.
fn split_status_lines(status: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for segment in status.split_whitespace() {
        if segment.contains(':') || segment.contains('/') {
            lines.push(segment.to_owned());
        } else if let Some(last) = lines.last_mut() {
            // Attach orphan tokens (like "tw:392/300/1" is one token, fine)
            last.push(' ');
            last.push_str(segment);
        }
    }
    lines
}