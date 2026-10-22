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

/// Build cached debug rows (rebuild ≤ every 250 ms to avoid per-frame overhead).
/// Uses static cached rows; the matrix is recomputed no more than once per 250 ms window.
/// Each row is a fixed-width cell: padded label + right-aligned value, so when a value's
/// width changes only that cell's interior shifts and nothing layout-shifts.
fn build_debug_rows(status: &str, settings: &crate::settings::Settings) -> Vec<String> {
    static LAST_REBUILD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    static CACHED_ROWS: std::sync::atomic::AtomicPtr<Vec<String>> = std::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let last = LAST_REBUILD.load(std::sync::atomic::Ordering::Relaxed);
    let cached_ptr = CACHED_ROWS.load(std::sync::atomic::Ordering::Relaxed);

    // Rebuild if no cached rows or if 250 ms has elapsed since last rebuild.
    if cached_ptr.is_null() || now_ms.saturating_sub(last) > 250 {
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
        let st = f("stride:");
        let pick = f("pick:");
        let enc = f("enc:");
        let err = f("err:");
        let cs = f("cs:");

        const LABEL_WIDTH: usize = 16;
        const VALUE_WIDTH: usize = 8;

        let res = if enc.is_empty() { "?".to_string() } else { enc };
        let err_val = if err.is_empty() { "0".to_string() } else { err };
        let color_val = if cs.is_empty() { "BT.709".to_string() } else { cs };

        let new_rows: Vec<String> = vec![
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", format!("Dec/Pres:{}", dp), format!("Dec/Pip:{}ms", da)),
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", format!("Repl:{}", repl), format!("Skip:{}", skip)),
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", format!("HWbuf:{}", hwb), format!("Lag:{}ms", lag)),
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", format!("BW:{}kbps", bw), format!("LagMx:{}ms", lagmax)),
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", format!("Resync:{}", rs), format!("Flush:{}", fl)),
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", format!("TWCC:{}", tw_s), format!("Strd:{}", st)),
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", format!("Pick:{}us", pick), ""),
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", "Decoder:avcdec", format!("Res:{}", res)),
            format!("{:<LABEL_WIDTH$}{:>VALUE_WIDTH$}", format!("Color:{}", color_val), format!("Err:{}", err_val)),
        ];

        // Free any previously cached rows and store the new ones.
        let _ = CACHED_ROWS.store(std::ptr::null_mut(), std::sync::atomic::Ordering::Relaxed);
        let _ = LAST_REBUILD.store(now_ms, std::sync::atomic::Ordering::Relaxed);
        let _boxed = Box::into_raw(Box::new(new_rows.clone()));
        CACHED_ROWS.store(_boxed, std::sync::atomic::Ordering::Relaxed);

        new_rows
    } else {
        // Return a clone of the cached rows.
        if !cached_ptr.is_null() {
            unsafe { (*cached_ptr).clone() }
        } else {
            build_debug_rows(status, settings)
        }
    }
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

        // Full debug overlay (fixed-width columns, semi-transparent bg, non-zero hidden)
        if app.settings.show_stream_debug_info && !streaming.status.is_empty() {
            let rows = build_debug_rows(&streaming.status, &app.settings);

            if !rows.is_empty() {
                const ROW_HEIGHT: f32 = 13.0_f32;
                const LABEL_WIDTH: usize = 16;
                const VALUE_WIDTH: usize = 8;
                let n = rows.len();

                // Two-column layout when many rows (>10), otherwise single column
                let two_columns = n > 10;
                let n_left = if two_columns { n / 2 } else { n };
                let n_right = if two_columns { n - n_left } else { 0 };

                let cell_px_width = (LABEL_WIDTH + VALUE_WIDTH) as f32 * 6.0; // approx 6px per char at 10pt monospace
                const COLUMN_GAP: usize = 32;      // 5+ extra space cells at 6px/char ≈ 96-192px
                const CELL_RIGHT_MARGIN: usize = 8;  // 8 spare cells of right margin per cell
                let panel_w = if two_columns {
                    cell_px_width * 2.0 + COLUMN_GAP as f32 + 24.0 + CELL_RIGHT_MARGIN as f32 * 6.0
                } else {
                    cell_px_width + 16.0 + CELL_RIGHT_MARGIN as f32 * 6.0
                };
                let panel_h = ROW_HEIGHT * n as f32 + 16.0;

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

                        // Draw rows from build_debug_rows (fixed-width cells)
                        for (i, cell_text) in rows.iter().enumerate() {
                            let y = 8.0 + i as f32 * ROW_HEIGHT;

                            if two_columns && i >= n_left {
                                // Right column: split cell text into label and value
                                // Cell format: "LABEL:       VALUE" (label left-padded to LABEL_WIDTH, value right-aligned)
                                let label_part = &cell_text[..std::cmp::min(LABEL_WIDTH, cell_text.len())];
                                let value_part = if cell_text.len() > LABEL_WIDTH {
                                    &cell_text[LABEL_WIDTH..]
                                } else {
                                    ""
                                };
                                // Draw label in right column, right-aligned within cell width
                                let x = cell_px_width + COLUMN_GAP as f32 + 32.0;
                                painter.text(
                                    egui::pos2(x, y), egui::Align2::RIGHT_TOP,
                                    label_part,
                                    FontId::monospace(10.0),
                                    theme.text.gamma_multiply(0.85),
                                );
                                // Draw value right-aligned within cell width
                                let value_x = x - cell_px_width + 32.0 - CELL_RIGHT_MARGIN as f32 * 6.0;
                                painter.text(
                                    egui::pos2(value_x + cell_px_width, y), egui::Align2::RIGHT_TOP,
                                    value_part,
                                    FontId::monospace(10.0),
                                    theme.text.gamma_multiply(0.85),
                                );
                                continue;
                            }

                            // Left column (or single column)
                            let label_part = &cell_text[..std::cmp::min(LABEL_WIDTH, cell_text.len())];
                            let value_part = if cell_text.len() > LABEL_WIDTH {
                                &cell_text[LABEL_WIDTH..]
                            } else {
                                ""
                            };

                            let x = 8.0;
                            // Draw label left-aligned at start of cell
                            painter.text(
                                egui::pos2(x, y), egui::Align2::LEFT_TOP,
                                label_part,
                                FontId::monospace(10.0),
                                theme.text.gamma_multiply(0.85),
                            );
                            // Draw value right-aligned within cell width
                            let value_x = x + LABEL_WIDTH as f32 * 6.0 + 48.0;
                            painter.text(
                                egui::pos2(value_x, y), egui::Align2::RIGHT_TOP,
                                value_part,
                                FontId::monospace(10.0),
                                theme.text.gamma_multiply(0.85),
                            );
                        }

                        // Additional row: current input→present latency (single column only, below all debug rows)
                        if !two_columns {
                            let lat_age = crate::latency_test::last_press_age_ms();
                            let lat_label = if lat_age.is_some() {
                                format!("lat:{} ms", lat_age.unwrap())
                            } else {
                                "lat:--- ms".to_owned()
                            };
                            let y = 8.0 + n as f32 * ROW_HEIGHT;
                            painter.text(
                                egui::pos2(8.0, y), egui::Align2::LEFT_TOP,
                                "lat:",
                                FontId::monospace(10.0),
                                theme.text.gamma_multiply(0.85),
                            );
                            let val_x = 8.0 + LABEL_WIDTH as f32 * 6.0;
                            painter.text(
                                egui::pos2(val_x, y), egui::Align2::RIGHT_TOP,
                                &lat_label,
                                FontId::monospace(10.0),
                                theme.text.gamma_multiply(0.85),
                            );
                        }

                        // In two-column mode, draw the latency row at the bottom of the overlay
                        if two_columns {
                            let lat_age = crate::latency_test::last_press_age_ms();
                            let lat_label = if lat_age.is_some() {
                                format!("lat:{} ms", lat_age.unwrap())
                            } else {
                                "lat:--- ms".to_owned()
                            };
                            let y = 8.0 + n as f32 * ROW_HEIGHT;
                            painter.text(
                                egui::pos2(8.0, y), egui::Align2::LEFT_TOP,
                                "lat:",
                                FontId::monospace(10.0),
                                theme.text.gamma_multiply(0.85),
                            );
                            let val_x = 8.0 + LABEL_WIDTH as f32 * 6.0;
                            painter.text(
                                egui::pos2(val_x, y), egui::Align2::RIGHT_TOP,
                                &lat_label,
                                FontId::monospace(10.0),
                                theme.text.gamma_multiply(0.85),
                            );
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

