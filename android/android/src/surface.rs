//! Video straight from the hardware decoder, never through the CPU: less
//! heat and battery for 4K, and HDR shown as HDR. (Taken from Resumo TV.)
//! libVLC draws through its `AWindow` (from libVLC's own Java part, shipped
//! in the APK) into
//!
//! * the picture: by default a `SurfaceView` under the app's window, as in
//!   VLC: the decoder's frames go to the screen as they are, HDR included,
//!   and an HDR screen shows them as HDR. The window is see-through where the
//!   picture is (see [`hole`]). Otherwise a `SurfaceTexture` whose frames
//!   are drawn into an ordinary texture that egui shows like any image, with
//!   HDR (PQ) turned into SDR on the way, since the app's own window is SDR;
//! * the subtitles: always a `SurfaceTexture`, drawn by egui over the picture.
//!
//! libVLC draws straight into these (its "android_display" output, which
//! passes the hardware decoder's frames on untouched, HDR metadata and all)
//! only when someone is there to place the picture: `AWindow` needs a video
//! layout listener for it, and otherwise falls back to its OpenGL output,
//! which turns HDR into a washed-out SDR picture. The app places the picture
//! itself, so the listener is libVLC's own `VideoHelper`, made without its
//! constructor: with no player of its own it just takes note of the layout
//! and does nothing.
//!
//! Black bars in the video are looked for in a small copy of the picture
//! ([`SurfaceVideo::probe`]).

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use eframe::glow::{self, HasContext};
use jni::objects::{GlobalRef, JFloatArray, JIntArray, JObject, JValue};

use crate::platform;

const TEXTURE_EXTERNAL_OES: u32 = 0x8D65;

/// Biggest picture kept in a texture: more than a phone's window shows
/// across its short side.
const MAX_W: u32 = 1920;
const MAX_H: u32 = 1080;

/// The small copy of the picture that black bars are looked for in.
pub const PROBE_W: usize = 384;
pub const PROBE_H: usize = 216;

const VERTEX: &str = r#"#version 100
attribute vec2 pos;
uniform mat4 st;
// The part of the frame that is picture, from its top left corner (see
// `VideoLayout`).
uniform vec2 vis;
varying vec2 uv;
void main() {
    vec2 p = vec2(pos.x * vis.x, 1.0 - (1.0 - pos.y) * vis.y);
    uv = (st * vec4(p, 0.0, 1.0)).xy;
    // Top row first, the way egui's textures are.
    gl_Position = vec4(pos.x * 2.0 - 1.0, 1.0 - pos.y * 2.0, 0.0, 1.0);
}
"#;

const FRAGMENT: &str = r#"#version 100
#extension GL_OES_EGL_image_external : require
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
uniform samplerExternalOES tex;
// 0: picture, 1: HDR (PQ, BT.2020) picture to SDR, 2: subtitles (with alpha).
uniform int mode;
varying vec2 uv;

vec3 pq_to_nits(vec3 e) {
    const float m1 = 0.1593017578125;
    const float m2 = 78.84375;
    const float c1 = 0.8359375;
    const float c2 = 18.8515625;
    const float c3 = 18.6875;
    vec3 p = pow(max(e, 0.0), vec3(1.0 / m2));
    return 10000.0 * pow(max(p - c1, 0.0) / (c2 - c3 * p), vec3(1.0 / m1));
}

void main() {
    vec4 c = texture2D(tex, uv);
    if (mode == 2) {
        gl_FragColor = c;
        return;
    }
    if (mode == 1) {
        // Relative to SDR white (203 nits), then BT.2020 to BT.709 colours.
        vec3 l = pq_to_nits(c.rgb) / 203.0;
        l = vec3(
            dot(l, vec3(1.6605, -0.5876, -0.0728)),
            dot(l, vec3(-0.1246, 1.1329, -0.0083)),
            dot(l, vec3(-0.0182, -0.1006, 1.1187)));
        l = max(l, 0.0);
        // Extended Reinhard on brightness, 1000-nit highlights to white.
        float y = dot(l, vec3(0.2126, 0.7152, 0.0722));
        const float white = 1000.0 / 203.0;
        float mapped = y * (1.0 + y / (white * white)) / (1.0 + y);
        l *= mapped / max(y, 1e-5);
        c.rgb = pow(clamp(l, 0.0, 1.0), vec3(1.0 / 2.2));
    }
    gl_FragColor = vec4(c.rgb, 1.0);
}
"#;

/// What libVLC last told the layout listener about the frames it sends: when
/// it can't have the surface cut them (frames from the CPU decoder, on
/// Android 7+), they come whole, padding and all, and only the top left
/// `visible_width` x `visible_height` of a `width` x `height` frame is
/// picture. `sar` is the shape of its pixels.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct VideoLayout {
    pub width: i32,
    pub height: i32,
    pub visible_width: i32,
    pub visible_height: i32,
    pub sar: (i32, i32),
}

impl VideoLayout {
    /// The picture's share of the frame, across and down.
    fn visible(&self) -> (f32, f32) {
        (self.visible_width as f32 / self.width as f32, self.visible_height as f32 / self.height as f32)
    }
}

/// One `SurfaceTexture` libVLC draws into, and the texture its frames are
/// copied to for egui.
struct Layer {
    st: GlobalRef,
    oes: glow::Texture,
    last_stamp: i64,
    target: Option<Target>,
    id: Option<egui::TextureId>,
    /// The transform of the newest frame.
    matrix: [f32; 16],
}

type Target = (glow::Texture, glow::Framebuffer, u32, u32);

/// The `SurfaceView` the picture goes to.
struct VideoView {
    view: GlobalRef,
    /// Where it was last put (window pixels), and the window's size then.
    placed: Option<([i32; 4], (i32, i32))>,
    /// Placements until it is placed again anyway: what it is placed in can
    /// move on its own (see `place_layer`).
    recheck: u32,
    /// Frames until `AWindow` is checked for being attached again.
    check_in: u32,
    copier: Option<Copier>,
}

pub struct SurfaceVideo {
    awindow: GlobalRef,
    /// The video layout listener (see the module doc).
    listener: GlobalRef,
    video: Layer,
    view: Option<VideoView>,
    subs: Layer,
    matrix: GlobalRef,
    program: glow::Program,
    quad: glow::Buffer,
    window: (i32, i32),
    /// Where the small copy for [`Self::probe`] is drawn.
    probe: Option<Target>,
    /// How the picture was last drawn (0 or 1, see the shader).
    mode: i32,
    layout: Option<VideoLayout>,
    /// A picture of the current media has arrived.
    pub has_picture: bool,
    pub has_subtitles: bool,
}

/// What [`SurfaceVideo::frame`] made of this frame.
pub struct Shown {
    pub picture: Option<(egui::TextureId, (f32, f32))>,
    pub subtitles: Option<egui::TextureId>,
    /// The video layer lost its surface (the app was in the background) and
    /// got it back: libVLC has to start the picture again.
    pub reattached: bool,
}

impl SurfaceVideo {
    /// Sets up the surfaces and libVLC's `AWindow` around them, with the
    /// picture in a video layer if `layer`. Needs the GL context current, on
    /// the thread that will draw.
    pub fn new(gl: &glow::Context, window: (i32, i32), layer: bool) -> Option<Self> {
        unsafe {
            let program = program(gl).map_err(|e| log::warn!("video shader: {e}")).ok()?;
            let quad = gl.create_buffer().ok()?;
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(quad));
            let pts: [f32; 8] = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, as_bytes(&pts), glow::STATIC_DRAW);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            let video_oes = oes_texture(gl)?;
            let subs_oes = oes_texture(gl)?;
            let made = platform::with_env(|env, activity| {
                let new_st = |env: &mut jni::JNIEnv, tex: glow::Texture| -> jni::errors::Result<GlobalRef> {
                    let st = env.new_object("android/graphics/SurfaceTexture", "(I)V", &[JValue::Int(tex.0.get() as i32)])?;
                    env.new_global_ref(st)
                };
                let video = new_st(env, video_oes)?;
                let subs = new_st(env, subs_oes)?;
                // From the app's class loader: this thread's FindClass only sees the system's.
                let loader = env.call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?.l()?;
                let name = env.new_string("org.videolan.libvlc.AWindow")?;
                let class = env
                    .call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&name)])?
                    .l()?;
                let class = jni::objects::JClass::from(class);
                let aw = env.new_object(&class, "(Lorg/videolan/libvlc/AWindow$SurfaceCallback;)V", &[JValue::Object(&JObject::null())])?;
                let name = env.new_string("org.videolan.libvlc.VideoHelper")?;
                let helper = env
                    .call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&name)])?
                    .l()?;
                let listener = env.alloc_object(jni::objects::JClass::from(helper))?;
                let matrix = env.new_float_array(16)?;
                Ok((env.new_global_ref(aw)?, env.new_global_ref(listener)?, video, subs, env.new_global_ref(matrix)?))
            });
            let Some((awindow, listener, video, subs, matrix)) = made else {
                log::warn!("no AWindow: video goes through the CPU");
                gl.delete_program(program);
                return None;
            };
            let view = if layer { add_video_view(&awindow, &listener, &subs) } else { None };
            let attached = view.is_some()
                || platform::with_env(|env, _| {
                    let aw = awindow.as_obj();
                    env.call_method(aw, "setVideoSurface", "(Landroid/graphics/SurfaceTexture;)V", &[JValue::Object(video.as_obj())])?;
                    env.call_method(aw, "setSubtitlesSurface", "(Landroid/graphics/SurfaceTexture;)V", &[JValue::Object(subs.as_obj())])?;
                    // With two plain surfaces (no views) AWindow already calls them ready
                    // after the first, and throws on the second; it is attached by then.
                    if env.call_method(aw, "attachViews", ATTACH_VIEWS, &[JValue::Object(listener.as_obj())]).is_err() {
                        env.exception_clear()?;
                    }
                    env.call_method(aw, "areViewsAttached", "()Z", &[])?.z()
                })
                .unwrap_or(false);
            let sized = platform::with_env(|env, _| {
                env.call_method(awindow.as_obj(), "setWindowSize", "(II)V", &[JValue::Int(window.0), JValue::Int(window.1)])?;
                Ok(())
            });
            if !attached || sized.is_none() {
                log::warn!("AWindow not attached: video goes through the CPU");
                gl.delete_program(program);
                return None;
            };
            // Before its first frame a `SurfaceTexture` says 0: nothing to
            // copy yet (on some GPUs it would come out opaque black).
            let layer = |st, oes| Layer { st, oes, last_stamp: 0, target: None, id: None, matrix: [0.0; 16] };
            Some(Self {
                awindow,
                listener,
                video: layer(video, video_oes),
                view: view.map(|view| VideoView { view, placed: None, recheck: 0, check_in: 0, copier: None }),
                subs: layer(subs, subs_oes),
                matrix,
                program,
                quad,
                window,
                probe: None,
                mode: 0,
                layout: None,
                has_picture: false,
                has_subtitles: false,
            })
        }
    }

    /// Whether the picture is in the video layer under the window, not in a
    /// texture.
    pub fn is_layer(&self) -> bool {
        self.view.is_some()
    }

    /// Puts the video layer at `rect` (window pixels: the whole picture,
    /// even where it is cut off), if it isn't there already. Padding in the
    /// frames goes past it to the right and bottom, under the window.
    pub fn place_layer(&mut self, rect: [i32; 4]) {
        let (vis_w, vis_h) = self.layout.map_or((1.0, 1.0), |l| l.visible());
        let [x, y, w, h] = rect;
        let rect = [x, y, (w as f32 / vis_w).round() as i32, (h as f32 / vis_h).round() as i32];
        let window = self.window;
        let Some(v) = &mut self.view else { return };
        v.recheck = v.recheck.saturating_sub(1);
        if v.placed == Some((rect, window)) && v.recheck > 0 {
            return;
        }
        v.placed = Some((rect, window));
        v.recheck = 10;
        let view = v.view.clone();
        platform::on_main_thread(move || {
            platform::with_env(|env, activity| {
                let [x, y, w, h] = rect;
                // A floating window (the picture-in-picture one) has margins
                // around it for a shadow, which the app's drawing covers too:
                // its views start inside them.
                let win = env.call_method(activity, "getWindow", "()Landroid/view/Window;", &[])?.l()?;
                let decor = env.call_method(&win, "getDecorView", "()Landroid/view/View;", &[])?.l()?;
                let dw = env.call_method(&decor, "getWidth", "()I", &[])?.i()?;
                let dh = env.call_method(&decor, "getHeight", "()I", &[])?.i()?;
                let inset = if dw > 0 && dh > 0 { (((window.0 - dw) / 2).max(0), ((window.1 - dh) / 2).max(0)) } else { (0, 0) };
                WINDOW_INSET[0].store(inset.0, Ordering::Relaxed);
                WINDOW_INSET[1].store(inset.1, Ordering::Relaxed);
                // And the view's parent leaves the camera cutout out on some phones.
                let parent = env.call_method(view.as_obj(), "getParent", "()Landroid/view/ViewParent;", &[])?.l()?;
                let at = env.new_int_array(2)?;
                env.call_method(&parent, "getLocationInWindow", "([I)V", &[JValue::Object(&at)])?;
                let mut origin = [0i32; 2];
                env.get_int_array_region(&JIntArray::from(at), 0, &mut origin)?;
                let (left, top) = (x - inset.0 - origin[0], y - inset.1 - origin[1]);
                let params = env.call_method(view.as_obj(), "getLayoutParams", "()Landroid/view/ViewGroup$LayoutParams;", &[])?.l()?;
                let mut field = |name: &str| env.get_field(&params, name, "I").and_then(|v| v.i());
                let now = [field("leftMargin")?, field("topMargin")?, field("width")?, field("height")?];
                if now == [left, top, w, h] {
                    return Ok(());
                }
                // Gravity.TOP | Gravity.LEFT, so the margins place it.
                let params = env.new_object("android/widget/FrameLayout$LayoutParams", "(III)V", &[JValue::Int(w), JValue::Int(h), JValue::Int(0x33)])?;
                env.set_field(&params, "leftMargin", "I", JValue::Int(left))?;
                env.set_field(&params, "topMargin", "I", JValue::Int(top))?;
                env.call_method(view.as_obj(), "setLayoutParams", "(Landroid/view/ViewGroup$LayoutParams;)V", &[JValue::Object(&params)])?;
                Ok(())
            });
        });
    }

    /// The `AWindow` for `libvlc_media_player_set_android_context`.
    pub fn awindow(&self) -> *mut std::ffi::c_void {
        self.awindow.as_obj().as_raw() as *mut _
    }

    /// A new media: nothing of it is on screen yet.
    pub fn reset(&mut self) {
        self.has_picture = false;
        self.has_subtitles = false;
        self.layout = None;
    }

    /// The picture's size as it is to be shown (pixel shape applied), once
    /// libVLC has said.
    pub fn picture_size(&self) -> Option<(f32, f32)> {
        let l = self.layout?;
        let (num, den) = if l.sar.0 > 0 && l.sar.1 > 0 { l.sar } else { (1, 1) };
        Some((l.visible_width as f32 * num as f32 / den as f32, l.visible_height as f32))
    }

    /// Reads what libVLC told the layout listener (`VideoHelper` keeps it in
    /// its fields).
    fn read_layout(&self) -> Option<VideoLayout> {
        platform::with_env(|env, _| {
            let l = self.listener.as_obj();
            let mut get = |name: &str| env.get_field(l, name, "I").and_then(|v| v.i());
            Ok(VideoLayout {
                width: get("mVideoWidth")?,
                height: get("mVideoHeight")?,
                visible_width: get("mVideoVisibleWidth")?,
                visible_height: get("mVideoVisibleHeight")?,
                sar: (get("mVideoSarNum")?, get("mVideoSarDen")?),
            })
        })
        .filter(|l| l.visible_width > 0 && l.visible_height > 0 && l.width >= l.visible_width && l.height >= l.visible_height)
    }

    /// Takes the newest frames from the decoder (`video` is the picture's
    /// size from libVLC) and returns the textures to draw.
    pub fn frame(&mut self, gl: &glow::Context, frame: &mut eframe::Frame, window: (i32, i32), video: Option<(u32, u32)>, hdr: bool) -> Shown {
        if window != self.window && window.0 > 0 && window.1 > 0 {
            self.window = window;
            platform::with_env(|env, _| {
                env.call_method(self.awindow.as_obj(), "setWindowSize", "(II)V", &[JValue::Int(window.0), JValue::Int(window.1)])?;
                Ok(())
            });
        }
        if video.is_some() {
            self.layout = self.read_layout();
        }
        let size = video.map(|(w, h)| fit(w, h)).unwrap_or((MAX_W, MAX_H));
        self.mode = if hdr { 1 } else { 0 };
        let mut reattached = false;
        if let Some(v) = &mut self.view {
            // The layer's surface goes away with the window, and `AWindow`
            // lets go of the views then: give them back once it is there.
            v.check_in = v.check_in.saturating_sub(1);
            if v.check_in == 0 {
                v.check_in = 30;
                let attached = platform::with_env(|env, _| env.call_method(self.awindow.as_obj(), "areViewsAttached", "()Z", &[])?.z());
                if attached == Some(false) {
                    reattached = attach_views(&self.awindow, &self.listener, &v.view, &self.subs.st);
                }
            }
        } else if self.update(gl, frame, true, size, self.mode) {
            self.has_picture = true;
        }
        let subs_size = (self.window.0.max(1) as u32, self.window.1.max(1) as u32);
        if self.update(gl, frame, false, subs_size, 2) {
            self.has_subtitles = true;
        }
        let real = self
            .picture_size()
            .or(video.map(|(w, h)| (w as f32, h as f32)))
            .unwrap_or((size.0 as f32, size.1 as f32));
        Shown {
            picture: self.video.id.filter(|_| self.has_picture).map(|id| (id, real)),
            subtitles: self.subs.id.filter(|_| self.has_subtitles),
            reattached,
        }
    }

    /// A small copy of the newest picture ([`PROBE_W`] x [`PROBE_H`], RGBA),
    /// to look for black bars in. From the video layer it is read back by
    /// Android (on Android 14+ a moment later: the copy then is the one
    /// asked for last time).
    pub fn probe(&mut self, gl: &glow::Context) -> Option<Vec<u8>> {
        let (vis_w, vis_h) = self.layout.map_or((1.0, 1.0), |l| l.visible());
        if let Some(v) = &mut self.view {
            if v.copier.is_none() {
                v.copier = Copier::new();
            }
            // The copy is of the whole frame: keep the picture.
            let px = v.copier.as_mut()?.copy(&v.view)?;
            let (w, h) = ((PROBE_W as f32 * vis_w).round() as usize, (PROBE_H as f32 * vis_h).round() as usize);
            let mut out = vec![0u8; PROBE_W * PROBE_H * 4];
            for y in 0..PROBE_H {
                for x in 0..PROBE_W {
                    let from = ((y * h / PROBE_H) * PROBE_W + x * w / PROBE_W) * 4;
                    out[(y * PROBE_W + x) * 4..][..4].copy_from_slice(&px[from..from + 4]);
                }
            }
            return Some(out);
        }
        if !self.has_picture {
            return None;
        }
        unsafe {
            if self.probe.is_none() {
                self.probe = target(gl, (PROBE_W as u32, PROBE_H as u32));
            }
            let (_, fbo, w, h) = self.probe?;
            let (oes, m) = (self.video.oes, self.video.matrix);
            self.draw(gl, fbo, (w, h), oes, &m, (vis_w, vis_h), self.mode);
            let mut px = vec![0u8; PROBE_W * PROBE_H * 4];
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            // Bottom row first; the bars are looked for on both sides alike.
            gl.read_pixels(0, 0, w as i32, h as i32, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelPackData::Slice(Some(&mut px)));
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            Some(px)
        }
    }

    /// Latches `layer`'s newest frame and copies it into its texture.
    /// Returns whether there was a new one.
    fn update(&mut self, gl: &glow::Context, frame: &mut eframe::Frame, video: bool, size: (u32, u32), mode: i32) -> bool {
        let matrix = self.matrix.clone();
        let layer = if video { &mut self.video } else { &mut self.subs };
        let Some((stamp, m)) = platform::with_env(|env, _| {
            env.call_method(layer.st.as_obj(), "updateTexImage", "()V", &[])?;
            let stamp = env.call_method(layer.st.as_obj(), "getTimestamp", "()J", &[])?.j()?;
            let arr = <&JFloatArray>::from(matrix.as_obj());
            env.call_method(layer.st.as_obj(), "getTransformMatrix", "([F)V", &[JValue::Object(arr)])?;
            let mut m = [0f32; 16];
            env.get_float_array_region(arr, 0, &mut m)?;
            Ok((stamp, m))
        }) else {
            return false;
        };
        if stamp == layer.last_stamp {
            return false;
        }
        layer.last_stamp = stamp;
        layer.matrix = m;
        unsafe {
            if layer.target.is_some_and(|t| (t.2, t.3) != size) {
                let (tex, fbo, ..) = layer.target.take().unwrap();
                // eframe can't forget a texture id; the old one just goes unused.
                gl.delete_framebuffer(fbo);
                gl.delete_texture(tex);
                layer.id = None;
            }
            if layer.target.is_none() {
                let Some(t) = target(gl, size) else { return false };
                layer.target = Some(t);
                layer.id = Some(frame.register_native_glow_texture(t.0));
            }
            let (_, fbo, w, h) = layer.target.unwrap();
            let oes = layer.oes;
            let vis = if video { self.layout.map_or((1.0, 1.0), |l| l.visible()) } else { (1.0, 1.0) };
            self.draw(gl, fbo, (w, h), oes, &m, vis, mode);
        }
        true
    }

    /// Draws the `vis` part of the external texture `oes` over all of `fbo`.
    #[allow(clippy::too_many_arguments)]
    unsafe fn draw(
        &self,
        gl: &glow::Context,
        fbo: glow::Framebuffer,
        (w, h): (u32, u32),
        oes: glow::Texture,
        m: &[f32; 16],
        vis: (f32, f32),
        mode: i32,
    ) {
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.viewport(0, 0, w as i32, h as i32);
        gl.disable(glow::BLEND);
        gl.disable(glow::SCISSOR_TEST);
        gl.disable(glow::CULL_FACE);
        gl.disable(glow::DEPTH_TEST);
        gl.bind_vertex_array(None);
        gl.use_program(Some(self.program));
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(TEXTURE_EXTERNAL_OES, Some(oes));
        gl.uniform_1_i32(gl.get_uniform_location(self.program, "tex").as_ref(), 0);
        gl.uniform_1_i32(gl.get_uniform_location(self.program, "mode").as_ref(), mode);
        gl.uniform_matrix_4_f32_slice(gl.get_uniform_location(self.program, "st").as_ref(), false, m);
        gl.uniform_2_f32(gl.get_uniform_location(self.program, "vis").as_ref(), vis.0, vis.1);
        let pos = gl.get_attrib_location(self.program, "pos").unwrap_or(0);
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.quad));
        gl.enable_vertex_attrib_array(pos);
        gl.vertex_attrib_pointer_f32(pos, 2, glow::FLOAT, false, 8, 0);
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.disable_vertex_attrib_array(pos);
        gl.bind_buffer(glow::ARRAY_BUFFER, None);
        gl.bind_texture(TEXTURE_EXTERNAL_OES, None);
        gl.use_program(None);
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
    }
}

/// The window's shadow margins (pixels across, down), when it floats as the
/// picture-in-picture window: the app draws over them, though they are not
/// part of the window. Known once the video layer has been placed.
static WINDOW_INSET: [AtomicI32; 2] = [AtomicI32::new(0), AtomicI32::new(0)];

pub fn window_inset() -> (f32, f32) {
    (WINDOW_INSET[0].load(Ordering::Relaxed) as f32, WINDOW_INSET[1].load(Ordering::Relaxed) as f32)
}

/// Makes `rect` of the window see-through, so that the video layer under it
/// shows: egui can only paint over what is there, so this clears the window
/// itself (to transparent) when egui gets to it. Whatever is painted after
/// it (the controls) goes on top.
pub fn hole(rect: egui::Rect) -> egui::Shape {
    let clear = eframe::egui_glow::CallbackFn::new(|_, painter| unsafe {
        // Only inside the clip rect: egui has set the scissor to it.
        let gl = painter.gl();
        gl.clear_color(0.0, 0.0, 0.0, 0.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
    });
    egui::Shape::Callback(egui::PaintCallback { rect, callback: Arc::new(clear) })
}

/// Reads the video layer back through Android's `PixelCopy` into a small
/// bitmap. `PixelCopy` reports back through a listener, which takes Java
/// code to make; it is handed none, and a `Handler` whose thread never runs
/// it, since the copy itself is all that is wanted.
struct Copier {
    bitmap: GlobalRef,
    handler: GlobalRef,
    pixels: Vec<u8>,
    /// Copies in a row that Android refused; after a few it is given up.
    failures: u32,
}

impl Copier {
    fn new() -> Option<Self> {
        if platform::sdk_int() < 24 {
            return None;
        }
        // A Looper is made on a thread of its own that ends right away, so
        // nothing ever handles what is posted to it.
        let handler = std::thread::spawn(|| {
            platform::with_env(|env, _| {
                env.call_static_method("android/os/Looper", "prepare", "()V", &[])?;
                let looper = env.call_static_method("android/os/Looper", "myLooper", "()Landroid/os/Looper;", &[])?.l()?;
                let handler = env.new_object("android/os/Handler", "(Landroid/os/Looper;)V", &[JValue::Object(&looper)])?;
                env.new_global_ref(handler)
            })
        })
        .join()
        .ok()
        .flatten()?;
        let bitmap = platform::with_env(|env, _| {
            let config = env
                .get_static_field("android/graphics/Bitmap$Config", "ARGB_8888", "Landroid/graphics/Bitmap$Config;")?
                .l()?;
            let bitmap = env
                .call_static_method(
                    "android/graphics/Bitmap",
                    "createBitmap",
                    "(IILandroid/graphics/Bitmap$Config;)Landroid/graphics/Bitmap;",
                    &[JValue::Int(PROBE_W as i32), JValue::Int(PROBE_H as i32), JValue::Object(&config)],
                )?
                .l()?;
            env.new_global_ref(bitmap)
        })?;
        Some(Self { bitmap, handler, pixels: vec![0; PROBE_W * PROBE_H * 4], failures: 0 })
    }

    fn copy(&mut self, view: &GlobalRef) -> Option<Vec<u8>> {
        if self.failures >= 3 {
            return None;
        }
        let (bitmap, handler) = (&self.bitmap, &self.handler);
        let pixels = &mut self.pixels;
        let copied = platform::with_env(|env, _| {
            // What the last copy posted (and nobody will run).
            env.call_method(handler.as_obj(), "removeCallbacksAndMessages", "(Ljava/lang/Object;)V", &[JValue::Object(&JObject::null())])?;
            let holder = env.call_method(view.as_obj(), "getHolder", "()Landroid/view/SurfaceHolder;", &[])?.l()?;
            let surface = env.call_method(&holder, "getSurface", "()Landroid/view/Surface;", &[])?.l()?;
            if surface.is_null() || !env.call_method(&surface, "isValid", "()Z", &[])?.z()? {
                return Ok(false);
            }
            env.call_static_method(
                "android/view/PixelCopy",
                "request",
                "(Landroid/view/Surface;Landroid/graphics/Bitmap;Landroid/view/PixelCopy$OnPixelCopyFinishedListener;Landroid/os/Handler;)V",
                &[JValue::Object(&surface), JValue::Object(bitmap.as_obj()), JValue::Object(&JObject::null()), JValue::Object(handler.as_obj())],
            )?;
            // SAFETY: `pixels` outlives the buffer, which is only used here.
            let buf = unsafe { env.new_direct_byte_buffer(pixels.as_mut_ptr(), pixels.len()) }?;
            env.call_method(bitmap.as_obj(), "copyPixelsToBuffer", "(Ljava/nio/Buffer;)V", &[JValue::Object(&buf)])?;
            Ok(true)
        });
        self.failures = if copied.is_none() { self.failures + 1 } else { 0 };
        copied.filter(|ok| *ok).map(|_| self.pixels.clone())
    }
}

/// Makes the window see-through, puts a `SurfaceView` under it for the
/// picture and gives it and the subtitles' `subs` to `awindow`. Views belong
/// to the main thread: this waits for it.
fn add_video_view(awindow: &GlobalRef, listener: &GlobalRef, subs: &GlobalRef) -> Option<GlobalRef> {
    let (tx, rx) = std::sync::mpsc::channel();
    let (awindow, listener, subs) = (awindow.clone(), listener.clone(), subs.clone());
    platform::on_main_thread(move || {
        let view = platform::with_env(|env, activity| {
            let window = env.call_method(activity, "getWindow", "()Landroid/view/Window;", &[])?.l()?;
            // PixelFormat.TRANSLUCENT: where the window is clear, the layer shows.
            env.call_method(&window, "setFormat", "(I)V", &[JValue::Int(-3)])?;
            let view = env.new_object("android/view/SurfaceView", "(Landroid/content/Context;)V", &[JValue::Object(activity)])?;
            let params = env.new_object("android/widget/FrameLayout$LayoutParams", "(II)V", &[JValue::Int(-1), JValue::Int(-1)])?;
            // First among the content views (android.R.id.content), before
            // NativeActivity's own: the system takes a layer under the window
            // as a hole in it, unless a view that comes later covers it. The
            // window would then be skipped where the picture is, interface and
            // all.
            let content = env.call_method(activity, "findViewById", "(I)Landroid/view/View;", &[JValue::Int(0x0102_0002)])?.l()?;
            env.call_method(
                &content,
                "addView",
                "(Landroid/view/View;ILandroid/view/ViewGroup$LayoutParams;)V",
                &[JValue::Object(&view), JValue::Int(0), JValue::Object(&params)],
            )?;
            // The picture can reach past NativeActivity's view (under the
            // camera cutout, or zoomed in): an empty view over the whole
            // window covers it there too. (Nothing draws views here: the app
            // draws the window itself.)
            let cover = env.new_object("android/view/View", "(Landroid/content/Context;)V", &[JValue::Object(activity)])?;
            let params = env.new_object("android/widget/FrameLayout$LayoutParams", "(II)V", &[JValue::Int(-1), JValue::Int(-1)])?;
            let decor = env.call_method(&window, "getDecorView", "()Landroid/view/View;", &[])?.l()?;
            env.call_method(&decor, "addView", "(Landroid/view/View;Landroid/view/ViewGroup$LayoutParams;)V", &[JValue::Object(&cover), JValue::Object(&params)])?;
            env.new_global_ref(view)
        });
        let view = view.filter(|view| attach_views(&awindow, &listener, view, &subs));
        let _ = tx.send(view);
    });
    // The main thread can be waiting on this one (a window coming or going):
    // then there is no layer, and the picture goes through a texture.
    let view = rx.recv_timeout(std::time::Duration::from_secs(3)).ok().flatten();
    if view.is_none() {
        log::warn!("no video layer: the picture goes through a texture");
    }
    view
}

/// Gives `AWindow` the picture's `view` and the subtitles' `subs`.
fn attach_views(awindow: &GlobalRef, listener: &GlobalRef, view: &GlobalRef, subs: &GlobalRef) -> bool {
    platform::with_env(|env, _| {
        let aw = awindow.as_obj();
        env.call_method(aw, "setVideoView", "(Landroid/view/SurfaceView;)V", &[JValue::Object(view.as_obj())])?;
        env.call_method(aw, "setSubtitlesSurface", "(Landroid/graphics/SurfaceTexture;)V", &[JValue::Object(subs.as_obj())])?;
        // Throws like with two plain surfaces when the view's surface is
        // already there (see `SurfaceVideo::new`).
        if env.call_method(aw, "attachViews", ATTACH_VIEWS, &[JValue::Object(listener.as_obj())]).is_err() {
            env.exception_clear()?;
        }
        env.call_method(aw, "areViewsAttached", "()Z", &[])?.z()
    })
    .unwrap_or(false)
}

/// `AWindow.attachViews(OnNewVideoLayoutListener)`.
const ATTACH_VIEWS: &str = "(Lorg/videolan/libvlc/interfaces/IVLCVout$OnNewVideoLayoutListener;)V";

/// The picture's size, shrunk to fit 1080p.
fn fit(w: u32, h: u32) -> (u32, u32) {
    let scale = (MAX_W as f32 / w as f32).min(MAX_H as f32 / h as f32).min(1.0);
    (((w as f32 * scale).round() as u32).max(2), ((h as f32 * scale).round() as u32).max(2))
}

unsafe fn oes_texture(gl: &glow::Context) -> Option<glow::Texture> {
    let t = gl.create_texture().ok()?;
    gl.bind_texture(TEXTURE_EXTERNAL_OES, Some(t));
    gl.tex_parameter_i32(TEXTURE_EXTERNAL_OES, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
    gl.tex_parameter_i32(TEXTURE_EXTERNAL_OES, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
    gl.tex_parameter_i32(TEXTURE_EXTERNAL_OES, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
    gl.tex_parameter_i32(TEXTURE_EXTERNAL_OES, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
    gl.bind_texture(TEXTURE_EXTERNAL_OES, None);
    Some(t)
}

unsafe fn target(gl: &glow::Context, (w, h): (u32, u32)) -> Option<Target> {
    let tex = gl.create_texture().ok()?;
    gl.bind_texture(glow::TEXTURE_2D, Some(tex));
    gl.tex_image_2d(
        glow::TEXTURE_2D,
        0,
        glow::RGBA8 as i32,
        w as i32,
        h as i32,
        0,
        glow::RGBA,
        glow::UNSIGNED_BYTE,
        glow::PixelUnpackData::Slice(None),
    );
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
    gl.bind_texture(glow::TEXTURE_2D, None);
    let fbo = gl.create_framebuffer().ok()?;
    gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
    gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), 0);
    let ok = gl.check_framebuffer_status(glow::FRAMEBUFFER) == glow::FRAMEBUFFER_COMPLETE;
    gl.bind_framebuffer(glow::FRAMEBUFFER, None);
    if !ok {
        gl.delete_framebuffer(fbo);
        gl.delete_texture(tex);
        return None;
    }
    Some((tex, fbo, w, h))
}

unsafe fn program(gl: &glow::Context) -> Result<glow::Program, String> {
    let program = gl.create_program()?;
    for (kind, src) in [(glow::VERTEX_SHADER, VERTEX), (glow::FRAGMENT_SHADER, FRAGMENT)] {
        let sh = gl.create_shader(kind)?;
        gl.shader_source(sh, src);
        gl.compile_shader(sh);
        if !gl.get_shader_compile_status(sh) {
            return Err(gl.get_shader_info_log(sh));
        }
        gl.attach_shader(program, sh);
        gl.delete_shader(sh);
    }
    gl.link_program(program);
    if !gl.get_program_link_status(program) {
        return Err(gl.get_program_info_log(program));
    }
    Ok(program)
}

fn as_bytes(v: &[f32]) -> &[u8] {
    // SAFETY: plain floats, read as bytes for the upload.
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}
