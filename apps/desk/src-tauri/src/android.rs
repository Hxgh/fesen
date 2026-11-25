use jni::{
    JNIEnv,
    objects::{JObject, JString},
    sys::{jboolean, jlong, jstring},
};
use fesen_core::platform::android::Engine;

#[unsafe(no_mangle)]
extern "system" fn Java_io_fesen_bridge_FesenTile_nativeCanResume(
    mut env: JNIEnv<'_>,
    _object: JObject<'_>,
    path: JString<'_>,
) -> jboolean {
    let Ok(path) = env.get_string(&path) else {
        return 0;
    };
    let path: String = path.into();
    Engine::can_resume(path.as_ref()).into()
}

#[unsafe(no_mangle)]
extern "system" fn Java_io_fesen_bridge_FesenVpn_nativeStart(
    env: JNIEnv<'_>,
    object: JObject<'_>,
    path: JString<'_>,
) -> jlong {
    Engine::start(env, object, path).map_or(0, |engine| Box::into_raw(engine) as jlong)
}

#[unsafe(no_mangle)]
extern "system" fn Java_io_fesen_bridge_FesenVpn_nativeRequest(
    mut env: JNIEnv<'_>,
    _object: JObject<'_>,
    engine: jlong,
    body: JString<'_>,
) -> jstring {
    let Some(engine) = (unsafe { (engine as *const Engine).as_ref() }) else {
        return std::ptr::null_mut();
    };
    let Ok(data) = env.get_string(&body) else {
        return std::ptr::null_mut();
    };
    let body: String = data.into();
    let reply = engine.request(&body);
    env.new_string(reply)
        .map_or(std::ptr::null_mut(), |s| s.into_raw())
}

#[unsafe(no_mangle)]
extern "system" fn Java_io_fesen_bridge_FesenVpn_nativeHalt(
    _env: JNIEnv<'_>,
    _object: JObject<'_>,
    engine: jlong,
) {
    if let Some(engine) = unsafe { (engine as *const Engine).as_ref() } {
        engine.halt();
    }
}

#[unsafe(no_mangle)]
extern "system" fn Java_io_fesen_bridge_FesenVpn_nativeRunning(
    _env: JNIEnv<'_>,
    _object: JObject<'_>,
    engine: jlong,
) -> jboolean {
    unsafe { (engine as *const Engine).as_ref() }
        .is_some_and(Engine::running)
        .into()
}

#[unsafe(no_mangle)]
extern "system" fn Java_io_fesen_bridge_FesenVpn_nativeDrop(
    _env: JNIEnv<'_>,
    _object: JObject<'_>,
    engine: jlong,
) {
    if engine != 0 {
        unsafe { Box::from_raw(engine as *mut Engine) }.shutdown();
    }
}
