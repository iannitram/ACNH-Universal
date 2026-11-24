//! Android `InputDevice` queries over JNI (`cfg(target_os = "android")`).

use std::ffi::c_void;

use jni::objects::{JIntArray, JObject, JString, JValue};
use jni::{jni_sig, jni_str, JavaVM};

use super::AndroidDevice;

/// Query all currently connected Android `InputDevice`s via JNI.
/// Returns an empty list if the Android context is unavailable or a JNI call fails.
pub fn connected_devices() -> Vec<AndroidDevice> {
    let ctx = ndk_context::android_context();
    let vm_ptr = ctx.vm() as *mut jni::sys::JavaVM;
    if vm_ptr.is_null() {
        return Vec::new();
    }
    let vm = unsafe { JavaVM::from_raw(vm_ptr) };
    vm.attach_current_thread(|env| -> jni::errors::Result<Vec<AndroidDevice>> {
        let cls = env.find_class(jni_str!("android/view/InputDevice"))?;
        let ids_obj = env
            .call_static_method(&cls, jni_str!("getDeviceIds"), jni_sig!("()[I"), &[])?
            .l()?;
        if ids_obj.is_null() {
            return Ok(Vec::new());
        }
        let ids_arr = env.cast_local::<JIntArray>(ids_obj)?;
        let len = ids_arr.len(env)?;
        let mut ids = vec![0i32; len];
        ids_arr.get_region(env, 0, &mut ids)?;

        let mut devices = Vec::with_capacity(len);
        for id in ids {
            let dev = env
                .call_static_method(
                    &cls,
                    jni_str!("getDevice"),
                    jni_sig!("(I)Landroid/view/InputDevice;"),
                    &[JValue::Int(id)],
                )?
                .l()?;
            if dev.is_null() {
                continue;
            }
            if let Ok(desc) = read_device(env, &dev, id) {
                devices.push(desc);
            }
        }
        Ok(devices)
    })
    .unwrap_or_default()
}

fn read_device(env: &mut jni::Env, dev: &JObject, id: i32) -> jni::errors::Result<AndroidDevice> {
    let sources = env
        .call_method(dev, jni_str!("getSources"), jni_sig!("()I"), &[])?
        .i()?;
    let is_virtual = env
        .call_method(dev, jni_str!("isVirtual"), jni_sig!("()Z"), &[])?
        .z()?;
    let vendor_raw = env
        .call_method(dev, jni_str!("getVendorId"), jni_sig!("()I"), &[])?
        .i()?;
    let product_raw = env
        .call_method(dev, jni_str!("getProductId"), jni_sig!("()I"), &[])?
        .i()?;
    let name_obj = env
        .call_method(
            dev,
            jni_str!("getName"),
            jni_sig!("()Ljava/lang/String;"),
            &[],
        )?
        .l()?;
    let name = if name_obj.is_null() {
        String::new()
    } else {
        env.cast_local::<JString>(name_obj)?.try_to_string(env)?
    };

    let vendor = u16::try_from(vendor_raw).ok().filter(|&v| v != 0);
    let product = u16::try_from(product_raw).ok();

    Ok(AndroidDevice {
        id,
        vendor,
        product,
        name,
        sources,
        is_virtual,
    })
}

/// Keep `c_void` imported only if needed by raw pointer casts; unused otherwise.
const _: fn(*mut c_void) = |_| {};
