use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

const MAX_NEW_COLOR_TEXTURES_PER_FRAME: usize = 1;

struct SoftwareTexture {
    pixels: Arc<Vec<u8>>,
    width: u32,
    height: u32,
    pot_width: u32,
    pot_height: u32,
}

pub struct SdlEguiPainter {
    textures: HashMap<egui::TextureId, SoftwareTexture>,
    pending_textures: HashMap<egui::TextureId, egui::epaint::ImageDelta>,
    pending_order: VecDeque<egui::TextureId>,
}

impl Default for SdlEguiPainter {
    fn default() -> Self {
        Self::new()
    }
}

impl SdlEguiPainter {
    pub fn new() -> Self {
        Self {
            textures: HashMap::new(),
            pending_textures: HashMap::new(),
            pending_order: VecDeque::new(),
        }
    }

    pub fn paint(
        &mut self,
        overlay: &mut [u8],
        screen_size: [u32; 2],
        pixels_per_point: f32,
        primitives: &[egui::ClippedPrimitive],
        textures_delta: &egui::TexturesDelta,
    ) {
        self.apply_textures(primitives, textures_delta);

        for clipped_primitive in primitives {
            let Some(clip_rect) =
                screen_clip_rect(clipped_primitive.clip_rect, screen_size, pixels_per_point)
            else {
                continue;
            };
            let egui::epaint::Primitive::Mesh(mesh) = &clipped_primitive.primitive else {
                continue;
            };
            if mesh.indices.is_empty() || mesh.vertices.is_empty() {
                continue;
            }
            let Some(texture) = self.textures.get(&mesh.texture_id) else {
                continue;
            };

            let screen_w = screen_size[0] as f32;
            let screen_h = screen_size[1] as f32;

            // Convert egui vertices to screen-space + UV
            let verts: Vec<SoftwareVertex> = mesh
                .vertices
                .iter()
                .map(|v| {
                    let pos = egui::pos2(
                        v.pos.x * pixels_per_point,
                        v.pos.y * pixels_per_point,
                    );
                    let uv = egui::pos2(
                        v.uv.x * texture.width as f32 / texture.pot_width as f32,
                        v.uv.y * texture.height as f32 / texture.pot_height as f32,
                    );
                    SoftwareVertex { pos, uv, color: v.color }
                })
                .collect();

            // Rasterize each triangle
            for chunk in mesh.indices.chunks(3) {
                if chunk.len() < 3 {
                    continue;
                }
                let v0 = &verts[chunk[0] as usize];
                let v1 = &verts[chunk[1] as usize];
                let v2 = &verts[chunk[2] as usize];
                rasterize_triangle(overlay, screen_w, screen_h, v0, v1, v2, texture, clip_rect);
            }
        }

        for texture_id in &textures_delta.free {
            self.textures.remove(texture_id);
            self.pending_textures.remove(texture_id);
        }
        self.pending_order
            .retain(|texture_id| self.pending_textures.contains_key(texture_id));
    }

    fn apply_textures(
        &mut self,
        primitives: &[egui::ClippedPrimitive],
        textures_delta: &egui::TexturesDelta,
    ) {
        for (texture_id, delta) in &textures_delta.set {
            let is_new_color_texture = delta.pos.is_none()
                && !self.textures.contains_key(texture_id)
                && matches!(delta.image, egui::ImageData::Color(_));
            if is_new_color_texture {
                if !self.pending_textures.contains_key(texture_id) {
                    self.pending_order.push_back(*texture_id);
                }
                self.pending_textures.insert(*texture_id, delta.clone());
                continue;
            }
            upload_texture(&mut self.textures, *texture_id, delta);
        }

        let visible_texture_ids: HashSet<_> = primitives
            .iter()
            .filter_map(|primitive| match &primitive.primitive {
                egui::epaint::Primitive::Mesh(mesh) => Some(mesh.texture_id),
                egui::epaint::Primitive::Callback(_) => None,
            })
            .collect();

        for _ in 0..MAX_NEW_COLOR_TEXTURES_PER_FRAME {
            let Some(index) = self
                .pending_order
                .iter()
                .enumerate()
                .filter(|(_, texture_id)| visible_texture_ids.contains(texture_id))
                .max_by_key(|(_, texture_id)| {
                    self.pending_textures
                        .get(texture_id)
                        .map(|delta| delta.image.width() * delta.image.height())
                        .unwrap_or(0)
                })
                .map(|(index, _)| index)
            else {
                break;
            };
            let texture_id = self
                .pending_order
                .remove(index)
                .expect("pending texture index disappeared");
            let Some(delta) = self.pending_textures.remove(&texture_id) else {
                continue;
            };
            upload_texture(&mut self.textures, texture_id, &delta);
        }
    }
}

struct SoftwareVertex {
    pos: egui::Pos2,
    uv: egui::Pos2,
    color: egui::Color32,
}

fn upload_texture(
    textures: &mut HashMap<egui::TextureId, SoftwareTexture>,
    texture_id: egui::TextureId,
    delta: &egui::epaint::ImageDelta,
) {
    let [w, h] = delta.image.size();
    let pixels = image_to_rgba(&delta.image);
    if delta.pos.is_none() || !textures.contains_key(&texture_id) {
        let pot_w = w.next_power_of_two();
        let pot_h = h.next_power_of_two();
        textures.insert(
            texture_id,
            SoftwareTexture {
                pixels: Arc::new(pixels),
                width: w as u32,
                height: h as u32,
                pot_width: pot_w as u32,
                pot_height: pot_h as u32,
            },
        );
    }
}

fn image_to_rgba(image: &egui::ImageData) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(image.width() * image.height() * 4);
    match image {
        egui::ImageData::Color(image) => {
            for pixel in &image.pixels {
                pixels.extend_from_slice(&pixel.to_srgba_unmultiplied());
            }
        }
        egui::ImageData::Font(image) => {
            for pixel in image.srgba_pixels(None) {
                pixels.extend_from_slice(&pixel.to_srgba_unmultiplied());
            }
        }
    }
    pixels
}

fn screen_clip_rect(
    clip_rect: egui::Rect,
    [screen_w, screen_h]: [u32; 2],
    pixels_per_point: f32,
) -> Option<[i32; 4]> {
    let min_x = (clip_rect.min.x * pixels_per_point).floor().clamp(0.0, screen_w as f32) as i32;
    let min_y = (clip_rect.min.y * pixels_per_point).floor().clamp(0.0, screen_h as f32) as i32;
    let max_x = (clip_rect.max.x * pixels_per_point).ceil().clamp(0.0, screen_w as f32) as i32;
    let max_y = (clip_rect.max.y * pixels_per_point).ceil().clamp(0.0, screen_h as f32) as i32;
    if max_x <= min_x || max_y <= min_y {
        None
    } else {
        Some([min_x, min_y, max_x, max_y])
    }
}

fn rasterize_triangle(
    overlay: &mut [u8],
    _screen_w: f32,
    _screen_h: f32,
    v0: &SoftwareVertex,
    v1: &SoftwareVertex,
    v2: &SoftwareVertex,
    texture: &SoftwareTexture,
    clip: [i32; 4],
) {
    let stride = 960 * 4; // RGBA bytes per row

    // Bounding box
    let min_x = (v0.pos.x.min(v1.pos.x).min(v2.pos.x) as i32).max(clip[0]);
    let min_y = (v0.pos.y.min(v1.pos.y).min(v2.pos.y) as i32).max(clip[1]);
    let max_x = (v0.pos.x.max(v1.pos.x).max(v2.pos.x) as i32).min(clip[2]);
    let max_y = (v0.pos.y.max(v1.pos.y).max(v2.pos.y) as i32).min(clip[3]);

    if max_x <= min_x || max_y <= min_y {
        return;
    }

    let area = -edge_fn(v0, v1, v2);
    if area.abs() < 1e-6 {
        return;
    }
    let inv_area = 1.0 / area;

    // Top-left fill rule requires checking which edges are "top" or "left"
    let _bias0 = edge_bias(v0, v1);
    let _bias1 = edge_bias(v1, v2);
    let _bias2 = edge_bias(v2, v0);

    // Precompute per-triangle constants
    let w01 = edge_coeffs(v0, v1);
    let w12 = edge_coeffs(v1, v2);
    let w20 = edge_coeffs(v2, v0);
    let tex_w = texture.width as f32;
    let tex_h = texture.height as f32;
    let tex_w_usize = texture.width as usize;
    let tex_wm1 = tex_w_usize - 1;
    let tex_hm1 = texture.height as usize - 1;
    let texels = &texture.pixels;
    let vcol_r = v2.color.r() as u32;
    let vcol_g = v2.color.g() as u32;
    let vcol_b = v2.color.b() as u32;
    let vcol_a = v2.color.a() as u32;
    let uv0x = v0.uv.x;
    let uv0y = v0.uv.y;
    let uv1x = v1.uv.x;
    let uv1y = v1.uv.y;
    let uv2x = v2.uv.x;
    let uv2y = v2.uv.y;

    for y in min_y..max_y {
        let row_off = y as usize * stride;
        let yf = y as f32 + 0.5;
        let mut e01 = w01.a * (min_x as f32 + 0.5) + w01.b * yf + w01.c;
        let mut e12 = w12.a * (min_x as f32 + 0.5) + w12.b * yf + w12.c;
        let mut e20 = w20.a * (min_x as f32 + 0.5) + w20.b * yf + w20.c;

        for x in min_x..max_x {
            let b0 = e12 * inv_area;
            let b1 = e20 * inv_area;
            let b2 = e01 * inv_area;

            if b0 >= -1e-5 && b1 >= -1e-5 && b2 >= -1e-5 {
                let u = uv0x * b0 + uv1x * b1 + uv2x * b2;
                let v = uv0y * b0 + uv1y * b1 + uv2y * b2;

                let tx = ((u * tex_w) as usize).min(tex_wm1);
                let ty = ((v * tex_h) as usize).min(tex_hm1);
                let tex_off = (ty * tex_w_usize + tx) * 4;
                let ta = texels[tex_off + 3] as u32;
                let pix_off = row_off + x as usize * 4;

                let src_a = (ta * vcol_a) / 255;
                if src_a == 0 {
                    e01 += w01.a;
                    e12 += w12.a;
                    e20 += w20.a;
                    continue;
                }
                let src_r = (vcol_r * ta) / 255;
                let src_g = (vcol_g * ta) / 255;
                let src_b = (vcol_b * ta) / 255;

                let dst_a = overlay[pix_off + 3] as u32;
                if dst_a == 0 {
                    overlay[pix_off] = src_r as u8;
                    overlay[pix_off + 1] = src_g as u8;
                    overlay[pix_off + 2] = src_b as u8;
                    overlay[pix_off + 3] = src_a as u8;
                } else {
                    let inv = 255 - src_a;
                    overlay[pix_off] = ((src_r * src_a + overlay[pix_off] as u32 * inv) / 255) as u8;
                    overlay[pix_off + 1] = ((src_g * src_a + overlay[pix_off + 1] as u32 * inv) / 255) as u8;
                    overlay[pix_off + 2] = ((src_b * src_a + overlay[pix_off + 2] as u32 * inv) / 255) as u8;
                    overlay[pix_off + 3] = (src_a + dst_a * inv / 255).min(255) as u8;
                }
            }

            e01 += w01.a;
            e12 += w12.a;
            e20 += w20.a;
        }
    }
}

fn edge_fn(a: &SoftwareVertex, b: &SoftwareVertex, c: &SoftwareVertex) -> f32 {
    (b.pos.x - a.pos.x) * (c.pos.y - a.pos.y) - (b.pos.y - a.pos.y) * (c.pos.x - a.pos.x)
}

struct EdgeCoeffs {
    a: f32,
    b: f32,
    c: f32,
}

fn edge_coeffs(a: &SoftwareVertex, b: &SoftwareVertex) -> EdgeCoeffs {
    EdgeCoeffs {
        a: b.pos.y - a.pos.y,
        b: a.pos.x - b.pos.x,
        c: -(a.pos.x * (b.pos.y - a.pos.y) + a.pos.y * (a.pos.x - b.pos.x)),
    }
}

fn edge_bias(a: &SoftwareVertex, b: &SoftwareVertex) -> f32 {
    // Top-left fill rule: if edge is top (same y, x decreases) or left (y increases),
    // bias toward including pixels exactly on the edge.
    if a.pos.y == b.pos.y {
        if a.pos.x < b.pos.x { -1.0 } else { 0.0 } // top edge
    } else if b.pos.y > a.pos.y {
        -1.0 // left edge
    } else {
        0.0
    }
}