use crate::app::stream_session::session::StreamReturnTarget;
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
        if let Some(code) = streaming.maintain_backend().await {
            self.stop_stream_with_error("error-stream-ended", code)
                .await;
        }
        Ok(())
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
