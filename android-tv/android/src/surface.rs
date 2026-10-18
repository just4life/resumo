//! Video straight from the hardware decoder, never through the CPU, which a
//! 4K (or 10-bit HDR) video can't afford on a TV. libVLC draws through its
//! `AWindow` (from libVLC's own Java part, shipped in the APK) into
//!
//! * the picture: by default a `SurfaceView` under the app's window, as in
//!   VLC: the decoder's frames go to the screen as they are, HDR included,
//!   and the TV switches to HDR by itself. The window is see-through where
//!   the picture is (see [`SurfaceVideo::is_layer`]). Otherwise a
//!   `SurfaceTexture` whose frames are drawn into an ordinary texture that
//!   egui shows like any image, with HDR (PQ) turned into SDR on the way,
//!   since the app's own window is SDR;
//! * the subtitles: always a `SurfaceTexture`, drawn by egui over the picture.
//!
//! libVLC draws straight into these (its "android_display" output, which
//! passes the hardware decoder's frames on untouched, HDR metadata and all)
//! only when someone is there to place the picture: `AWindow` needs a video
//! layout listener for it, and otherwise falls back to its OpenGL output,
//! which turns HDR into a washed-out SDR picture and is black on some TVs.
//! The app places the picture itself, so the listener is libVLC's own
//! `VideoHelper`, made without its constructor: with no player of its own
//! it just takes note of the layout and does nothing.

use eframe::glow::{self, HasContext};
use jni::objects::{GlobalRef, JFloatArray, JObject, JValue};

use crate::platform;

const TEXTURE_EXTERNAL_OES: u32 = 0x8D65;

/// Biggest picture kept: the TV's window is drawn at no more than 1080p.
const MAX_W: u32 = 1920;
const MAX_H: u32 = 1080;

const VERTEX: &str = r#"#version 100
attribute vec2 pos;
uniform mat4 st;
varying vec2 uv;
void main() {
    uv = (st * vec4(pos, 0.0, 1.0)).xy;
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

/// One `SurfaceTexture` libVLC draws into, and the texture its frames are
/// copied to for egui.
struct Layer {
    st: GlobalRef,
    oes: glow::Texture,
    last_stamp: i64,
    target: Option<(glow::Texture, glow::Framebuffer, u32, u32)>,
    id: Option<egui::TextureId>,
}

/// The `SurfaceView` the picture goes to.
struct VideoView {
    view: GlobalRef,
    /// Where it was last put, in window pixels.
    rect: Option<[i32; 4]>,
    /// Frames until `AWindow` is checked for being attached again.
    check_in: u32,
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
            let layer = |st, oes| Layer { st, oes, last_stamp: i64::MIN, target: None, id: None };
            Some(Self {
                awindow,
                listener,
                video: layer(video, video_oes),
                view: view.map(|view| VideoView { view, rect: None, check_in: 0 }),
                subs: layer(subs, subs_oes),
                matrix,
                program,
                quad,
                window,
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
    /// even where it is cut off), if it isn't there already.
    pub fn place_layer(&mut self, rect: [i32; 4]) {
        let Some(v) = &mut self.view else { return };
        if v.rect == Some(rect) {
            return;
        }
        v.rect = Some(rect);
        let view = v.view.clone();
        platform::on_main_thread(move || {
            platform::with_env(|env, _| {
                let [x, y, w, h] = rect;
                // Gravity.TOP | Gravity.LEFT, so the margins place it.
                let params = env.new_object("android/widget/FrameLayout$LayoutParams", "(III)V", &[JValue::Int(w), JValue::Int(h), JValue::Int(0x33)])?;
                env.set_field(&params, "leftMargin", "I", JValue::Int(x))?;
                env.set_field(&params, "topMargin", "I", JValue::Int(y))?;
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
        let size = video.map(|(w, h)| fit(w, h)).unwrap_or((MAX_W, MAX_H));
        let mode = if hdr { 1 } else { 0 };
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
        } else if self.update(gl, frame, true, size, mode) {
            self.has_picture = true;
        }
        let subs_size = (self.window.0.max(1) as u32, self.window.1.max(1) as u32);
        if self.update(gl, frame, false, subs_size, 2) {
            self.has_subtitles = true;
        }
        let real = video.map(|(w, h)| (w as f32, h as f32)).unwrap_or((size.0 as f32, size.1 as f32));
        Shown {
            picture: self.video.id.filter(|_| self.has_picture).map(|id| (id, real)),
            subtitles: self.subs.id.filter(|_| self.has_subtitles),
            reattached,
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
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.viewport(0, 0, w as i32, h as i32);
            gl.disable(glow::BLEND);
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::CULL_FACE);
            gl.disable(glow::DEPTH_TEST);
            gl.bind_vertex_array(None);
            gl.use_program(Some(self.program));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(TEXTURE_EXTERNAL_OES, Some(layer.oes));
            gl.uniform_1_i32(gl.get_uniform_location(self.program, "tex").as_ref(), 0);
            gl.uniform_1_i32(gl.get_uniform_location(self.program, "mode").as_ref(), mode);
            gl.uniform_matrix_4_f32_slice(gl.get_uniform_location(self.program, "st").as_ref(), false, &m);
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
        true
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
            // PixelFormat.TRANSLUCENT: where egui leaves the window clear, the layer shows.
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

unsafe fn target(gl: &glow::Context, (w, h): (u32, u32)) -> Option<(glow::Texture, glow::Framebuffer, u32, u32)> {
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
