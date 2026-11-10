//! Calls into the Java helper `FileBridge` over JNI.
//!
//! Every call takes the application context and plain strings and answers with a string:
//! `ok:<payload>` or `err:<message>`. Java never throws across this boundary and never calls back
//! into Rust, so there are no native methods to register; Rust polls for results instead.
//!
//! This is the only module that uses `unsafe`: creating a [`JavaVM`] and wrapping the activity from
//! the raw pointers Android provides. Both come from [`AndroidApp`], which guarantees they are
//! valid for the life of the process.

#![allow(unsafe_code)]

use android_activity::AndroidApp;
use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
use jni::{JNIEnv, JavaVM};

const HELPER: &str = "ai.storyteller.photocraft.FileBridge";

pub struct Java {
    vm: JavaVM,
    /// The application context, which outlives any one activity.
    context: GlobalRef,
    /// The helper class, loaded through the app's class loader: a native thread's own `FindClass`
    /// only sees the system classes.
    helper: GlobalRef,
}

impl Java {
    pub fn new(app: &AndroidApp) -> Result<Self, String> {
        // SAFETY: `vm_as_ptr` is the process's `JavaVM*`, valid for the life of the process.
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }.map_err(|e| format!("no Java VM: {e}"))?;
        let mut env = vm.attach_current_thread().map_err(|e| format!("couldn't attach to the Java VM: {e}"))?;
        // SAFETY: `activity_as_ptr` is a valid `jobject` for the activity while `app` is alive; the
        // wrapper doesn't free it, and the global references below own what we keep.
        let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
        let context = env
            .call_method(&activity, "getApplicationContext", "()Landroid/content/Context;", &[])
            .and_then(|v| v.l())
            .map_err(|e| format!("no application context: {e}"))?;
        let loader = env
            .call_method(&activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
            .and_then(|v| v.l())
            .map_err(|e| format!("no class loader: {e}"))?;
        let name = env.new_string(HELPER).map_err(|e| e.to_string())?;
        let class = env
            .call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&name)])
            .and_then(|v| v.l())
            .map_err(|e| format!("couldn't load {HELPER}: {e}"))?;
        let context = env.new_global_ref(&context).map_err(|e| e.to_string())?;
        let helper = env.new_global_ref(&class).map_err(|e| e.to_string())?;
        drop(env);
        Ok(Self { vm, context, helper })
    }

    /// Calls `FileBridge.<name>(Context, String...)`; `ok:<payload>` gives `Ok(payload)`.
    pub fn call(&self, name: &str, args: &[&str]) -> Result<String, String> {
        let mut env = self.vm.attach_current_thread().map_err(|e| format!("couldn't attach to the Java VM: {e}"))?;
        let result = self.call_in(&mut env, name, args);
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_describe();
            let _ = env.exception_clear();
            return Err(format!("{name} raised a Java exception"));
        }
        result
    }

    fn call_in(&self, env: &mut JNIEnv<'_>, name: &str, args: &[&str]) -> Result<String, String> {
        let class = JClass::from(env.new_local_ref(self.helper.as_obj()).map_err(|e| e.to_string())?);
        let jargs: Vec<JString<'_>> = args.iter().map(|a| env.new_string(a)).collect::<Result<_, _>>().map_err(|e| e.to_string())?;
        let mut values: Vec<JValue<'_, '_>> = vec![JValue::Object(self.context.as_obj())];
        values.extend(jargs.iter().map(|s| JValue::Object(s)));
        let signature = format!("(Landroid/content/Context;{})Ljava/lang/String;", "Ljava/lang/String;".repeat(args.len()));
        let answer = env.call_static_method(&class, name, &signature, &values).and_then(|v| v.l()).map_err(|e| format!("{name}: {e}"))?;
        if answer.is_null() {
            return Err(format!("{name} gave no answer"));
        }
        let text: String = env.get_string(&JString::from(answer)).map_err(|e| e.to_string())?.into();
        match text.split_once(':') {
            Some(("ok", payload)) => Ok(payload.to_string()),
            Some(("err", message)) => Err(message.to_string()),
            _ => Err(format!("{name} answered something unexpected")),
        }
    }
}
