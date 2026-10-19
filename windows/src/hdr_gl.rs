//! Draws P010 HDR frames with OpenGL, tone-mapping them to SDR on the GPU:
//! PQ / HLG decoding, BT.2390 highlight roll-off, BT.2020 → BT.709 gamut
//! mapping. Used through an egui paint callback.

use std::sync::{Arc, Mutex};

use eframe::egui_glow::{self, ShaderVersion};
use eframe::glow::{self, HasContext as _};
use egui::{Rect, Ui};

use crate::hdr::{HdrInfo, Transfer};
use crate::snapshot::Image;
use crate::video::{chroma_size, PixelFormat, VideoSink};

const VERTEX: &str = r#"
uniform vec4 u_uv; // left, top, right, bottom
OUT vec2 v_uv;
void main() {
    vec2 p = vec2(float(gl_VertexID & 1), float(gl_VertexID >> 1));
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
    v_uv = vec2(mix(u_uv.x, u_uv.z, p.x), mix(u_uv.w, u_uv.y, p.y));
}
"#;

const FRAGMENT: &str = r#"
uniform sampler2D u_luma;
uniform sampler2D u_chroma;
uniform int u_hlg;
uniform float u_src_peak; // nits
uniform float u_white; // nits shown as SDR white (1.0)
IN vec2 v_uv;
const vec3 LUMA_2020 = vec3(0.2627, 0.6780, 0.0593); // for the HLG OOTF

const float M1 = 0.1593017578125;
const float M2 = 78.84375;
const float C1 = 0.8359375;
const float C2 = 18.8515625;
const float C3 = 18.6875;

float pq_to_nits(float e) {
    float p = pow(max(e, 0.0), 1.0 / M2);
    return 10000.0 * pow(max(p - C1, 0.0) / (C2 - C3 * p), 1.0 / M1);
}

float nits_to_pq(float nits) {
    float y = pow(clamp(nits / 10000.0, 0.0, 1.0), M1);
    return pow((C1 + C2 * y) / (1.0 + C3 * y), M2);
}

float hlg_to_scene(float e) {
    const float A = 0.17883277;
    const float B = 0.28466892;
    const float C = 0.55991073;
    return e <= 0.5 ? e * e / 3.0 : (exp((e - C) / A) + B) / 12.0;
}

// BT.2390 EETF: keeps everything below the knee, rolls highlights off so the
// source peak lands on white instead of clipping.
float tone_map(float nits) {
    float src = nits_to_pq(u_src_peak);
    float dst = nits_to_pq(u_white);
    float e = min(nits_to_pq(nits) / src, 1.0);
    float max_lum = dst / src;
    float ks = 1.5 * max_lum - 0.5;
    if (e > ks) {
        float t = (e - ks) / (1.0 - ks);
        float t2 = t * t;
        float t3 = t2 * t;
        e = (2.0 * t3 - 3.0 * t2 + 1.0) * ks + (t3 - 2.0 * t2 + t) * (1.0 - ks) + (-2.0 * t3 + 3.0 * t2) * max_lum;
    }
    return pq_to_nits(e * src);
}

void main() {
    // P010 keeps 10-bit codes in the top bits of 16-bit samples.
    float y = TEX(u_luma, v_uv).r * (65535.0 / 64.0);
    vec2 c = TEX(u_chroma, v_uv).rg * (65535.0 / 64.0);
    // Limited range, BT.2020 non-constant luminance.
    float yy = (y - 64.0) / 876.0;
    float cb = (c.x - 512.0) / 896.0;
    float cr = (c.y - 512.0) / 896.0;
    vec3 rgb = clamp(vec3(yy + 1.4746 * cr, yy - 0.164553 * cb - 0.571353 * cr, yy + 1.8814 * cb), 0.0, 1.0);

    vec3 nits;
    if (u_hlg == 1) {
        vec3 scene = vec3(hlg_to_scene(rgb.r), hlg_to_scene(rgb.g), hlg_to_scene(rgb.b));
        // HLG OOTF for a 1000 nit display (system gamma 1.2).
        float ys = dot(LUMA_2020, scene);
        nits = 1000.0 * pow(max(ys, 1e-6), 0.2) * scene;
    } else {
        nits = vec3(pq_to_nits(rgb.r), pq_to_nits(rgb.g), pq_to_nits(rgb.b));
    }

    vec3 rgb709 = mat3(1.6605, -0.1246, -0.0182,
                      -0.5876,  1.1329, -0.1006,
                      -0.0728, -0.0083,  1.1187) * nits;

    // Compress highlights by the brightest channel and scale the others with
    // it: hue and saturation stay, and nothing ends up above white.
    float hi = max(rgb709.r, max(rgb709.g, rgb709.b));
    vec3 sdr = hi > 0.0 ? rgb709 * (tone_map(hi) / hi) / u_white : vec3(0.0);

    // Colours outside BT.709: desaturate towards grey rather than clip each
    // channel, which would shift hues.
    float l = clamp(dot(vec3(0.2126, 0.7152, 0.0722), sdr), 0.0, 1.0);
    float lo = min(sdr.r, min(sdr.g, sdr.b));
    if (lo < 0.0) sdr = mix(sdr, vec3(l), clamp(-lo / max(l - lo, 1e-6), 0.0, 1.0));
    sdr = clamp(sdr, 0.0, 1.0);

    // BT.1886 display gamma, plus a little noise against banding in 8 bits.
    vec3 outc = pow(sdr, vec3(1.0 / 2.4));
    float n = fract(sin(dot(gl_FragCoord.xy, vec2(12.9898, 78.233))) * 43758.5453);
    FRAG_COLOR = vec4(outc + (n - 0.5) / 255.0, 1.0);
}
"#;

pub struct Renderer {
    program: glow::Program,
    vao: glow::VertexArray,
    luma: glow::Texture,
    chroma: glow::Texture,
    u_uv: Option<glow::UniformLocation>,
    u_hlg: Option<glow::UniformLocation>,
    u_src_peak: Option<glow::UniformLocation>,
    u_white: Option<glow::UniformLocation>,
    /// Size of the textures, so we know when to reallocate them.
    size: (usize, usize),
    serial: u64,
    /// Set by the app: on the next paint, also render the frame at its own
    /// resolution into `captured` (for saving it).
    pub capture: bool,
    /// `Some(None)` when the capture failed.
    pub captured: Option<Option<Image>>,
}

/// Shared between the app and its paint callbacks.
pub type Shared = Arc<Mutex<Renderer>>;

impl Renderer {
    /// `None` when the GL context can't do this (no 16-bit textures).
    pub fn new(gl: &glow::Context) -> Option<Self> {
        let version = ShaderVersion::get(gl);
        let ok = match version {
            ShaderVersion::Gl140 => true,
            ShaderVersion::Es300 => gl.supported_extensions().contains("GL_EXT_texture_norm16"),
            ShaderVersion::Gl120 | ShaderVersion::Es100 => false,
        };
        if !ok {
            return None;
        }
        let prefix = if version.is_embedded() { "precision highp float;\nprecision highp int;\n" } else { "" };
        let vertex = format!(
            "{}{prefix}#define OUT out\n{VERTEX}",
            version.version_declaration()
        );
        let fragment = format!(
            "{}{prefix}#define IN in\n#define TEX texture\n#define FRAG_COLOR out_color\nout vec4 out_color;\n{FRAGMENT}",
            version.version_declaration()
        );
        unsafe {
            let program = gl.create_program().ok()?;
            let mut shaders = Vec::new();
            for (kind, src) in [(glow::VERTEX_SHADER, &vertex), (glow::FRAGMENT_SHADER, &fragment)] {
                let shader = gl.create_shader(kind).ok()?;
                gl.shader_source(shader, src);
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    eprintln!("HDR shader: {}", gl.get_shader_info_log(shader));
                    return None;
                }
                gl.attach_shader(program, shader);
                shaders.push(shader);
            }
            gl.link_program(program);
            for s in shaders {
                gl.detach_shader(program, s);
                gl.delete_shader(s);
            }
            if !gl.get_program_link_status(program) {
                eprintln!("HDR shader: {}", gl.get_program_info_log(program));
                return None;
            }
            let texture = || -> Option<glow::Texture> {
                let t = gl.create_texture().ok()?;
                gl.bind_texture(glow::TEXTURE_2D, Some(t));
                for (param, value) in [
                    (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                    (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                    (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                    (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
                ] {
                    gl.tex_parameter_i32(glow::TEXTURE_2D, param, value as i32);
                }
                Some(t)
            };
            let renderer = Self {
                vao: gl.create_vertex_array().ok()?,
                luma: texture()?,
                chroma: texture()?,
                u_uv: gl.get_uniform_location(program, "u_uv"),
                u_hlg: gl.get_uniform_location(program, "u_hlg"),
                u_src_peak: gl.get_uniform_location(program, "u_src_peak"),
                u_white: gl.get_uniform_location(program, "u_white"),
                program,
                size: (0, 0),
                serial: 0,
                capture: false,
                captured: None,
            };
            gl.use_program(Some(program));
            gl.uniform_1_i32(gl.get_uniform_location(program, "u_luma").as_ref(), 0);
            gl.uniform_1_i32(gl.get_uniform_location(program, "u_chroma").as_ref(), 1);
            gl.use_program(None);
            Some(renderer)
        }
    }

    /// Uploads the sink's frame if it changed. False when there's no P010 frame.
    fn upload(&mut self, gl: &glow::Context, sink: &VideoSink) -> bool {
        sink.with_frame(|f| {
            if f.format != PixelFormat::P010 || f.width == 0 {
                return false;
            }
            if f.serial == self.serial {
                return true;
            }
            self.serial = f.serial;
            let (y, uv) = f.p010_planes();
            let (cw, ch) = chroma_size(f.width, f.height);
            let realloc = self.size != (f.width, f.height);
            self.size = (f.width, f.height);
            unsafe {
                gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
                for (tex, w, h, internal, format, data) in [
                    (self.luma, f.width, f.height, glow::R16, glow::RED, y),
                    (self.chroma, cw, ch, glow::RG16, glow::RG, uv),
                ] {
                    gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                    let data = glow::PixelUnpackData::Slice(Some(data));
                    if realloc {
                        gl.tex_image_2d(
                            glow::TEXTURE_2D,
                            0,
                            internal as i32,
                            w as i32,
                            h as i32,
                            0,
                            format,
                            glow::UNSIGNED_SHORT,
                            data,
                        );
                    } else {
                        gl.tex_sub_image_2d(glow::TEXTURE_2D, 0, 0, 0, w as i32, h as i32, format, glow::UNSIGNED_SHORT, data);
                    }
                }
            }
            true
        })
    }

    /// Renders the `uv` part of the frame at its native size, off screen, and
    /// reads it back. Leaves the screen framebuffer bound.
    fn read_back(&self, gl: &glow::Context, uv: Rect, info: HdrInfo, white: f32) -> Option<Image> {
        let w = ((uv.width() * self.size.0 as f32).round() as i32).max(1);
        let h = ((uv.height() * self.size.1 as f32).round() as i32).max(1);
        unsafe {
            let tex = gl.create_texture().ok()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                w,
                h,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            let fbo = gl.create_framebuffer().ok();
            let mut image = None;
            if let Some(fbo) = fbo {
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
                gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), 0);
                if gl.check_framebuffer_status(glow::FRAMEBUFFER) == glow::FRAMEBUFFER_COMPLETE {
                    gl.disable(glow::SCISSOR_TEST);
                    gl.viewport(0, 0, w, h);
                    self.draw(gl, uv, info, white);
                    let (w, h) = (w as usize, h as usize);
                    let mut rgba = vec![0u8; w * h * 4];
                    gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
                    gl.read_pixels(0, 0, w as i32, h as i32, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelPackData::Slice(Some(&mut rgba)));
                    // GL rows go bottom to top.
                    let row = w * 4;
                    let flipped = rgba.chunks_exact(row).rev().flatten().copied().collect();
                    image = Some(Image { width: w, height: h, rgba: flipped });
                }
                gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                gl.delete_framebuffer(fbo);
            }
            gl.delete_texture(tex);
            image
        }
    }

    fn draw(&self, gl: &glow::Context, uv: Rect, info: HdrInfo, white: f32) {
        unsafe {
            gl.disable(glow::BLEND);
            gl.use_program(Some(self.program));
            gl.uniform_4_f32(self.u_uv.as_ref(), uv.left(), uv.top(), uv.right(), uv.bottom());
            gl.uniform_1_i32(self.u_hlg.as_ref(), (info.transfer == Transfer::Hlg) as i32);
            gl.uniform_1_f32(self.u_src_peak.as_ref(), info.peak_nits);
            gl.uniform_1_f32(self.u_white.as_ref(), white.min(info.peak_nits));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.luma));
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.chroma));
            gl.bind_vertex_array(Some(self.vao));
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            gl.bind_vertex_array(None);
            gl.active_texture(glow::TEXTURE0);
        }
    }
}

/// Queues drawing the sink's current HDR frame into `target`, showing the
/// `uv` part of it (for cropping). `white` is how bright (in nits) a pixel
/// must be to come out white: higher makes the picture darker.
pub fn paint(ui: &Ui, renderer: &Shared, sink: &Arc<VideoSink>, target: Rect, uv: Rect, info: HdrInfo, white: f32) {
    let renderer = renderer.clone();
    let sink = sink.clone();
    let callback = egui_glow::CallbackFn::new(move |_info, painter| {
        let gl = painter.gl();
        let mut r = renderer.lock().unwrap();
        if r.upload(gl, &sink) {
            r.draw(gl, uv, info, white);
            if std::mem::take(&mut r.capture) {
                // egui restores its own GL state after this callback.
                r.captured = Some(r.read_back(gl, uv, info, white));
            }
        }
    });
    ui.painter().add(egui::PaintCallback { rect: target, callback: Arc::new(callback) });
}
