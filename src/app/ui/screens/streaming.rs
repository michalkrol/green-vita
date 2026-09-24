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

        // Full debug overlay (left side, 2 columns, semi-transparent bg, non-zero hidden)
        if app.settings.show_stream_debug_info && !streaming.status.is_empty() {
            let rows = build_debug_rows(&streaming.status, &app.settings);

            if !rows.is_empty() {
                let row_h = 13.0_f32;
                let col_w = 185.0_f32;
                let n = rows.len();
                let panel_h = row_h * n as f32 + 16.0_f32;
                let panel_w = col_w * 2.0_f32 + 24.0_f32;

                egui::Area::new(egui::Id::new("debug_overlay"))
                    .order(egui::Order::Foreground)
                    .anchor(egui::Align2::LEFT_TOP, egui::vec2(8.0, 8.0))
                    .show(ctx, |ui| {
                        let painter = ui.painter();
                        let rect = egui::Rect::from_min_size(
                            egui::pos2(0.0, 0.0),
                            egui::vec2(panel_w.min(ui.available_width()), panel_h),
                        );
                        painter.rect_filled(rect, 4.0, egui::Color32::from_black_alpha(200));

                        for (i, (left, right)) in rows.iter().enumerate() {
                            let y = 8.0 + i as f32 * row_h;
                            painter.text(
                                egui::pos2(8.0, y), egui::Align2::LEFT_TOP,
                                left,
                                egui::FontId::monospace(10.0),
                                theme.text.gamma_multiply(0.85),
                            );
                            if let Some(r) = right {
                                painter.text(
                                    egui::pos2(8.0 + col_w, y), egui::Align2::LEFT_TOP,
                                    r,
                                    egui::FontId::monospace(10.0),
                                    theme.text.gamma_multiply(0.85),
                                );
                            }
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

/// Build curated debug rows (2 columns, fixed positions, permanently exclude always-zero fields).
fn build_debug_rows(status: &str, settings: &crate::settings::Settings) -> Vec<(String, Option<String>)> {
    let f = |key: &str| extract_field(status, key);
    let dp = f("d/p:");
    let da = f("d/a:");
    let repl = f("repl:");
    let skip = f("skip:");
    let hwb = f("hwb:");
    let lag = f("lag:");
    let bw = f("bw:");
    let lagmax = f("lagMax:");
    let fl = f("fl:");
    let rs = f("rs:");
    let tw_s = f("tw:");
    let st = f("st:");
    let pick = f("pick:");

    use crate::settings::H264Profile;
    let profile_label = match settings.video_h264_profile {
        H264Profile::Baseline => "Base", H264Profile::Main => "Main",
    };

    vec![
        (format!("Dec/Pres:{}", dp),   Some(format!("Dec/Pip:{}ms", da))),
        (format!("Repl:{}", repl),      Some(format!("Skip:{}", skip))),
        (format!("HWbuf:{}", hwb),      Some(format!("Lag:{}ms", lag))),
        (format!("BW:{}kbps", bw),      Some(format!("LagMx:{}ms", lagmax))),
        (format!("Resync:{}", rs),      Some(format!("Flush:{}", fl))),
        (format!("TWCC:{}", tw_s),      Some(format!("Strd:{}", st))),
        (format!("Pick:{}us", pick),    Some(format!("Prof:{}", profile_label))),
    ]
}