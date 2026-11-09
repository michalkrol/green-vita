use crate::app::stream_session::session::{self, StreamReturnTarget};
use crate::app::ui::screens::paused_overlay;
use crate::app::{App, AppState};
use anyhow::Result;

impl App {
    pub(in crate::app) async fn pump_rtc_session(&mut self) -> Result<()> {
        let Some(streaming) = self.state.streaming_mut() else {
            return Ok(());
        };

        let (closed, error) = streaming.drain_backend_events();
        if let Some(error) = error {
            self.stop_stream_with_error("error-webrtc-session", error.to_string())
                .await;
            return Ok(());
        }
        if closed {
            // Console-side close (TODO 16a): an expected event, but the user
            // must see WHY the stream ended — a silent bounce back to the
            // list reads as a crash. Same teardown as an error, friendlier
            // message; raw detail stays in `details`.
            self.stop_stream_with_error(
                "error-stream-ended-by-console",
                "connection closed by the console".to_owned(),
            )
            .await;
            return Ok(());
        }

        let Some(streaming) = self.state.streaming_mut() else {
            return Ok(());
        };
        // Stall detection (idle/screensaver goal): absolute frame silence.
        match streaming.poll_stall() {
            session::StallEvent::Stalled => {
                eprintln!(
                    "stream stall: no decoded frames for {} s — console idle/screensaver? sending guide wake pulse",
                    session::STREAM_STALL_THRESHOLD.as_secs()
                );
                // Preservation attempt: one guide pulse dismisses the console
                // screensaver if the input path is still alive. If media
                // resumes, poll_stall reports Recovered and the overlay we
                // open below closes itself.
                streaming.press_guide_button();
                if !streaming.paused {
                    streaming.note_stall_opened_overlay();
                    // Focus Reconnect — the action we recommend.
                    streaming.pause_selected = paused_overlay::STALL_FOCUS_INDEX;
                    streaming.set_paused(true);
                }
            }
            session::StallEvent::Recovered => {
                eprintln!("stream stall: media resumed after wake pulse/reconnect");
            }
            session::StallEvent::None => {}
        }

        let Some(streaming) = self.state.streaming_mut() else {
            return Ok(());
        };
        if let Some(code) = streaming.maintain_backend().await {
            self.stop_stream_with_error("error-stream-ended", code)
                .await;
        }
        Ok(())
    }

    /// One-button reconnect (idle/screensaver goal): tear down the stalled
    /// session and restart the same target through the normal
    /// StartingStream → Connecting → Streaming flow. The old console session
    /// is stopped twice at most — `stop()` here, and
    /// `cleanup_active_sessions` at the start of the new connection, which
    /// is idempotent server-side.
    pub(in crate::app) async fn reconnect_stream(&mut self) {
        crate::streaming::video::metrics::flush_csv_log();
        crate::streaming::video::rtp_dump_log::flush_rtp_dump();
        let state = std::mem::replace(&mut self.state, AppState::ModeSelect { selected: 0 });
        let Some(streaming) = state.into_streaming() else {
            return;
        };
        let selected = match streaming.return_target {
            StreamReturnTarget::Titles(selected) | StreamReturnTarget::Consoles(selected) => {
                selected
            }
        };
        let title_id = streaming.title_id.clone();
        let return_target = streaming.return_target;
        let started = std::time::Instant::now();
        eprintln!("stream reconnect: stopping stalled backend...");
        if let Err(error) = streaming.stop().await {
            eprintln!(
                "stream reconnect: old-session stop failed (continuing): {error:#}"
            );
        }
        eprintln!(
            "stream reconnect: old backend stopped in {} ms, restarting target",
            started.elapsed().as_millis()
        );

        use crate::app::stream_session::connection::StreamStartTarget;
        let target = match return_target {
            StreamReturnTarget::Consoles(_) => {
                let Some(console) = self.service.consoles.get(selected).cloned() else {
                    eprintln!("stream reconnect: console list no longer has index {selected}");
                    self.set_state(AppState::ConsoleList { selected });
                    return;
                };
                StreamStartTarget {
                    kind: crate::StreamKind::Home,
                    target_id: console.server_id,
                    game_id: None,
                    label: console.device_name,
                    return_selected: selected,
                }
            }
            StreamReturnTarget::Titles(_) => {
                let Some(title) = self.service.titles.get(selected).cloned() else {
                    eprintln!("stream reconnect: title list no longer has index {selected}");
                    self.set_state(AppState::TitleList { selected });
                    return;
                };
                if let Some(id) = &title_id
                    && &title.id != id
                {
                    eprintln!(
                        "stream reconnect: title list changed at {selected} ({} != {id}); reconnecting to the listed title anyway",
                        title.id
                    );
                }
                StreamStartTarget {
                    kind: crate::StreamKind::Cloud,
                    label: format!("cloud title {}", title.launch_id),
                    target_id: title.launch_id,
                    game_id: Some(title.id),
                    return_selected: selected,
                }
            }
        };
        self.start_stream_for_target(target);
    }

    async fn stop_stream_with_error(&mut self, reason_key: &'static str, error: String) {
        crate::streaming::video::metrics::flush_csv_log();
        // Flush the RAM-buffered RTP dump log to ux0: at session end.
        crate::streaming::video::rtp_dump_log::flush_rtp_dump();
        let state = std::mem::replace(&mut self.state, AppState::ModeSelect { selected: 0 });
        if let Some(streaming) = state.into_streaming() {
            // Teardown breadcrumb (TODO 16a): if the console-close repro ever
            // hangs, this pair pins whether `stop()` is the blocking stage.
            let started = std::time::Instant::now();
            eprintln!("stream teardown: stopping backend...");
            let _ = streaming.stop().await;
            eprintln!("stream teardown: backend stopped in {} ms", started.elapsed().as_millis());
        }
        self.set_localized_error_screen(reason_key, error);
    }

    pub(in crate::app) async fn exit_stream(&mut self) {
        crate::streaming::video::metrics::flush_csv_log();
        // Flush the RAM-buffered RTP dump log to ux0: at session end.
        crate::streaming::video::rtp_dump_log::flush_rtp_dump();
        let state = std::mem::replace(&mut self.state, AppState::ModeSelect { selected: 0 });
        let return_target = if let Some(streaming) = state.into_streaming() {
            let return_target = streaming.return_target;
            let description = streaming.backend_description();
            eprintln!("Exiting stream: stopping {description}...");
            let started = std::time::Instant::now();
            match streaming.stop().await {
                Ok(()) => eprintln!(
                    "stream teardown: backend stopped in {} ms",
                    started.elapsed().as_millis()
                ),
                Err(error) => eprintln!(
                    "Failed to stop {description} after {} ms: {error:#}",
                    started.elapsed().as_millis()
                ),
            }
            return_target
        } else {
            self.set_state(AppState::ModeSelect { selected: 0 });
            return;
        };
        self.set_state(match return_target {
            StreamReturnTarget::Titles(selected) => AppState::TitleList { selected },
            StreamReturnTarget::Consoles(selected) => AppState::ConsoleList { selected },
        });
    }
}
