use crate::shell::framebuffer::{FB_HEIGHT, FB_WIDTH, debug_log};
use anyhow::{Context, Result};
use std::collections::{HashMap, VecDeque};
use std::ffi::CStr;
use vitasdk_sys::*;

// ── Pre-compiled SDL vitagxm shaders (from SDL_render_vita_gxm_shaders.h) ──
//
// texture_v: vertex shader with attributes aPosition (F32×2), aTexcoord
// (F32×2), aColor (U8N×4), uniform float4x4 wvp.
// texture_f: fragment shader with tex0 sampler + vertex color modulation.
//
// These are lifted from SDL's vitagxm backend (zlib license compatible).
//
// Wrapped in repr(align(4)) so sceGxmProgramCheck's 32-bit reads don't fault
// on ARM (static const [u8] has alignment 1).

#[repr(C, align(4))]
struct AlignedGxp([u8; 400]);

#[rustfmt::skip]
const TEXTURE_V_GXP: AlignedGxp = AlignedGxp([
    0x47, 0x58, 0x50, 0x00, 0x01, 0x05, 0x50, 0x03,
    0x8f, 0x01, 0x00, 0x00, 0x60, 0x1e, 0x69, 0x97,
    0x82, 0x7e, 0x0c, 0xac, 0x00, 0x00, 0x19, 0x00,
    0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00,
    0x08, 0x01, 0x00, 0x00, 0x70, 0x00, 0x00, 0x00,
    0x0c, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x01, 0x00, 0x0a, 0x00, 0x00, 0x00,
    0x98, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00,
    0x74, 0x00, 0x00, 0x00, 0x88, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00,
    0xc0, 0x00, 0x00, 0x00, 0xc0, 0x3d, 0x03, 0x00,
    0x00, 0x00, 0x00, 0x00, 0xb4, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0xb4, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0xa4, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x9c, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x94, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x33, 0x0f, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x18, 0x00, 0x0a,
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x06, 0x43, 0x00, 0x61,
    0x86, 0x81, 0xa2, 0x00, 0x07, 0x53, 0x40, 0x61,
    0x86, 0x81, 0xa2, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x40, 0x01, 0x04, 0xf8, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x44, 0xfa,
    0x01, 0x0e, 0x01, 0x01, 0x02, 0x00, 0x10, 0xfa,
    0x80, 0x00, 0x08, 0x83, 0x21, 0x25, 0x80, 0x38,
    0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x14, 0xfa,
    0x00, 0x00, 0xf0, 0x83, 0x20, 0x0d, 0x80, 0x38,
    0x0a, 0x84, 0xb9, 0xff, 0xbc, 0x0d, 0xc0, 0x40,
    0x02, 0x11, 0x45, 0xcf, 0x80, 0x8f, 0xb1, 0x18,
    0x00, 0x11, 0x01, 0xc0, 0x81, 0x81, 0xb1, 0x18,
    0x01, 0xd1, 0x42, 0xc0, 0x81, 0x81, 0xb1, 0x18,
    0x00, 0x00, 0x20, 0xa0, 0x00, 0x50, 0x27, 0xfb,
    0x0e, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00,
    0x40, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x3a, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00,
    0x34, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00,
    0x2b, 0x00, 0x00, 0x00, 0x01, 0xe4, 0x00, 0x00,
    0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x61, 0x50, 0x6f, 0x73, 0x69, 0x74, 0x69, 0x6f,
    0x6e, 0x00, 0x61, 0x54, 0x65, 0x78, 0x63, 0x6f,
    0x6f, 0x72, 0x64, 0x00, 0x61, 0x43, 0x6f, 0x6c,
    0x6f, 0x72, 0x00, 0x77, 0x76, 0x70, 0x00, 0x00,
]);

#[repr(C, align(4))]
struct AlignedGxpF([u8; 288]);

#[rustfmt::skip]
const TEXTURE_F_GXP: AlignedGxpF = AlignedGxpF([
    0x47, 0x58, 0x50, 0x00, 0x01, 0x05, 0x50, 0x03,
    0x20, 0x01, 0x00, 0x00, 0xeb, 0x4f, 0xb5, 0xba,
    0x60, 0xb2, 0xd0, 0x8d, 0x05, 0x18, 0x18, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
    0xc4, 0x00, 0x00, 0x00, 0x70, 0x00, 0x00, 0x00,
    0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x01, 0x00, 0x05, 0x00, 0x00, 0x00,
    0x84, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x78, 0x00, 0x00, 0x00, 0x74, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x84, 0x00, 0x00, 0x00, 0xc0, 0x3d, 0x03, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x78, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x70, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x68, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x60, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x58, 0x00, 0x00, 0x00,
    0x64, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x04,
    0x01, 0x00, 0x01, 0x00, 0x04, 0x00, 0x00, 0x00,
    0x00, 0xa9, 0xd0, 0x0e, 0x00, 0x00, 0x00, 0x00,
    0xf0, 0x00, 0x00, 0x00, 0x30, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x07, 0x44, 0xfa, 0x00, 0x00, 0x00, 0x00,
    0x40, 0x09, 0x00, 0xf8, 0x02, 0x80, 0x99, 0xaf,
    0xbc, 0x0d, 0xc0, 0x40, 0x06, 0x82, 0xb9, 0xaf,
    0xbc, 0x0d, 0x80, 0x40, 0x7c, 0x0f, 0x04, 0x00,
    0x86, 0x47, 0xa4, 0x10, 0x30, 0x00, 0x00, 0x00,
    0x02, 0x04, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x01, 0x03, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x74, 0x65, 0x78, 0x00,
]);

pub type SceGxmShaderPatcherId = *mut SceGxmRegisteredProgram;

// ── Vertex format: position(F32×2) + texcoord(F32×2) + color(U8N×4) ──
// Matches SDL's texture_vertex layout and the pre-compiled .gxp attribute
// declarations (20 bytes per vertex).

#[derive(Clone, Copy)]
#[repr(C, align(16))]
struct TextureVertex {
    x: f32,
    y: f32,
    u: f32,
    v: f32,
    color: u32,
    _pad: [u8; 12],
}

const VERTEX_STRIDE: u16 = 32;

// ── Uncached memory wrapper for Gxm ring buffers ───────────────────

struct GxmBuf {
    uid: SceUID,
    ptr: *mut u8,
    size: u32,
}

impl GxmBuf {
    fn alloc(size: u32, name: &CStr) -> Result<Self> {
        unsafe {
            let aligned = size.next_multiple_of(4096);
            let uid = sceKernelAllocMemBlock(
                name.as_ptr(),
                SCE_KERNEL_MEMBLOCK_TYPE_USER_RW_UNCACHE,
                aligned,
                std::ptr::null_mut(),
            );
            if uid < 0 {
                anyhow::bail!("sceKernelAllocMemBlock({name:?}) failed: {uid:#x}");
            }
            let mut base: *mut c_void = std::ptr::null_mut();
            let ret = sceKernelGetMemBlockBase(uid, &mut base);
            if ret < 0 {
                sceKernelFreeMemBlock(uid);
                anyhow::bail!("sceKernelGetMemBlockBase({name:?}) failed: {ret:#x}");
            }
            let ret = sceGxmMapMemory(base, aligned, SCE_GXM_MEMORY_ATTRIB_READ);
            if ret < 0 {
                sceKernelFreeMemBlock(uid);
                anyhow::bail!("sceGxmMapMemory({name:?}) failed: {ret:#x}");
            }
            Ok(GxmBuf { uid, ptr: base.cast(), size: aligned })
        }
    }

    fn as_ptr(&self) -> *mut c_void {
        self.ptr as *mut c_void
    }
}

impl Drop for GxmBuf {
    fn drop(&mut self) {
        unsafe {
            sceGxmUnmapMemory(self.ptr as *mut c_void);
            sceKernelFreeMemBlock(self.uid);
        }
    }
}

// ── Fragment USSE ring (UNCACHE + sceGxmMapFragmentUsseMemory) ─────

struct FragUsseRing {
    uid: SceUID,
    ptr: *mut u8,
    offset: u32,
}

impl FragUsseRing {
    fn alloc(size: u32, name: &CStr) -> Result<Self> {
        unsafe {
            let aligned = size.next_multiple_of(4096);
            let uid = sceKernelAllocMemBlock(
                name.as_ptr(),
                SCE_KERNEL_MEMBLOCK_TYPE_USER_RW_UNCACHE,
                aligned,
                std::ptr::null_mut(),
            );
            if uid < 0 {
                anyhow::bail!("alloc USSE ring {name:?} failed: {uid:#x}");
            }
            let mut base: *mut c_void = std::ptr::null_mut();
            sceKernelGetMemBlockBase(uid, &mut base);
            let mut off: u32 = 0;
            let ret = sceGxmMapFragmentUsseMemory(base, aligned, &mut off);
            if ret < 0 {
                sceKernelFreeMemBlock(uid);
                anyhow::bail!("sceGxmMapFragmentUsseMemory({name:?}) failed: {ret:#x}");
            }
            Ok(FragUsseRing { uid, ptr: base.cast(), offset: off })
        }
    }
}

impl Drop for FragUsseRing {
    fn drop(&mut self) {
        unsafe {
            sceGxmUnmapFragmentUsseMemory(self.ptr as *mut c_void);
            sceKernelFreeMemBlock(self.uid);
        }
    }
}

// ── Display queue callback (required by sceGxmInitialize) ──────────
unsafe extern "C" {
    fn malloc(size: usize) -> *mut c_void;
    fn free(ptr: *mut c_void);
}

unsafe extern "C" fn display_callback(_cb_data: *const c_void) {}

// ── Shader patcher callbacks (heap-backed) ─────────────────────────
unsafe extern "C" fn patcher_host_alloc(
    _user_data: *mut c_void,
    size: SceSize,
) -> *mut c_void {
    unsafe { malloc(size as usize) }
}

unsafe extern "C" fn patcher_host_free(
    _user_data: *mut c_void,
    ptr: *mut c_void,
) {
    unsafe { free(ptr) }
}

// ── Shader program parameter handles ───────────────────────────────
struct ProgramParams {
    wvp_param: *const SceGxmProgramParameter,
    tex_unit: u32,
}

// ── GxmTexture cache entry (holds pixel data alive) ────────────────
struct GxmTextureEntry {
    gxm_tex: SceGxmTexture,
    _buf: GxmBuf,
}

// ── GxmRenderer ───────────────────────────────────────────────────

pub struct GxmRenderer {
    context: *mut SceGxmContext,
    render_target: *mut SceGxmRenderTarget,
    shader_patcher: *mut SceGxmShaderPatcher,

    vertex_program: *mut SceGxmVertexProgram,
    fragment_program: *mut SceGxmFragmentProgram,
    v_registered: SceGxmShaderPatcherId,
    f_registered: SceGxmShaderPatcherId,

    params: ProgramParams,

// Sync objects (kernel-managed, pointer is a handle)
    vertex_sync: *mut SceGxmSyncObject,
    fragment_sync: *mut SceGxmSyncObject,
    depth_stencil: SceGxmDepthStencilSurface,

    // per-frame vertex buffer (CDRAM, never freed so async GPU reads are safe)
    vertex_data: Vec<TextureVertex>,
    vtx_buf: GxmBuf,
    vtx_capacity: u32,

    // sequential index buffer in CDRAM (GPU must read from mapped memory)
    idx_buf: GxmBuf,
    idx_count: u32,

    // GPU texture cache for egui
    textures: HashMap<egui::TextureId, GxmTextureEntry>,
    pending_textures: HashMap<egui::TextureId, egui::epaint::ImageDelta>,
    pending_order: VecDeque<egui::TextureId>,

    // CDRAM blocks (keep alive)
    _host_mem: GxmBuf,
    _vdm: GxmBuf,
    _vertex_ring: GxmBuf,
    _fragment_ring: GxmBuf,
    _frag_usse_ring: FragUsseRing,
    _patcher_buf: GxmBuf,
    _patcher_v_usse: GxmBuf,
    _patcher_f_usse: GxmBuf,

    // GXP program data (must outlive shader patcher registration)
    _v_gxp: Vec<u8>,
    _f_gxp: Vec<u8>,

    // Fallback 1x1 white texture (bound when no egui texture is available)
    _fallback_buf: GxmBuf,
    fallback_tex: SceGxmTexture,
}

impl GxmRenderer {
    pub fn new() -> Result<Self> {
        unsafe {
debug_log("GXM: init start");

            // SceGxm_stub auto-initializes Gxm on module load with default params.
            // Do NOT terminate+re-init — that corrupts state.
            debug_log("GXM: using stub's default initialization");

            // ── Allocate ring buffers ────────────────────────────────
            let vdm = GxmBuf::alloc(SCE_GXM_DEFAULT_VDM_RING_BUFFER_SIZE, c"gxm_vdm")?;
            debug_log(&format!("GXM: vdm ptr={:#x}", vdm.as_ptr() as usize));
            let vertex_ring = GxmBuf::alloc(
                SCE_GXM_DEFAULT_VERTEX_RING_BUFFER_SIZE, c"gxm_vertex_ring")?;
            let fragment_ring = GxmBuf::alloc(
                SCE_GXM_DEFAULT_FRAGMENT_RING_BUFFER_SIZE, c"gxm_frag_ring")?;
            let frag_usse_ring = FragUsseRing::alloc(
                SCE_GXM_DEFAULT_FRAGMENT_USSE_RING_BUFFER_SIZE, c"gxm_frag_usse")?;
            // Host memory: 128KB in UNCACHE (minimum 2KB is too small for uniform pool)
            let host_mem = GxmBuf::alloc(128 * 1024, c"gxm_host")?;

            // ── Create context ───────────────────────────────────────
            let context_params = SceGxmContextParams {
                hostMem: host_mem.as_ptr(),
                hostMemSize: 128 * 1024,
                vdmRingBufferMem: vdm.as_ptr(),
                vdmRingBufferMemSize: SCE_GXM_DEFAULT_VDM_RING_BUFFER_SIZE as SceSize,
                vertexRingBufferMem: vertex_ring.as_ptr(),
                vertexRingBufferMemSize: SCE_GXM_DEFAULT_VERTEX_RING_BUFFER_SIZE as SceSize,
                fragmentRingBufferMem: fragment_ring.as_ptr(),
                fragmentRingBufferMemSize: SCE_GXM_DEFAULT_FRAGMENT_RING_BUFFER_SIZE as SceSize,
                fragmentUsseRingBufferMem: frag_usse_ring.ptr as *mut c_void,
                fragmentUsseRingBufferMemSize: SCE_GXM_DEFAULT_FRAGMENT_USSE_RING_BUFFER_SIZE as SceSize,
                fragmentUsseRingBufferOffset: frag_usse_ring.offset,
            };
            let mut context: *mut SceGxmContext = std::ptr::null_mut();
            let ret = sceGxmCreateContext(&raw const context_params, &mut context);
            if ret < 0 {
                anyhow::bail!("sceGxmCreateContext failed: {ret:#x}");
            }
            debug_log("GXM: context created ok");

            // ── Allocate patcher memory ──────────────────────────────
            let patcher_buf = GxmBuf::alloc(64 * 1024, c"gxm_patcher_buf")?;
            let patcher_v_usse = GxmBuf::alloc(64 * 1024, c"gxm_patcher_v_usse")?;
            let patcher_f_usse = GxmBuf::alloc(64 * 1024, c"gxm_patcher_f_usse")?;

            let mut vertex_usse_off: u32 = 0;
            sceGxmMapVertexUsseMemory(
                patcher_v_usse.as_ptr(), 64 * 1024, &mut vertex_usse_off);
            let mut frag_usse_off: u32 = 0;
            sceGxmMapFragmentUsseMemory(
                patcher_f_usse.as_ptr(), 64 * 1024, &mut frag_usse_off);

            // ── Create shader patcher ────────────────────────────────
            let patcher_params = SceGxmShaderPatcherParams {
                userData: std::ptr::null_mut(),
                hostAllocCallback: Some(patcher_host_alloc),
                hostFreeCallback: Some(patcher_host_free),
                bufferAllocCallback: None,
                bufferFreeCallback: None,
                bufferMem: patcher_buf.as_ptr(),
                bufferMemSize: 64 * 1024,
                vertexUsseAllocCallback: None,
                vertexUsseFreeCallback: None,
                vertexUsseMem: patcher_v_usse.as_ptr(),
                vertexUsseMemSize: 64 * 1024,
                vertexUsseOffset: vertex_usse_off,
                fragmentUsseAllocCallback: None,
                fragmentUsseFreeCallback: None,
                fragmentUsseMem: patcher_f_usse.as_ptr(),
                fragmentUsseMemSize: 64 * 1024,
                fragmentUsseOffset: frag_usse_off,
            };
            let mut shader_patcher: *mut SceGxmShaderPatcher = std::ptr::null_mut();
            let ret = sceGxmShaderPatcherCreate(&raw const patcher_params, &mut shader_patcher);
            if ret < 0 {
                anyhow::bail!("sceGxmShaderPatcherCreate failed: {ret:#x}");
            }
            debug_log("GXM: shader patcher created ok");

            // ── Register pre-compiled shaders ────────────────────────
            let v_prog = &TEXTURE_V_GXP.0 as *const _ as *const SceGxmProgram;
            let f_prog = &TEXTURE_F_GXP.0 as *const _ as *const SceGxmProgram;

            if sceGxmProgramCheck(v_prog) < 0 {
                anyhow::bail!("vertex GXP failed program check");
            }
            if sceGxmProgramCheck(f_prog) < 0 {
                anyhow::bail!("fragment GXP failed program check");
            }
            debug_log("GXM: GXP program check passed");

            // Copy GXP data to heap-allocated memory (Vita GPU driver may not
            // be able to read from .rodata / const data sections).
            debug_log("GXM: copying GXP data to heap...");
            let v_gxp_data: Vec<u8> = TEXTURE_V_GXP.0.to_vec();
            let f_gxp_data: Vec<u8> = TEXTURE_F_GXP.0.to_vec();
            let v_prog = v_gxp_data.as_ptr() as *const SceGxmProgram;
            let f_prog = f_gxp_data.as_ptr() as *const SceGxmProgram;
            debug_log(&format!("GXM: v_prog={:#x} f_prog={:#x}", v_prog as usize, f_prog as usize));

            let mut v_reg: SceGxmShaderPatcherId = std::ptr::null_mut();
            let ret = sceGxmShaderPatcherRegisterProgram(shader_patcher, v_prog, &mut v_reg);
            debug_log(&format!("GXM: register vertex program returned {ret:#x}, v_reg={:#x}", v_reg as usize));
            if ret < 0 { anyhow::bail!("register vertex program: {ret:#x}"); }

            let mut f_reg: SceGxmShaderPatcherId = std::ptr::null_mut();
            let ret = sceGxmShaderPatcherRegisterProgram(shader_patcher, f_prog, &mut f_reg);
            debug_log(&format!("GXM: register fragment program returned {ret:#x}, f_reg={:#x}", f_reg as usize));
            if ret < 0 { anyhow::bail!("register fragment program: {ret:#x}"); }
            debug_log("GXM: programs registered");

            // ── Query GXP register indices via name reflection ─────────
            // SDL's CG shaders don't embed Vita-specific semantics, so we
            // find parameters by name and extract their resource indices.
            debug_log("GXM: querying register indices...");
            let pos_param = sceGxmProgramFindParameterByName(
                v_prog, c"aPosition".as_ptr() as *const c_char);
            let tex_param = sceGxmProgramFindParameterByName(
                v_prog, c"aTexcoord".as_ptr() as *const c_char);
            let col_param = sceGxmProgramFindParameterByName(
                v_prog, c"aColor".as_ptr() as *const c_char);
            debug_log(&format!("GXM: pos={:#x} tex={:#x} col={:#x}",
                pos_param as usize, tex_param as usize, col_param as usize));
            if pos_param.is_null() || tex_param.is_null() || col_param.is_null() {
                anyhow::bail!("attribute parameter lookup failed (missing names?)");
            }

            let pos_reg = sceGxmProgramParameterGetResourceIndex(pos_param) as u16;
            let tex_reg = sceGxmProgramParameterGetResourceIndex(tex_param) as u16;
            let col_reg = sceGxmProgramParameterGetResourceIndex(col_param) as u16;
            debug_log(&format!("GXM: pos_reg={} tex_reg={} col_reg={}", pos_reg, tex_reg, col_reg));

            // ── Create vertex program with reflected register indices ─
            debug_log("GXM: creating vertex program...");
            let mut vertex_program: *mut SceGxmVertexProgram = std::ptr::null_mut();
            let attributes = [
                SceGxmVertexAttribute {
                    streamIndex: 0, offset: 0,
                    format: SCE_GXM_ATTRIBUTE_FORMAT_F32 as u8,
                    componentCount: 2, regIndex: pos_reg,
                },
                SceGxmVertexAttribute {
                    streamIndex: 0, offset: 8,
                    format: SCE_GXM_ATTRIBUTE_FORMAT_F32 as u8,
                    componentCount: 2, regIndex: tex_reg,
                },
                SceGxmVertexAttribute {
                    streamIndex: 0, offset: 16,
                    format: SCE_GXM_ATTRIBUTE_FORMAT_U8N as u8,
                    componentCount: 4, regIndex: col_reg,
                },
            ];
            let streams = [
                SceGxmVertexStream {
                    stride: VERTEX_STRIDE,
                    indexSource: SCE_GXM_INDEX_SOURCE_INDEX_16BIT as u16,
                },
            ];
            let ret = sceGxmShaderPatcherCreateVertexProgram(
                shader_patcher, v_reg,
                attributes.as_ptr(), attributes.len() as u32,
                streams.as_ptr(), streams.len() as u32,
                &mut vertex_program,
            );
            debug_log(&format!("GXM: createVertexProgram returned {ret:#x}, vertex_program={:#x}",
                vertex_program as usize));
            if ret < 0 { anyhow::bail!("create vertex program: {ret:#x}"); }
            debug_log("GXM: vertex program created ok");

            let blend_info = {
                // Use bindgen's setter methods — they handle the correct
                // bitfield layout (colorMask is a separate u8 at offset 0;
                // 6 × 4-bit blend fields are packed in _bitfield_1).
                let mut bi: SceGxmBlendInfo = std::mem::zeroed();
                bi.colorMask = SCE_GXM_COLOR_MASK_ALL as u8;
                bi.set_colorFunc(SCE_GXM_BLEND_FUNC_ADD as u8);
                bi.set_alphaFunc(SCE_GXM_BLEND_FUNC_ADD as u8);
                bi.set_colorSrc(SCE_GXM_BLEND_FACTOR_ONE as u8);
                bi.set_colorDst(SCE_GXM_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA as u8);
                bi.set_alphaSrc(SCE_GXM_BLEND_FACTOR_ONE as u8);
                bi.set_alphaDst(SCE_GXM_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA as u8);
                bi
            };
            debug_log("GXM: blend info constructed, creating fragment program...");
            let mut fragment_program: *mut SceGxmFragmentProgram = std::ptr::null_mut();
            let ret = sceGxmShaderPatcherCreateFragmentProgram(
                shader_patcher, f_reg,
                SCE_GXM_OUTPUT_REGISTER_FORMAT_UCHAR4,
                SCE_GXM_MULTISAMPLE_NONE,
                &raw const blend_info,
                v_prog,
                &mut fragment_program,
            );
            debug_log(&format!("GXM: createFragmentProgram returned {ret:#x}, fragment_program={:#x}",
                fragment_program as usize));
            if ret < 0 { anyhow::bail!("create fragment program: {ret:#x}"); }
            debug_log("GXM: fragment program created");

            // ── Find shader parameters ───────────────────────────────
            debug_log("GXM: getting vertex program ptr...");
            let v_prog_ptr = sceGxmVertexProgramGetProgram(vertex_program);
            debug_log(&format!("GXM: v_prog_ptr={:#x}", v_prog_ptr as usize));
            let wvp_param =
                sceGxmProgramFindParameterByName(v_prog_ptr, c"wvp".as_ptr() as *const c_char);
            debug_log(&format!("GXM: wvp_param={:#x}", wvp_param as usize));
            if wvp_param.is_null() {
                anyhow::bail!("wvp param not found in vertex program");
            }

            debug_log("GXM: getting fragment program ptr...");
            let f_prog_ptr = sceGxmFragmentProgramGetProgram(fragment_program);
            debug_log(&format!("GXM: f_prog_ptr={:#x}", f_prog_ptr as usize));
            let tex_param =
                sceGxmProgramFindParameterByName(f_prog_ptr, c"tex".as_ptr() as *const c_char);
            debug_log(&format!("GXM: tex_param={:#x}", tex_param as usize));
            let tex_unit = sceGxmProgramParameterGetResourceIndex(tex_param) as u32;

            // ── Render target ────────────────────────────────────────────
            // sceGxmBeginScene requires a valid SceGxmRenderTarget handle
            // (cannot be NULL).
            debug_log("GXM: creating render target...");
            let mut render_target: *mut SceGxmRenderTarget = std::ptr::null_mut();
            let rt_params = SceGxmRenderTargetParams {
                flags: 0,
                width: FB_WIDTH as u16,
                height: FB_HEIGHT as u16,
                scenesPerFrame: 1,
                multisampleMode: SCE_GXM_MULTISAMPLE_NONE as u16,
                multisampleLocations: 0,
                driverMemBlock: -1,
            };
            let ret = sceGxmCreateRenderTarget(&raw const rt_params, &mut render_target);
            debug_log(&format!("GXM: createRenderTarget returned {ret:#x}, rt={:#x}", render_target as usize));
            if ret < 0 {
                anyhow::bail!("create render target: {ret:#x}");
            }

            // ── Sync objects (kernel-managed GPU fences) ─────────────────
            // Use sceGxmSyncObjectCreate (kernel manages sync state).
            debug_log("GXM: creating sync objects...");
            let mut vertex_sync: *mut SceGxmSyncObject = std::ptr::null_mut();
            let mut fragment_sync: *mut SceGxmSyncObject = std::ptr::null_mut();
            sceGxmSyncObjectCreate(&mut vertex_sync);
            sceGxmSyncObjectCreate(&mut fragment_sync);

            // ── Depth/stencil (disabled) ─────────────────────────────
            let mut depth_stencil: SceGxmDepthStencilSurface = std::mem::zeroed();
            sceGxmDepthStencilSurfaceInitDisabled(&mut depth_stencil);
            debug_log("GXM: depth stencil init'd");

            // ── Fallback white texture (1×1 RGBA) ────────────────────
            // Always bound so the fragment shader never samples void.
            debug_log("GXM: creating fallback texture...");
            let fallback_buf = GxmBuf::alloc(4, c"gxm_fallback_tex")?;
            unsafe {
                std::ptr::write(fallback_buf.as_ptr() as *mut u32, 0xFFFFFFFFu32);
            }
            let mut fallback_tex: SceGxmTexture = unsafe { std::mem::zeroed() };
            unsafe {
                sceGxmTextureInitLinear(
                    &mut fallback_tex,
                    fallback_buf.as_ptr() as *mut c_void,
                    SCE_GXM_TEXTURE_FORMAT_U8U8U8U8_RGBA,
                    1, 1, 0,
                );
            }

            debug_log("GXM: init complete");

            Ok(GxmRenderer {
                context,
                render_target,
                shader_patcher,
                vertex_program,
                fragment_program,
                v_registered: v_reg,
                f_registered: f_reg,
                params: ProgramParams { wvp_param, tex_unit },
                vertex_sync,
                fragment_sync,
                depth_stencil,
                vertex_data: Vec::new(),
                vtx_buf: GxmBuf::alloc(65536, c"gxm_vtx_buf")?,
                vtx_capacity: 65536 / std::mem::size_of::<TextureVertex>() as u32,
                idx_buf: GxmBuf::alloc(65536, c"gxm_idx_buf")?,
                idx_count: 0,
                textures: HashMap::new(),
                pending_textures: HashMap::new(),
                pending_order: VecDeque::new(),
                _host_mem: host_mem,
                _vdm: vdm,
                _vertex_ring: vertex_ring,
                _fragment_ring: fragment_ring,
                _frag_usse_ring: frag_usse_ring,
                _patcher_buf: patcher_buf,
                _patcher_v_usse: patcher_v_usse,
                _patcher_f_usse: patcher_f_usse,
                _v_gxp: v_gxp_data,
                _f_gxp: f_gxp_data,
                _fallback_buf: fallback_buf,
                fallback_tex,
            })
        }
    }

    // ── Render egui overlay onto framebuffer ────────────────────────
    pub unsafe fn render_overlay(
        &mut self,
        fb_ptr: *mut u8,
        pixels_per_point: f32,
        primitives: &[egui::ClippedPrimitive],
        textures_delta: &egui::TexturesDelta,
) {
        debug_log("GXM: render_overlay enter");
        let ctx = self.context;
        let fb_data = fb_ptr as *mut c_void;

        // Process pending textures
        debug_log("GXM: applying texture deltas");
        self.apply_texture_deltas(textures_delta);

        // Build vertex buffer from egui primitives
        debug_log("GXM: building vertices");
        self.build_vertices(primitives);

        debug_log(&format!("GXM: vertex_data.len={}", self.vertex_data.len()));
        if self.vertex_data.is_empty() {
            debug_log("GXM: no vertices, returning");
            return;
        }

        // Set up color surface wrapping the framebuffer
        debug_log("GXM: setting up color surface");
        let mut color_surface: SceGxmColorSurface = std::mem::zeroed();
        let cs_ret = sceGxmColorSurfaceInit(
            &mut color_surface,
            SCE_GXM_COLOR_FORMAT_U8U8U8U8_ABGR,
            SCE_GXM_COLOR_SURFACE_LINEAR,
            SCE_GXM_COLOR_SURFACE_SCALE_NONE,
            SCE_GXM_OUTPUT_REGISTER_SIZE_32BIT,
            FB_WIDTH as u32,
            FB_HEIGHT as u32,
            FB_WIDTH as u32,
            fb_data,
        );
        // Explicitly set data pointer (some SDK versions don't set it in init)
        sceGxmColorSurfaceSetData(&mut color_surface, fb_data);
        debug_log(&format!("GXM: colorSurfaceInit returned {cs_ret:#x}, fb_data={:#x}", fb_data as usize));
        debug_log(&format!("GXM: vs={:#x} fs={:#x}", self.vertex_sync as usize, self.fragment_sync as usize));

// ── Set up scene state (before BeginScene) ──────────────────
        sceGxmSetFrontDepthFunc(ctx, SCE_GXM_DEPTH_FUNC_ALWAYS);
        sceGxmSetFrontDepthWriteEnable(ctx, 0);
        sceGxmSetCullMode(ctx, SCE_GXM_CULL_NONE);

        debug_log(&format!("GXM: ctx={:#x} vp={:#x} fp={:#x}",
            ctx as usize, self.vertex_program as usize, self.fragment_program as usize));
        debug_log("GXM: calling SetVertexProgram");
        sceGxmSetVertexProgram(ctx, self.vertex_program);
        debug_log("GXM: calling SetFragmentProgram");
        sceGxmSetFragmentProgram(ctx, self.fragment_program);
        debug_log("GXM: programs set");

        // Enable/disable modes (AFTER programs are bound)
        sceGxmSetFrontFragmentProgramEnable(ctx, 0);
        sceGxmSetBackFragmentProgramEnable(ctx, 0);

        // Begin scene
        debug_log("GXM: beginning scene");
        let bs_ret = sceGxmBeginScene(
            ctx,
            0,
            self.render_target,
            std::ptr::null(),
            std::ptr::null_mut(),
            self.fragment_sync,
            &raw const color_surface,
            &raw const self.depth_stencil,
        );
        debug_log(&format!("GXM: beginScene returned {ret:#x}", ret = bs_ret));
        debug_log("GXM: scene begun");

        // Set viewport: maps NDC [-1,1] → screen pixels
        let sw = FB_WIDTH as f32 / 2.0;
        let sh = FB_HEIGHT as f32 / 2.0;
        sceGxmSetViewport(ctx, sw, sw, sh, -sh, 0.5, 0.5);
        debug_log("GXM: viewport set");

        // Set region clip to full screen
        sceGxmSetRegionClip(ctx, SCE_GXM_REGION_CLIP_NONE, 0, 0, FB_WIDTH as u32, FB_HEIGHT as u32);

        // Viewport: clip-to-screen transform per SDL
        let wf = FB_WIDTH as f32;
        let hf = FB_HEIGHT as f32;
        let x_scale = wf * 0.5;
        let x_off = 0.0 + x_scale;
        let y_scale = hf * -0.5;
        let y_off = 0.0 - y_scale;
        sceGxmSetViewport(ctx, x_off, x_scale, y_off, y_scale, 0.5, 0.5);

        // Reserve and set vertex WVP uniform
        debug_log("GXM: reserving uniform buffer");
        let mut wvp_buf: *mut c_void = std::ptr::null_mut();
        let ub_ret = sceGxmReserveVertexDefaultUniformBuffer(ctx, &mut wvp_buf);
        debug_log(&format!("GXM: reserveUniform ret={:#x}, wvp_buf={:#x}", ub_ret, wvp_buf as usize));
        // SDL orthographic: glOrtho(0, w, h, 0, 0, 1) → column-major
        // shader does: mul(float4(pos, 1.0, 0.5), wvp)
        let ortho: [f32; 16] = [
            2.0 / wf, 0.0, 0.0, 0.0,
            0.0, -2.0 / hf, 0.0, 0.0,
            0.0, 0.0, 1.0, 0.0,
            -1.0, 1.0, 0.0, 1.0,
        ];
        debug_log("GXM: setting uniform data");
        sceGxmSetUniformDataF(wvp_buf, self.params.wvp_param, 0, 16, &raw const ortho as *const f32);

        // Bind fragment texture — use egui texture if available, else fallback
        let tex_to_bind = match self.textures.values().next() {
            Some(entry) => &raw const entry.gxm_tex,
            None => &raw const self.fallback_tex,
        };
        sceGxmSetFragmentTexture(ctx, self.params.tex_unit, tex_to_bind);

        // Draw all vertices as one batch (from CDRAM buffers)
        let verts = &self.vertex_data;
        if !verts.is_empty() {
            let count = verts.len();
            assert!(count as u32 <= self.vtx_capacity);

            // Copy vertices to persistent CDRAM buffer
            unsafe {
                std::ptr::copy_nonoverlapping(
                    verts.as_ptr(),
                    self.vtx_buf.as_ptr() as *mut TextureVertex,
                    count,
                );
            }

            // Indices already staged into CDRAM by build_vertices
            debug_log(&format!("GXM: drawing {} vertices with {} indices", count, self.idx_count));
            sceGxmSetVertexStream(ctx, 0, self.vtx_buf.as_ptr());

            if self.idx_count >= 3 {
                sceGxmDraw(
                    ctx,
                    SCE_GXM_PRIMITIVE_TRIANGLES,
                    SCE_GXM_INDEX_FORMAT_U16,
                    self.idx_buf.as_ptr(),
                    self.idx_count,
                );
            }
            debug_log("GXM: draw complete");
        }

        // End scene
        debug_log("GXM: ending scene");
        sceGxmEndScene(ctx, std::ptr::null(), std::ptr::null());
        // Stall CPU until GPU finishes (avoids race on CDRAM buffer reuse)
        sceGxmFinish(ctx);
        debug_log("GXM: render_overlay done");
    }

    fn apply_texture_deltas(&mut self, deltas: &egui::TexturesDelta) {
        for (id, delta) in &deltas.set {
            let (w, h, bytes) = match &delta.image {
                egui::ImageData::Color(img) => {
                    let w = img.width() as u16;
                    let h = img.height() as u16;
                    let bytes: Vec<u8> = img.pixels.iter()
                        .flat_map(|c| [c.r(), c.g(), c.b(), c.a()])
                        .collect();
                    (w, h, bytes)
                }
                egui::ImageData::Font(img) => {
                    let w = img.width() as u16;
                    let h = img.height() as u16;
                    let bytes: Vec<u8> = img.pixels.iter()
                        .flat_map(|&a| {
                            let a = (a.max(0.0).min(1.0) * 255.0) as u8;
                            [255u8, 255, 255, a]
                        })
                        .collect();
                    (w, h, bytes)
                }
            };
            let size = bytes.len();
            let buf = GxmBuf::alloc(size as u32, c"egui_tex");
            let buf = match buf {
                Ok(b) => b,
                Err(_) => continue,
            };
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf.as_ptr() as *mut u8, size);
            }

            let mut gxm_tex: SceGxmTexture = unsafe { std::mem::zeroed() };
            unsafe {
                sceGxmTextureInitLinear(
                    &mut gxm_tex,
                    buf.as_ptr() as *mut c_void,
                    SCE_GXM_TEXTURE_FORMAT_U8U8U8U8_RGBA,
                    w as u32,
                    h as u32,
                    0,
                );
            }

            self.textures.insert(*id, GxmTextureEntry {
                gxm_tex,
                _buf: buf,
            });
        }
        for id in &deltas.free {
            self.textures.remove(id);
        }
    }

    fn build_vertices(&mut self, primitives: &[egui::ClippedPrimitive]) {
        self.vertex_data.clear();
        self.idx_count = 0;
        for ep in primitives {
            let mesh = match &ep.primitive {
                egui::epaint::Primitive::Mesh(mesh) => mesh,
                _ => continue,
            };
            let base = self.vertex_data.len() as u32;
            for v in &mesh.vertices {
                let color = v.color.to_srgba_unmultiplied();
                self.vertex_data.push(TextureVertex {
                    x: v.pos.x,
                    y: v.pos.y,
                    u: v.uv.x,
                    v: v.uv.y,
                    color: color[0] as u32
                        | ((color[1] as u32) << 8)
                        | ((color[2] as u32) << 16)
                        | ((color[3] as u32) << 24),
                    _pad: [0u8; 12],
                });
            }
            // Write this mesh's indices into CDRAM buffer, offset by base vertex
            let idx_ptr = self.idx_buf.as_ptr() as *mut u16;
            let idx_off = self.idx_count as usize;
            for (j, &i) in mesh.indices.iter().enumerate() {
                unsafe { std::ptr::write(idx_ptr.add(idx_off + j), (base + i as u32) as u16); }
            }
            self.idx_count += mesh.indices.len() as u32;
        }
    }
}

impl Drop for GxmRenderer {
    fn drop(&mut self) {
        unsafe {
            if !self.fragment_program.is_null() {
                sceGxmShaderPatcherReleaseFragmentProgram(self.shader_patcher, self.fragment_program);
            }
            if !self.vertex_program.is_null() {
                sceGxmShaderPatcherReleaseVertexProgram(self.shader_patcher, self.vertex_program);
            }
            if !self.v_registered.is_null() {
                sceGxmShaderPatcherForceUnregisterProgram(self.shader_patcher, self.v_registered);
            }
            if !self.f_registered.is_null() {
                sceGxmShaderPatcherForceUnregisterProgram(self.shader_patcher, self.f_registered);
            }
            if !self.shader_patcher.is_null() {
                sceGxmShaderPatcherDestroy(self.shader_patcher);
            }
            if !self.context.is_null() {
                sceGxmDestroyContext(self.context);
            }
            let _ = sceGxmTerminate();
        }
    }
}