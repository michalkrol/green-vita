use crate::App;
use crate::app::ui::theme::Theme;
use crate::app::ui::widgets::draw_hold_progress_ring;
use crate::i18n::I18n;
use crate::api_xbox::streaming::control::input::{PROBE_COUNTER, PROBE_PRESS_US, SESSION_START};
use std::time::Instant;

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

        // Latency probe overlay - shows a white square and timer when the
        // probe button (Menu) is pressed, allowing end-to-end latency measurement
        // via camera flash + stopwatch. The overlay is composited onto presented
        // video frames, so if the video stalls the counter cannot update and would
        // under-read. That is acceptable (and itself a signal).
        if app.settings.latency_probe && !streaming.status.is_empty() {
            let press_us = PROBE_PRESS_US.load(std::sync::atomic::Ordering::Relaxed);
            let counter = PROBE_COUNTER.load(std::sync::atomic::Ordering::Relaxed);
            let mut display_text = String::new();
            let mut square_color = egui::Color32::from_rgb(64, 64, 64);  // dark by default
            let mut frozen = false;

            if press_us > 0 {
                let now_us = Instant::now().duration_since(*SESSION_START.get().expect("SESSION_START should be initialized")).as_micros() as u64;
                let elapsed_us = now_us.saturating_sub(press_us);
                let elapsed_ms = elapsed_us.saturating_div(1000);

                // Square turns white for first ~150ms after press
                if elapsed_us < 150_000 {
                    square_color = egui::Color32::WHITE;
                }

                // Freeze display at >= 2000ms; show frozen value
                if elapsed_ms >= 2000 {
                    display_text = format!("{} ms (frozen)", 2000);
                    frozen = true;
                } else {
                    display_text = format!("{} ms", elapsed_ms);
                }
            } else {
                display_text = "--- ms".to_owned();
            }

            // Small line showing the probe counter and the button being used
            let counter_text = if counter > 0 {
                format!("Probe #{}  Menu button", counter)
            } else {
                "Menu button".to_owned()
            };

            egui::Area::new(egui::Id::new("latency_probe_overlay"))
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::TOP_LEFT, egui::vec2(20.0, 20.0))
                .show(ctx, |ui| {
                    // Large filled square (approx 120x120 logical px)
                    // Bright white for first ~150ms after probe press, dark otherwise
                    let rect_size = egui::vec2(120.0, 120.0);
                    let rect_pos = egui::pos2(20.0, 20.0);
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(rect_pos, rect_size),
                        8.0,
                        square_color,
                    );

                    // Large text readout (48 pt) of microseconds-since-press formatted as milliseconds
                    let text_pos = egui::pos2(30.0, 50.0);
                    ui.painter().text(
                        text_pos,
                        egui::Align2::CENTER_TOP,
                        egui::RichText::new(&display_text)
                            .size(48.0),
                        egui::Color32::WHITE,
                    );

                    // Small line showing the probe counter and the button being used
                    let counter_pos = egui::pos2(30.0, 100.0);
                    ui.painter().text(
                        counter_pos,
                        egui::Align2::CENTER_TOP,
                        egui::RichText::new(&counter_text)
                            .size(16.0),
                        egui::Color32::LIGHT_GRAY,
                    );
                });
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
                        // Inside an empty Area `ui.available_width()` is ~0, which used to
                        // collapse the backdrop rectangle to zero width (invisible). Claim
                        // the full panel size and paint through a painter whose clip rect is
                        // the panel itself so the opaque backdrop is always drawn.
                        let panel_rect = egui::Rect::from_min_size(
                            egui::pos2(0.0, 0.0),
                            egui::vec2(panel_w, panel_h),
                        );
                        ui.set_min_size(panel_rect.size());
                        let painter = ui.painter_at(panel_rect.expand(4.0));

                        painter.rect_filled(panel_rect, 4.0, egui::Color32::from_black_alpha(235));

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