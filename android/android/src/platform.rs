//! The few things the player needs from Android itself, reached through JNI
//! from Rust: storage permission, screen brightness and orientation,
//! picture-in-picture (with its pause/play button), the clipboard, the intent the app was opened with,
//! Wi-Fi multicast for network discovery, and starting libVLC's Java side. There is no Java
//! code in the app.

use std::ffi::{c_void, CString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use winit::platform::android::activity::AndroidApp;
use jni::objects::{GlobalRef, JObject, JString, JValue};
use jni::{JNIEnv, JavaVM};

static APP: OnceLock<AndroidApp> = OnceLock::new();

const SCREEN_ORIENTATION_UNSPECIFIED: i32 = -1;
const SCREEN_ORIENTATION_SENSOR_LANDSCAPE: i32 = 6;

pub fn init(app: &AndroidApp) {
    let _ = APP.set(app.clone());
    if let Some(dir) = app.internal_data_path() {
        setup_fontconfig(&dir);
        let _ = crate::store::ANDROID_DATA_DIR.set(dir);
    }
    start_libvlc_jni();
    use_cutout_area();
}

/// libVLC's subtitle renderer (freetype, and libass for ASS) finds fonts
/// through fontconfig, which has no configuration on Android. Point it at the
/// system fonts, with its cache in the app's folder.
fn setup_fontconfig(data_dir: &Path) {
    let conf = data_dir.join("fonts.conf");
    let cache = data_dir.join("fontconfig-cache");
    let text = format!(
        r#"<?xml version="1.0"?>
<!DOCTYPE fontconfig SYSTEM "fonts.dtd">
<fontconfig>
  <dir>/system/fonts</dir>
  <dir>/product/fonts</dir>
  <cachedir>{}</cachedir>
  <alias><family>sans-serif</family><prefer><family>Roboto</family><family>Noto Sans</family></prefer></alias>
  <alias><family>serif</family><prefer><family>Noto Serif</family></prefer></alias>
  <alias><family>Arial</family><prefer><family>Roboto</family></prefer></alias>
</fontconfig>
"#,
        cache.display()
    );
    if std::fs::read_to_string(&conf).ok().as_deref() != Some(text.as_str()) {
        let _ = std::fs::create_dir_all(data_dir);
        let _ = std::fs::write(&conf, text);
    }
    // Set before libVLC starts, which is what reads them.
    std::env::set_var("FONTCONFIG_FILE", &conf);
    std::env::set_var("FONTCONFIG_PATH", data_dir);

    // The first scan of the system fonts takes a few seconds; do it now in
    // the background (libVLC exports fontconfig) rather than when the first
    // subtitle appears.
    std::thread::Builder::new()
        .name("font-cache".into())
        .spawn(|| unsafe {
            if let Some(f) = vlc_symbol("FcInit") {
                let fc_init: unsafe extern "C" fn() -> i32 = std::mem::transmute(f);
                fc_init();
            }
        })
        .ok();
}

/// A symbol exported by libvlc.so.
fn vlc_symbol(name: &str) -> Option<*mut c_void> {
    let lib_name = CString::new("libvlc.so").unwrap();
    let sym = CString::new(name).ok()?;
    unsafe {
        let lib = libc::dlopen(lib_name.as_ptr(), libc::RTLD_NOW);
        if lib.is_null() {
            log::error!("libvlc.so is not loaded");
            return None;
        }
        let f = libc::dlsym(lib, sym.as_ptr());
        (!f.is_null()).then_some(f)
    }
}

fn app() -> &'static AndroidApp {
    APP.get().expect("platform::init is called first")
}

fn vm() -> JavaVM {
    // SAFETY: the pointer is the process's JavaVM, valid for its lifetime.
    unsafe { JavaVM::from_raw(app().vm_as_ptr() as *mut jni::sys::JavaVM) }.expect("valid JavaVM")
}

/// Runs `f` with a JNI env for this thread and the activity object. Java
/// exceptions are logged and cleared, and turn into `None`. Local references
/// are freed on return (the thread stays attached, so nothing else would).
fn with_env<R>(f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<R>) -> Option<R> {
    let vm = vm();
    let mut env = vm.attach_current_thread_permanently().ok()?;
    // SAFETY: a global reference owned by the NativeActivity glue.
    let activity = unsafe { JObject::from_raw(app().activity_as_ptr() as jni::sys::jobject) };
    let result = env.with_local_frame(32, |env| f(env, &activity));
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    match result {
        Ok(r) => Some(r),
        Err(e) => {
            log::warn!("JNI call failed: {e}");
            None
        }
    }
}

/// libVLC's audio (AudioTrack, AAudio) and hardware decoding (MediaCodec)
/// modules talk to Java. They find the JavaVM through `JNI_OnLoad`, which
/// Java would call on `System.loadLibrary("vlc")`; here the library is linked
/// directly, so call it ourselves.
fn start_libvlc_jni() {
    type OnLoad = unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32;
    // The current thread must be attached for JNI_OnLoad's GetEnv.
    if with_env(|_, _| Ok(())).is_none() {
        return;
    }
    let Some(f) = vlc_symbol("JNI_OnLoad") else {
        log::warn!("libvlc.so has no JNI_OnLoad");
        return;
    };
    unsafe {
        let on_load: OnLoad = std::mem::transmute(f);
        let version = on_load(app().vm_as_ptr(), std::ptr::null_mut());
        log::info!("libVLC JNI_OnLoad -> {version:#x}");
    }
}

pub fn sdk_int() -> i32 {
    static SDK: OnceLock<i32> = OnceLock::new();
    *SDK.get_or_init(|| {
        with_env(|env, _| env.get_static_field("android/os/Build$VERSION", "SDK_INT", "I")?.i()).unwrap_or(0)
    })
}

/// Whether the app may read the shared storage (every file on Android 11+).
pub fn has_storage_access() -> bool {
    if sdk_int() >= 30 {
        with_env(|env, _| env.call_static_method("android/os/Environment", "isExternalStorageManager", "()Z", &[])?.z())
            .unwrap_or(false)
    } else {
        with_env(|env, activity| {
            let perm = env.new_string("android.permission.READ_EXTERNAL_STORAGE")?;
            let r = env.call_method(activity, "checkSelfPermission", "(Ljava/lang/String;)I", &[JValue::Object(&perm)])?;
            Ok(r.i()? == 0)
        })
        .unwrap_or(false)
    }
}

/// Asks for storage access: the "All files access" settings page on
/// Android 11+, the permission dialog on older versions.
pub fn request_storage_access() {
    if sdk_int() >= 30 {
        let opened = with_env(|env, activity| {
            let pkg = env.call_method(activity, "getPackageName", "()Ljava/lang/String;", &[])?.l()?;
            let pkg = env.get_string(&JString::from(pkg))?.to_string_lossy().into_owned();
            let uri_str = env.new_string(format!("package:{pkg}"))?;
            let uri = env
                .call_static_method(
                    "android/net/Uri",
                    "parse",
                    "(Ljava/lang/String;)Landroid/net/Uri;",
                    &[JValue::Object(&uri_str)],
                )?
                .l()?;
            let action = env.new_string("android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION")?;
            let intent = env.new_object(
                "android/content/Intent",
                "(Ljava/lang/String;Landroid/net/Uri;)V",
                &[JValue::Object(&action), JValue::Object(&uri)],
            )?;
            env.call_method(activity, "startActivity", "(Landroid/content/Intent;)V", &[JValue::Object(&intent)])?;
            Ok(())
        });
        if opened.is_none() {
            // Some firmwares only have the general list of apps.
            with_env(|env, activity| {
                let action = env.new_string("android.settings.MANAGE_ALL_FILES_ACCESS_PERMISSION")?;
                let intent = env.new_object("android/content/Intent", "(Ljava/lang/String;)V", &[JValue::Object(&action)])?;
                env.call_method(activity, "startActivity", "(Landroid/content/Intent;)V", &[JValue::Object(&intent)])?;
                Ok(())
            });
        }
    } else {
        with_env(|env, activity| {
            let perms = env.new_object_array(2, "java/lang/String", JObject::null())?;
            for (i, p) in ["android.permission.READ_EXTERNAL_STORAGE", "android.permission.WRITE_EXTERNAL_STORAGE"]
                .iter()
                .enumerate()
            {
                let s = env.new_string(p)?;
                env.set_object_array_element(&perms, i as i32, s)?;
            }
            env.call_method(activity, "requestPermissions", "([Ljava/lang/String;I)V", &[JValue::Object(&perms), JValue::Int(1)])?;
            Ok(())
        });
    }
}

/// Keeps the screen on while video plays. A screen wake lock is used rather
/// than the window flag: the NativeActivity glue's window-flag call blocks
/// when made from inside a frame.
pub fn set_keep_screen_on(on: bool) {
    static LOCK: std::sync::Mutex<Option<GlobalRef>> = std::sync::Mutex::new(None);
    const SCREEN_BRIGHT_WAKE_LOCK: i32 = 0x0000_000a;
    const ON_AFTER_RELEASE: i32 = 0x2000_0000;
    let mut slot = LOCK.lock().unwrap();
    if slot.is_none() {
        *slot = with_env(|env, activity| {
            let name = env.new_string("power")?;
            let pm = env
                .call_method(activity, "getSystemService", "(Ljava/lang/String;)Ljava/lang/Object;", &[JValue::Object(&name)])?
                .l()?;
            let tag = env.new_string("vplayer:playback")?;
            let lock = env
                .call_method(
                    &pm,
                    "newWakeLock",
                    "(ILjava/lang/String;)Landroid/os/PowerManager$WakeLock;",
                    &[JValue::Int(SCREEN_BRIGHT_WAKE_LOCK | ON_AFTER_RELEASE), JValue::Object(&tag)],
                )?
                .l()?;
            env.call_method(&lock, "setReferenceCounted", "(Z)V", &[JValue::Bool(0)])?;
            env.new_global_ref(lock)
        });
    }
    let Some(lock) = slot.as_ref() else { return };
    with_env(|env, _| {
        env.call_method(lock.as_obj(), if on { "acquire" } else { "release" }, "()V", &[])?;
        Ok(())
    });
}

/// The window's brightness from 0 to 1: what a swipe set, or else the
/// system's brightness setting.
pub fn brightness() -> f32 {
    if BRIGHTNESS_SET.load(Ordering::SeqCst) {
        return f32::from_bits(BRIGHTNESS.load(Ordering::SeqCst));
    }
    with_env(|env, activity| {
        let resolver = env.call_method(activity, "getContentResolver", "()Landroid/content/ContentResolver;", &[])?.l()?;
        let name = env.new_string("screen_brightness")?;
        let v = env
            .call_static_method(
                "android/provider/Settings$System",
                "getInt",
                "(Landroid/content/ContentResolver;Ljava/lang/String;I)I",
                &[JValue::Object(&resolver), JValue::Object(&name), JValue::Int(128)],
            )?
            .i()?;
        Ok((v as f32 / 255.0).clamp(0.0, 1.0))
    })
    .unwrap_or(0.5)
}

static BRIGHTNESS: AtomicU32 = AtomicU32::new(0);
static BRIGHTNESS_SET: AtomicBool = AtomicBool::new(false);
static BRIGHTNESS_QUEUED: AtomicBool = AtomicBool::new(false);

/// Sets this window's brightness (the system setting is left alone, so it
/// only lasts while the player is on screen). Window attributes can only be
/// changed on Android's UI thread; while one change waits there, newer
/// values just replace it.
pub fn set_brightness(v: f32) {
    BRIGHTNESS.store(v.clamp(0.01, 1.0).to_bits(), Ordering::SeqCst);
    BRIGHTNESS_SET.store(true, Ordering::SeqCst);
    if BRIGHTNESS_QUEUED.swap(true, Ordering::SeqCst) {
        return;
    }
    app().run_on_java_main_thread(Box::new(|| {
        BRIGHTNESS_QUEUED.store(false, Ordering::SeqCst);
        let v = f32::from_bits(BRIGHTNESS.load(Ordering::SeqCst));
        with_env(|env, activity| {
            let window = env.call_method(activity, "getWindow", "()Landroid/view/Window;", &[])?.l()?;
            let attrs = env.call_method(&window, "getAttributes", "()Landroid/view/WindowManager$LayoutParams;", &[])?.l()?;
            env.set_field(&attrs, "screenBrightness", "F", JValue::Float(v))?;
            env.call_method(&window, "setAttributes", "(Landroid/view/WindowManager$LayoutParams;)V", &[JValue::Object(&attrs)])?;
            Ok(())
        });
    }));
}

/// Lets the window cover the camera cutout too (Android 9+), so the video
/// reaches the screen's edge around the camera instead of stopping short of
/// it with a black strip. The app keeps its controls out of the cutout, see
/// [`cutout`].
fn use_cutout_area() {
    if sdk_int() < 28 {
        return;
    }
    app().run_on_java_main_thread(Box::new(|| {
        with_env(|env, activity| {
            let window = env.call_method(activity, "getWindow", "()Landroid/view/Window;", &[])?.l()?;
            let attrs = env.call_method(&window, "getAttributes", "()Landroid/view/WindowManager$LayoutParams;", &[])?.l()?;
            // LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS (Android 11+), else SHORT_EDGES.
            let mode = if sdk_int() >= 30 { 3 } else { 1 };
            env.set_field(&attrs, "layoutInDisplayCutoutMode", "I", JValue::Int(mode))?;
            env.call_method(&window, "setAttributes", "(Landroid/view/WindowManager$LayoutParams;)V", &[JValue::Object(&attrs)])?;
            Ok(())
        });
    }));
}

/// How far the camera cutout reaches into the window from each edge, in
/// pixels: left, top, right, bottom. Kept up to date by [`refresh_cutout`].
static CUTOUT: [AtomicU32; 4] = [const { AtomicU32::new(0) }; 4];
static CUTOUT_QUEUED: AtomicBool = AtomicBool::new(false);

pub fn cutout() -> [f32; 4] {
    CUTOUT.each_ref().map(|v| v.load(Ordering::Relaxed) as f32)
}

/// Re-reads the cutout (it moves when the screen turns). The window's insets
/// are read on Android's UI thread.
pub fn refresh_cutout() {
    if sdk_int() < 28 || CUTOUT_QUEUED.swap(true, Ordering::SeqCst) {
        return;
    }
    app().run_on_java_main_thread(Box::new(|| {
        CUTOUT_QUEUED.store(false, Ordering::SeqCst);
        let insets = with_env(|env, activity| {
            let window = env.call_method(activity, "getWindow", "()Landroid/view/Window;", &[])?.l()?;
            let decor = env.call_method(&window, "getDecorView", "()Landroid/view/View;", &[])?.l()?;
            let insets = env.call_method(&decor, "getRootWindowInsets", "()Landroid/view/WindowInsets;", &[])?.l()?;
            if insets.is_null() {
                return Ok([0; 4]);
            }
            let cutout = env.call_method(&insets, "getDisplayCutout", "()Landroid/view/DisplayCutout;", &[])?.l()?;
            if cutout.is_null() {
                return Ok([0; 4]);
            }
            let mut out = [0; 4];
            for (v, name) in out.iter_mut().zip(["getSafeInsetLeft", "getSafeInsetTop", "getSafeInsetRight", "getSafeInsetBottom"]) {
                *v = env.call_method(&cutout, name, "()I", &[])?.i()?.max(0) as u32;
            }
            Ok(out)
        });
        if let Some(insets) = insets {
            for (slot, v) in CUTOUT.iter().zip(insets) {
                slot.store(v, Ordering::Relaxed);
            }
        }
    }));
}

/// Holds Wi-Fi multicast on while the network is being browsed: UPnP and
/// mDNS discovery rely on it, and many phones filter it out otherwise.
pub fn set_multicast(on: bool) {
    static LOCK: std::sync::Mutex<Option<GlobalRef>> = std::sync::Mutex::new(None);
    let mut slot = LOCK.lock().unwrap();
    if slot.is_none() {
        *slot = with_env(|env, activity| {
            let ctx = env.call_method(activity, "getApplicationContext", "()Landroid/content/Context;", &[])?.l()?;
            let name = env.new_string("wifi")?;
            let wm = env
                .call_method(&ctx, "getSystemService", "(Ljava/lang/String;)Ljava/lang/Object;", &[JValue::Object(&name)])?
                .l()?;
            let tag = env.new_string("vplayer:discovery")?;
            let lock = env
                .call_method(
                    &wm,
                    "createMulticastLock",
                    "(Ljava/lang/String;)Landroid/net/wifi/WifiManager$MulticastLock;",
                    &[JValue::Object(&tag)],
                )?
                .l()?;
            env.call_method(&lock, "setReferenceCounted", "(Z)V", &[JValue::Bool(0)])?;
            env.new_global_ref(lock)
        });
    }
    let Some(lock) = slot.as_ref() else { return };
    with_env(|env, _| {
        env.call_method(lock.as_obj(), if on { "acquire" } else { "release" }, "()V", &[])?;
        Ok(())
    });
}

/// For landscape video, turns the screen sideways while in fullscreen.
/// (The status bar is always hidden by the app's theme.)
pub fn set_fullscreen(on: bool, landscape_video: bool) {
    let orientation =
        if on && landscape_video { SCREEN_ORIENTATION_SENSOR_LANDSCAPE } else { SCREEN_ORIENTATION_UNSPECIFIED };
    with_env(|env, activity| {
        env.call_method(activity, "setRequestedOrientation", "(I)V", &[JValue::Int(orientation)])?;
        Ok(())
    });
}

/// Picture-in-picture: the player as a small window over other apps.
/// Android 8+ (and the user can turn it off for the app in settings).
pub fn pip_supported() -> bool {
    sdk_int() >= 26
}

fn pip_params<'a>(
    env: &mut JNIEnv<'a>,
    aspect: Option<(i32, i32)>,
    auto_enter: bool,
    playing: bool,
) -> jni::errors::Result<JObject<'a>> {
    const BUILDER: &str = "Landroid/app/PictureInPictureParams$Builder;";
    let builder = env.new_object("android/app/PictureInPictureParams$Builder", "()V", &[])?;
    if let Some((w, h)) = aspect {
        let ratio = env.new_object("android/util/Rational", "(II)V", &[JValue::Int(w), JValue::Int(h)])?;
        env.call_method(&builder, "setAspectRatio", format!("(Landroid/util/Rational;){BUILDER}"), &[JValue::Object(&ratio)])?;
    }
    if sdk_int() >= 31 {
        env.call_method(&builder, "setAutoEnterEnabled", format!("(Z){BUILDER}"), &[JValue::Bool(auto_enter as u8)])?;
    }
    if let Some(action) = pip_play_pause_action(env, playing)? {
        let list = env.new_object("java/util/ArrayList", "()V", &[])?;
        env.call_method(&list, "add", "(Ljava/lang/Object;)Z", &[JValue::Object(&action)])?;
        env.call_method(&builder, "setActions", format!("(Ljava/util/List;){BUILDER}"), &[JValue::Object(&list)])?;
    }
    env.call_method(&builder, "build", "()Landroid/app/PictureInPictureParams;", &[])?.l()
}

/// Broadcast sent by the pause/play button on the small window.
const PIP_ACTION: &str = "org.vplayer.app.PIP_PLAY_PAUSE";

/// What receives [`PIP_ACTION`], and the intent the button sends.
struct PipButton {
    receiver: GlobalRef,
    intent: GlobalRef,
}

static PIP_BUTTON: std::sync::Mutex<Option<PipButton>> = std::sync::Mutex::new(None);

/// The small window's pause/play button. Android delivers its press as a
/// broadcast, which normally takes a Java `BroadcastReceiver` subclass. With
/// no Java here, a stock framework receiver (`AppWidgetProvider`, which
/// ignores unknown broadcasts) is registered instead: Android attaches each
/// broadcast it delivers to the receiver as its "pending result", and
/// [`pip_button_pressed`] notices a new one appearing.
fn pip_play_pause_action<'a>(env: &mut JNIEnv<'a>, playing: bool) -> jni::errors::Result<Option<JObject<'a>>> {
    let mut slot = PIP_BUTTON.lock().unwrap();
    if slot.is_none() {
        let activity = unsafe { JObject::from_raw(app().activity_as_ptr() as jni::sys::jobject) };
        let ctx = env.call_method(&activity, "getApplicationContext", "()Landroid/content/Context;", &[])?.l()?;
        let receiver = env.new_object("android/appwidget/AppWidgetProvider", "()V", &[])?;
        let action = env.new_string(PIP_ACTION)?;
        let filter = env.new_object("android/content/IntentFilter", "(Ljava/lang/String;)V", &[JValue::Object(&action)])?;
        // RECEIVER_NOT_EXPORTED (Android 13+): only this app (the button's
        // intent is sent on its behalf) may trigger it.
        let flags = if sdk_int() >= 33 { 4 } else { 0 };
        env.call_method(
            &ctx,
            "registerReceiver",
            "(Landroid/content/BroadcastReceiver;Landroid/content/IntentFilter;I)Landroid/content/Intent;",
            &[JValue::Object(&receiver), JValue::Object(&filter), JValue::Int(flags)],
        )?;
        let intent = env.new_object("android/content/Intent", "(Ljava/lang/String;)V", &[JValue::Object(&action)])?;
        let package = env.call_method(&ctx, "getPackageName", "()Ljava/lang/String;", &[])?.l()?;
        env.call_method(&intent, "setPackage", "(Ljava/lang/String;)Landroid/content/Intent;", &[JValue::Object(&package)])?;
        const FLAG_IMMUTABLE: i32 = 1 << 26;
        const FLAG_UPDATE_CURRENT: i32 = 1 << 27;
        let pending = env
            .call_static_method(
                "android/app/PendingIntent",
                "getBroadcast",
                "(Landroid/content/Context;ILandroid/content/Intent;I)Landroid/app/PendingIntent;",
                &[JValue::Object(&ctx), JValue::Int(0), JValue::Object(&intent), JValue::Int(FLAG_IMMUTABLE | FLAG_UPDATE_CURRENT)],
            )?
            .l()?;
        *slot = Some(PipButton { receiver: env.new_global_ref(receiver)?, intent: env.new_global_ref(pending)? });
    }
    let Some(button) = slot.as_ref() else { return Ok(None) };
    let (drawable, label) = if playing { ("ic_media_pause", "Пауза") } else { ("ic_media_play", "Воспроизвести") };
    let res = env.get_static_field("android/R$drawable", drawable, "I")?.i()?;
    let system = env.new_string("android")?;
    let icon = env
        .call_static_method(
            "android/graphics/drawable/Icon",
            "createWithResource",
            "(Ljava/lang/String;I)Landroid/graphics/drawable/Icon;",
            &[JValue::Object(&system), JValue::Int(res)],
        )?
        .l()?;
    let label = env.new_string(label)?;
    let action = env.new_object(
        "android/app/RemoteAction",
        "(Landroid/graphics/drawable/Icon;Ljava/lang/CharSequence;Ljava/lang/CharSequence;Landroid/app/PendingIntent;)V",
        &[JValue::Object(&icon), JValue::Object(&label), JValue::Object(&label), JValue::Object(button.intent.as_obj())],
    )?;
    Ok(Some(action))
}

/// Whether the small window's pause/play button was pressed since the last call.
pub fn pip_button_pressed() -> bool {
    let slot = PIP_BUTTON.lock().unwrap();
    let Some(button) = slot.as_ref() else { return false };
    with_env(|env, _| {
        // Null until a broadcast arrives; each broadcast brings a fresh, empty
        // bundle, which gets marked here once seen.
        let extras = env
            .call_method(button.receiver.as_obj(), "getResultExtras", "(Z)Landroid/os/Bundle;", &[JValue::Bool(1)])?
            .l()?;
        if extras.is_null() {
            return Ok(false);
        }
        let key = env.new_string("vplayer.seen")?;
        let seen = env.call_method(&extras, "containsKey", "(Ljava/lang/String;)Z", &[JValue::Object(&key)])?.z()?;
        if !seen {
            env.call_method(&extras, "putBoolean", "(Ljava/lang/String;Z)V", &[JValue::Object(&key), JValue::Bool(1)])?;
        }
        Ok(!seen)
    })
    .unwrap_or(false)
}

/// While a video plays, lets Android 12+ shrink the player into a small
/// window by itself when the user goes Home (older versions only do it on
/// "Back", see [`enter_pip`]). `aspect` is the video's shape as w:h;
/// `playing` picks the window's button (pause or play).
pub fn set_auto_pip(aspect: Option<(i32, i32)>, on: bool, playing: bool) {
    if !pip_supported() {
        return;
    }
    app().run_on_java_main_thread(Box::new(move || {
        with_env(|env, activity| {
            let params = pip_params(env, aspect, on, playing)?;
            env.call_method(activity, "setPictureInPictureParams", "(Landroid/app/PictureInPictureParams;)V", &[JValue::Object(&params)])?;
            Ok(())
        });
    }));
}

/// Shrinks the player into a small window now, or, where that is off or
/// unsupported, sends it to the background like Home.
pub fn enter_pip(aspect: Option<(i32, i32)>, playing: bool) {
    if !pip_supported() {
        move_to_back();
        return;
    }
    app().run_on_java_main_thread(Box::new(move || {
        let entered = with_env(|env, activity| {
            let params = pip_params(env, aspect, false, playing)?;
            env.call_method(activity, "enterPictureInPictureMode", "(Landroid/app/PictureInPictureParams;)Z", &[JValue::Object(&params)])?.z()
        });
        if entered != Some(true) {
            move_to_back();
        }
    }));
}

pub fn in_pip() -> bool {
    pip_supported()
        && with_env(|env, activity| env.call_method(activity, "isInPictureInPictureMode", "()Z", &[])?.z()).unwrap_or(false)
}

/// "Back" on the main screen: send the app to the background like Home does.
/// (Finishing the activity would end the process's only event loop.)
pub fn move_to_back() {
    with_env(|env, activity| {
        env.call_method(activity, "moveTaskToBack", "(Z)Z", &[JValue::Bool(1)])?;
        Ok(())
    });
}

pub fn clipboard_text() -> Option<String> {
    with_env(|env, activity| {
        let name = env.new_string("clipboard")?;
        let cm = env
            .call_method(activity, "getSystemService", "(Ljava/lang/String;)Ljava/lang/Object;", &[JValue::Object(&name)])?
            .l()?;
        if cm.is_null() {
            return Ok(None);
        }
        let clip = env.call_method(&cm, "getPrimaryClip", "()Landroid/content/ClipData;", &[])?.l()?;
        if clip.is_null() {
            return Ok(None);
        }
        let item = env.call_method(&clip, "getItemAt", "(I)Landroid/content/ClipData$Item;", &[JValue::Int(0)])?.l()?;
        let text = env
            .call_method(&item, "coerceToText", "(Landroid/content/Context;)Ljava/lang/CharSequence;", &[JValue::Object(activity)])?
            .l()?;
        if text.is_null() {
            return Ok(None);
        }
        let s = env.call_method(&text, "toString", "()Ljava/lang/String;", &[])?.l()?;
        Ok(Some(env.get_string(&JString::from(s))?.to_string_lossy().into_owned()))
    })
    .flatten()
}

/// What the app was opened with ("Open with VPlayer" in a file manager or a
/// browser): a local path when the file is reachable by path, otherwise an
/// `fd://` or network location libVLC can play directly.
pub fn intent_location() -> Option<String> {
    with_env(|env, activity| {
        let intent = env.call_method(activity, "getIntent", "()Landroid/content/Intent;", &[])?.l()?;
        if intent.is_null() {
            return Ok(None);
        }
        let uri = env.call_method(&intent, "getData", "()Landroid/net/Uri;", &[])?.l()?;
        if uri.is_null() {
            return Ok(None);
        }
        let jstr = |env: &mut JNIEnv, o: JObject| -> jni::errors::Result<Option<String>> {
            if o.is_null() {
                return Ok(None);
            }
            Ok(Some(env.get_string(&JString::from(o))?.to_string_lossy().into_owned()))
        };
        let scheme = env.call_method(&uri, "getScheme", "()Ljava/lang/String;", &[])?.l()?;
        let scheme = jstr(env, scheme)?.unwrap_or_default();
        match scheme.as_str() {
            "file" => {
                let p = env.call_method(&uri, "getPath", "()Ljava/lang/String;", &[])?.l()?;
                jstr(env, p)
            }
            "content" => {
                let resolver =
                    env.call_method(activity, "getContentResolver", "()Landroid/content/ContentResolver;", &[])?.l()?;
                let mode = env.new_string("r")?;
                let pfd = env
                    .call_method(
                        &resolver,
                        "openFileDescriptor",
                        "(Landroid/net/Uri;Ljava/lang/String;)Landroid/os/ParcelFileDescriptor;",
                        &[JValue::Object(&uri), JValue::Object(&mode)],
                    )?
                    .l()?;
                if pfd.is_null() {
                    return Ok(None);
                }
                let fd = env.call_method(&pfd, "detachFd", "()I", &[])?.i()?;
                Ok(Some(fd_location(fd)))
            }
            _ => {
                let s = env.call_method(&uri, "toString", "()Ljava/lang/String;", &[])?.l()?;
                jstr(env, s)
            }
        }
    })
    .flatten()
}

/// A plain path when the descriptor points at a file we can open by name
/// (keeps resume positions and the folder's subtitles working), else `fd://N`.
fn fd_location(fd: i32) -> String {
    if let Ok(target) = std::fs::read_link(format!("/proc/self/fd/{fd}")) {
        if target.is_absolute() && std::fs::File::open(&target).is_ok() {
            unsafe { libc::close(fd) };
            return target.to_string_lossy().into_owned();
        }
    }
    format!("fd://{fd}")
}

/// Places to start browsing from: internal storage, then SD cards and USB drives.
pub fn storage_roots() -> Vec<(String, PathBuf)> {
    let mut roots = vec![("Память телефона".to_owned(), PathBuf::from("/storage/emulated/0"))];
    if let Ok(rd) = std::fs::read_dir("/storage") {
        let mut extra: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                n != "emulated" && n != "self" && p.is_dir()
            })
            .collect();
        extra.sort();
        for p in extra {
            let n = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
            roots.push((format!("Карта {n}"), p));
        }
    }
    roots
}

pub fn default_dir() -> PathBuf {
    let home = PathBuf::from("/storage/emulated/0");
    for sub in ["Movies", "Download"] {
        if has_media(&home.join(sub)) {
            return home.join(sub);
        }
    }
    home
}

fn has_media(dir: &Path) -> bool {
    std::fs::read_dir(dir).map(|rd| rd.flatten().next().is_some()).unwrap_or(false)
}

/// Whether the app is on screen (in front, or as a picture-in-picture window),
/// by the importance Android gives its process. A hidden app, or one behind
/// a switched-off screen, ranks lower.
fn app_visible() -> bool {
    const IMPORTANCE_VISIBLE: i32 = 200;
    with_env(|env, _| {
        let info = env.new_object("android/app/ActivityManager$RunningAppProcessInfo", "()V", &[])?;
        env.call_static_method(
            "android/app/ActivityManager",
            "getMyMemoryState",
            "(Landroid/app/ActivityManager$RunningAppProcessInfo;)V",
            &[JValue::Object(&info)],
        )?;
        Ok(env.get_field(&info, "importance", "I")?.i()? <= IMPORTANCE_VISIBLE)
    })
    .unwrap_or(false)
}

/// Pauses video when the app goes to the background. While the app is
/// hidden Android takes the window away and the UI stops running, so frames
/// that stop coming, and Android confirming the app is not on screen, mean
/// "hidden". Audio-only media keeps playing, like in music players.
pub struct BackgroundGuard {
    shared: Arc<GuardState>,
}

struct GuardState {
    start: Instant,
    last_frame_ms: AtomicU64,
    has_video: AtomicBool,
    paused: AtomicBool,
}

impl GuardState {
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

impl BackgroundGuard {
    pub fn new(pause: impl Fn() + Send + 'static) -> Self {
        let shared = Arc::new(GuardState {
            start: Instant::now(),
            last_frame_ms: AtomicU64::new(0),
            has_video: AtomicBool::new(false),
            paused: AtomicBool::new(false),
        });
        let s = shared.clone();
        std::thread::Builder::new()
            .name("background-guard".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_millis(400));
                let idle = s.now_ms().saturating_sub(s.last_frame_ms.load(Ordering::SeqCst));
                // Frames also stall for a moment while a network video opens, so
                // ask Android before pausing.
                if idle > 1500 && s.has_video.load(Ordering::SeqCst) && !s.paused.load(Ordering::SeqCst) && !app_visible() {
                    s.paused.store(true, Ordering::SeqCst);
                    log::info!("app hidden, pausing video");
                    pause();
                }
            })
            .ok();
        Self { shared }
    }

    /// Call once per UI frame.
    pub fn tick(&self, video_playing: bool) {
        self.shared.last_frame_ms.store(self.shared.now_ms(), Ordering::SeqCst);
        self.shared.has_video.store(video_playing, Ordering::SeqCst);
        self.shared.paused.store(false, Ordering::SeqCst);
    }
}
