use crate::App;
use crate::app::ui::theme::Theme;
use crate::app::ui::widgets::draw_hold_progress_ring;
use crate::i18n::I18n;
use egui::FontId;

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

    vec![
        (format!("Dec/Pres:{}", dp), Some(format!("Dec/Pip:{}ms", da))),
        (format!("Repl:{}", repl), Some(format!("Skip:{}", skip))),
        (format!("HWbuf:{}", hwb), Some(format!("Lag:{}ms", lag))),
        (format!("BW:{}kbps", bw), Some(format!("LagMx:{}ms", lagmax))),
        (format!("Resync:{}", rs), Some(format!("Flush:{}", fl))),
        (format!("TWCC:{}", tw_s), Some(format!("Strd:{}", st))),
        (format!("Pick:{}us", pick), Some("".to_owned())),
    ]
}

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

        // Latency test mode: blinking square + stopwatch on button press.
        // The square flips colour on every button-press edge and the readout shows
        // ms from that edge to this frame (client-side input -> present delay).
        if app.settings.latency_test_mode {
            let blink = crate::latency_test::BLINK.load(std::sync::atomic::Ordering::Relaxed);
            let age_ms = crate::latency_test::last_press_age_ms();

            egui::Area::new(egui::Id::new("latency_test_marker"))
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::LEFT_TOP, egui::vec2(8.0, 8.0))
                .show(ctx, |ui| {
                    let painter = ui.painter();
                    let square = egui::Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(160.0, 160.0),
                    );
                    if blink {
                        painter.rect_filled(square, 0.0, egui::Color32::WHITE);
                        painter.rect_stroke(
                            square,
                            0.0,
                            egui::Stroke::new(2.0, egui::Color32::BLACK),
                            egui::StrokeKind::Inside,
                        );
                    } else {
                        painter.rect_filled(square, 0.0, egui::Color32::BLACK);
                        painter.rect_stroke(
                            square,
                            0.0,
                            egui::Stroke::new(2.0, egui::Color32::WHITE),
                            egui::StrokeKind::Inside,
                        );
                    }

                    let readout = match age_ms {
                        Some(ms) => format!("+{:.0} ms", ms),
                        None => "+--- ms".to_owned(),
                    };
                    let readout_rect = egui::Rect::from_min_size(
                        egui::pos2(168.0, 0.0),
                        egui::vec2(120.0, 40.0),
                    );
                    painter.rect_filled(readout_rect, 0.0, egui::Color32::from_black_alpha(200));
                    painter.text(
                        readout_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        readout,
                        FontId::monospace(16.0),
                        egui::Color32::from_rgb(144, 238, 144),
                    );

                    let _ = ui.allocate_space(egui::vec2(296.0, 160.0));
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
                        let painter = ui.painter();
                        let rect = egui::Rect::from_min_size(
                            egui::pos2(0.0, 0.0),
                            egui::vec2(panel_w.min(ui.available_width()), panel_h),
                        );
                        painter.rect_filled(rect, 4.0, egui::Color32::from_black_alpha(200));

                        // Draw rows from build_debug_rows
                        for (i, (left, right)) in rows.iter().enumerate() {
                            let y = 8.0 + i as f32 * row_h;
                            painter.text(
                                egui::pos2(8.0, y), egui::Align2::LEFT_TOP,
                                left,
                                FontId::monospace(10.0),
                                theme.text.gamma_multiply(0.85),
                            );
                            if let Some(r) = right {
                                painter.text(
                                    egui::pos2(8.0 + col_w, y), egui::Align2::LEFT_TOP,
                                    r,
                                    FontId::monospace(10.0),
                                    theme.text.gamma_multiply(0.85),
                                );
                            }
                        }

                        // Additional row: current input→present latency
                        let lat_age = crate::latency_test::last_press_age_ms();
                        let lat_label = if lat_age.is_some() {
                            format!("lat:{} ms", lat_age.unwrap())
                        } else {
                            "lat:--- ms".to_owned()
                        };
                        let y = 8.0 + rows.len() as f32 * row_h;
                        painter.text(
                            egui::pos2(8.0, y), egui::Align2::LEFT_TOP,
                            "lat:",
                            FontId::monospace(10.0),
                            theme.text.gamma_multiply(0.85),
                        );
                        painter.text(
                            egui::pos2(8.0 + 30.0, y), egui::Align2::LEFT_TOP,
                            &lat_label,
                            FontId::monospace(10.0),
                            theme.text.gamma_multiply(0.85),
                        );
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
