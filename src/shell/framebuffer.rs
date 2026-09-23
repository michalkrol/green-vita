use crate::streaming::video::VideoTextureTarget;
use crate::streaming::video::memory::CdramBlock;
use anyhow::Result;
use vitasdk_sys::*;

pub(crate) fn debug_log(msg: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true).append(true).open("ux0:/data/xcloud-rust/debug.txt")
    {
        let _ = std::io::Write::write(&mut f, msg.as_bytes());
        let _ = std::io::Write::write(&mut f, b"\n");
    }
}

pub const NUM_FB: usize = 3;
pub const FB_WIDTH: u32 = 960;
pub const FB_HEIGHT: u32 = 544;
pub const FB_STRIDE: usize = FB_WIDTH as usize * 4;
const FB_PITCH_PIXELS: u32 = FB_WIDTH;
pub const FB_SIZE: u32 = FB_STRIDE as u32 * FB_HEIGHT;
const FB_PIXELFORMAT: u32 = SCE_DISPLAY_PIXELFORMAT_A8B8G8R8 as u32;

pub(crate) struct DirectFramebuffers {
    pub(crate) blocks: [CdramBlock; NUM_FB],
    pub(crate) targets: [VideoTextureTarget; NUM_FB],
    displayed: usize,
}

impl DirectFramebuffers {
    pub(crate) fn allocate() -> Result<Self> {
        let mut raw_blocks: Vec<CdramBlock> = Vec::with_capacity(NUM_FB);
        for i in 0..NUM_FB {
            raw_blocks.push(CdramBlock::allocate(&format!("xhome_fb_{i}"), FB_SIZE)?);
        }
        let blocks: [CdramBlock; NUM_FB] = raw_blocks.try_into()
            .map_err(|_| anyhow::anyhow!("NUM_FB mismatch in framebuffer allocation"))?;
        debug_log(&format!("FB: allocated {} blocks ok", NUM_FB));

        let mut targets = [VideoTextureTarget { ptr: 0, pitch: 0, capacity: 0 }; NUM_FB];
        for (i, block) in blocks.iter().enumerate() {
            targets[i] = VideoTextureTarget {
                ptr: block.ptr as usize,
pitch: FB_STRIDE as u32,
                capacity: FB_SIZE,
            };
        }

        debug_log(&format!("FB: allocated {} blocks ok, first ptr={:#x}", NUM_FB, blocks[0].ptr as usize));
        Ok(Self { blocks, targets, displayed: NUM_FB })
    }

    pub(crate) fn flip(&mut self, index: usize) {
        if index == self.displayed || index >= NUM_FB {
            return;
        }
        let block = &self.blocks[index];
        let fb = SceDisplayFrameBuf {
            size: core::mem::size_of::<SceDisplayFrameBuf>() as SceSize,
            base: block.ptr as *mut core::ffi::c_void,
            pitch: FB_PITCH_PIXELS,
            pixelformat: FB_PIXELFORMAT,
            width: FB_WIDTH,
            height: FB_HEIGHT,
        };
        debug_log(&format!("FB: flip({index}) calling sceDisplaySetFrameBuf, ptr={:#x}", block.ptr as usize));
        let ret = unsafe { sceDisplaySetFrameBuf(&fb as *const _, SCE_DISPLAY_SETBUF_NEXTFRAME as u32) };
        if ret >= 0 {
            self.displayed = index;
            debug_log(&format!("FB: flip({index}) ok"));
        } else {
            debug_log(&format!("FB: flip({index}) failed: {ret:#x}"));
        }
    }
}