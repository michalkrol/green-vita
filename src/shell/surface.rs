use crate::shell::framebuffer::{debug_log, DirectFramebuffers, FB_HEIGHT, FB_STRIDE, FB_WIDTH, NUM_FB};
use crate::shell::gxm_renderer::GxmRenderer;
use crate::streaming::video::memory::CdramBlock;
use crate::streaming::video::{DirectVideoOutput, VideoTextureTarget, NUM_TEXTURES};
use crate::app::StreamingSession;
use anyhow::{Context, Result};
use std::sync::Arc;
use std::sync::atomic::Ordering;

pub const WIDTH: u32 = 960;
pub const HEIGHT: u32 = 544;
const DECODE_PITCH: usize = WIDTH as usize * 2;   // BGR565 = 2 bpp
const DECODE_SIZE: u32 = DECODE_PITCH as u32 * HEIGHT;

pub struct VitaSurface {
    canvas: sdl2::render::Canvas<sdl2::video::Window>,
    fbs: DirectFramebuffers,
    decode_blocks: [CdramBlock; NUM_TEXTURES],
    decode_targets: [VideoTextureTarget; NUM_TEXTURES],
    displayed_fb: Option<usize>,
    direct_video_output: Option<Arc<DirectVideoOutput>>,
    last_frame_id: u64,
    gxm: GxmRenderer,
}

impl VitaSurface {
pub fn new(video: &sdl2::VideoSubsystem) -> Result<Self> {
        let window = video
            .window("GreenVita", WIDTH, HEIGHT)
            .position_centered()
            .build()?;
        let mut canvas = window
            .into_canvas()
            .accelerated()
            .build()
            .map_err(anyhow::Error::msg)
            .context("failed to create Vita renderer")?;
        canvas
            .set_logical_size(WIDTH, HEIGHT)
            .map_err(anyhow::Error::msg)
            .context("failed to set logical render size")?;
        let fbs = DirectFramebuffers::allocate()?;
        debug_log("SURFACE: DirectFramebuffers allocated ok");
        let mut decode_blocks: Vec<CdramBlock> = Vec::with_capacity(NUM_TEXTURES);
        for i in 0..NUM_TEXTURES {
            decode_blocks.push(CdramBlock::allocate(&format!("xhome_dec_{i}"), DECODE_SIZE)?);
            debug_log(&format!("SURFACE: decode_block[{i}] allocated ok"));
        }
        let decode_blocks: [CdramBlock; NUM_TEXTURES] = decode_blocks.try_into()
            .map_err(|_| anyhow::anyhow!("decode_blocks NUM_TEXTURES mismatch"))?;
        let mut decode_targets = [VideoTextureTarget { ptr: 0, pitch: 0, capacity: 0 }; NUM_TEXTURES];
        for (i, block) in decode_blocks.iter().enumerate() {
            decode_targets[i] = VideoTextureTarget {
                ptr: block.ptr as usize,
                pitch: DECODE_PITCH as u32,
                capacity: DECODE_SIZE,
            };
        }

        debug_log("SURFACE: VitaSurface constructed ok");
        let gxm = GxmRenderer::new().context("failed to create Gxm renderer")?;
        debug_log("SURFACE: Gxm renderer ready");
        Ok(Self {
            canvas,
            fbs,
            decode_blocks,
            decode_targets,
            displayed_fb: None,
            direct_video_output: None,
            last_frame_id: 0,
            gxm,
        })
    }

    pub fn video_rect(&self) -> sdl2::rect::Rect {
        sdl2::rect::Rect::new(0, 0, WIDTH, HEIGHT)
    }

    pub fn window(&self) -> &sdl2::video::Window {
        &self.canvas.window()
    }

    pub fn sync_video_frame(&mut self, streaming: Option<&StreamingSession>) -> Result<()> {
        debug_log("SYNC: called");
        let Some(streaming) = streaming else {
            debug_log("SYNC: no streaming, detaching");
            self.detach_direct_video_output();
            return Ok(());
        };
        self.ensure_direct_video_output(streaming)?;

        let Some((frame_id, frame)) = streaming.video_frame() else {
            debug_log("SYNC: no new frame");
            return Ok(());
        };
        if frame_id == self.last_frame_id {
            debug_log(&format!("SYNC: same frame {frame_id}, skipping"));
            return Ok(());
        }
        debug_log(&format!("SYNC: new frame {frame_id}, index={}", frame.texture_index));
        let index = frame.texture_index;
        if index >= NUM_TEXTURES {
            anyhow::bail!("decoder returned invalid direct texture index {index}");
        }
        crate::streaming::video::metrics::METRICS
            .display_pickup_us
            .store(frame.published_at.elapsed().as_micros() as u64, Ordering::Relaxed);
        crate::streaming::video::metrics::METRICS
            .display_pickup_max_us
            .fetch_max(
                frame.published_at.elapsed().as_micros() as u64,
                Ordering::Relaxed,
            );
        if let Some(output) = &self.direct_video_output {
            output.mark_displayed(index, frame.generation);
        }
        self.displayed_fb = Some(index);
        debug_log(&format!("SYNC: set displayed_fb={index}"));
        crate::streaming::video::metrics::METRICS
            .presented
            .fetch_add(1, Ordering::Relaxed);
        self.last_frame_id = frame_id;
        Ok(())
    }

    fn ensure_direct_video_output(&mut self, streaming: &StreamingSession) -> Result<()> {
        let output = streaming.direct_video_output();
        let output_is_current = self
            .direct_video_output
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &output));
        if !output_is_current && self.direct_video_output.is_some() {
            self.detach_direct_video_output();
        }
        if !output.decoder_ready.load(Ordering::Acquire) {
            return Ok(());
        }
        if output_is_current {
            return Ok(());
        }
        self.detach_direct_video_output();
        debug_log("ENSURE: calling output.set_targets");
        output.set_targets(self.decode_targets);
        debug_log("ENSURE: targets set, storing output");
        self.direct_video_output = Some(output);
        self.last_frame_id = 0;
        Ok(())
    }

    fn detach_direct_video_output(&mut self) {
        if let Some(output) = self.direct_video_output.take() {
            output.clear_targets();
        }
        self.displayed_fb = None;
        self.last_frame_id = 0;
    }

    pub fn draw_scene(&mut self, show_video: bool) -> Result<()> {
        Ok(())
    }

    pub fn paint_egui(
        &mut self,
        pixels_per_point: f32,
        primitives: &[egui::ClippedPrimitive],
        textures_delta: &egui::TexturesDelta,
    ) -> Result<()> {
        let fb = &mut self.fbs;
        if let Some(dec_index) = self.displayed_fb {
            debug_log(&format!("PAINT: stream active, fb={dec_index}"));
            let dec_block = &self.decode_blocks[dec_index];
            let dec_src: &[u8] =
                unsafe { std::slice::from_raw_parts(dec_block.ptr, DECODE_SIZE as usize) };
            let fb_buf: &mut [u8] = unsafe {
                std::slice::from_raw_parts_mut(
                    fb.blocks[dec_index].ptr,
                    FB_STRIDE * FB_HEIGHT as usize,
                )
            };
            // CPU: convert BGR565 → RGBA8888 (no overlay compositing)
            convert_bgr565_to_rgba8888(dec_src, fb_buf);
            // GPU: render overlay with alpha blending on top
            unsafe {
                self.gxm.render_overlay(
                    fb.blocks[dec_index].ptr as *mut u8,
                    pixels_per_point,
                    primitives,
                    textures_delta,
                );
            }
            fb.flip(dec_index);
        } else {
            debug_log("PAINT: no stream, dark blue bg + egui overlay via Gxm");
            // Use framebuffer block 0 as scratch
            let fb_buf: &mut [u8] = unsafe {
                std::slice::from_raw_parts_mut(
                    fb.blocks[0].ptr,
                    FB_STRIDE * FB_HEIGHT as usize,
                )
            };
            // Fill with opaque dark blue
            for pixel in fb_buf.chunks_exact_mut(4) {
                pixel[0] = 0;    // R
                pixel[1] = 24;   // G
                pixel[2] = 48;   // B
                pixel[3] = 255;  // A
            }
            unsafe {
                self.gxm.render_overlay(
                    fb.blocks[0].ptr as *mut u8,
                    pixels_per_point,
                    primitives,
                    textures_delta,
                );
            }
            fb.flip(0);
        }
        Ok(())
    }
}

/// Convert BGR565 → RGBA8888 (no overlay compositing — Gxm handles that).
fn convert_bgr565_to_rgba8888(dec_buf: &[u8], fb_buf: &mut [u8]) {
    let pixels = FB_WIDTH as usize * FB_HEIGHT as usize;
    for i in 0..pixels {
        let s = i * 2;
        let d = i * 4;
        let pixel = dec_buf[s] as u32 | ((dec_buf[s + 1] as u32) << 8);
        let r5 = pixel & 0x1f;
        let g6 = (pixel >> 5) & 0x3f;
        let b5 = (pixel >> 11) & 0x1f;
        fb_buf[d] = ((r5 * 255 + 15) / 31) as u8;     // R
        fb_buf[d + 1] = ((g6 * 255 + 31) / 63) as u8;  // G
        fb_buf[d + 2] = ((b5 * 255 + 15) / 31) as u8;  // B
        fb_buf[d + 3] = 255;                            // A
    }
}