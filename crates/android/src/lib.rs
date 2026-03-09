//! Android JNI bindings for pc-agent-loop-core.
//!
//! # Kotlin Usage
//! ```kotlin
//! // KOTLIN STUB:
//! //
//! // package com.pcagentloop
//! //
//! // import kotlinx.coroutines.Dispatchers
//! // import kotlinx.coroutines.withContext
//! //
//! // class AgentSession(configJson: String, workDir: String) {
//! //     private val ptr: Long
//! //
//! //     init {
//! //         ptr = nativeCreate(configJson, workDir)
//! //         if (ptr == 0L) throw RuntimeException("Failed to create AgentSession")
//! //     }
//! //
//! //     suspend fun runTask(task: String, maxTurns: Int = 15): String =
//! //         withContext(Dispatchers.IO) {
//! //             nativeRunTask(ptr, task, maxTurns)
//! //         }
//! //
//! //     fun destroy() = nativeDestroy(ptr)
//! //
//! //     private external fun nativeCreate(configJson: String, workDir: String): Long
//! //     private external fun nativeRunTask(ptr: Long, task: String, maxTurns: Int): String
//! //     private external fun nativeDestroy(ptr: Long)
//! //
//! //     companion object {
//! //         init { System.loadLibrary("pc_agent_loop_android") }
//! //     }
//! // }
//! ```

use jni::objects::{JClass, JObject, JString};
use jni::sys::{jlong, jstring};
use jni::JNIEnv;
use pc_agent_loop_core::AgentSession;

/// JNI: com.pcagentloop.AgentSession.nativeCreate
///
/// Creates a new AgentSession on the heap, returns pointer as jlong.
/// Returns 0 on error.
#[no_mangle]
pub extern "system" fn Java_com_pcagentloop_AgentSession_nativeCreate(
    mut env: JNIEnv,
    _class: JClass,
    config_json: JString,
    work_dir: JString,
) -> jlong {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> anyhow::Result<jlong> {
        let config_str: String = env.get_string(&config_json)?.into();
        let work_dir_str: String = env.get_string(&work_dir)?.into();

        let session = AgentSession::new(&config_str, &work_dir_str)?;
        let boxed = Box::new(session);
        Ok(Box::into_raw(boxed) as jlong)
    }));

    match result {
        Ok(Ok(ptr)) => ptr,
        Ok(Err(e)) => {
            let _ = env.throw_new("java/lang/RuntimeException", e.to_string());
            0
        }
        Err(_) => {
            let _ = env.throw_new("java/lang/RuntimeException", "Panic in nativeCreate");
            0
        }
    }
}

/// JNI: com.pcagentloop.AgentSession.nativeRunTask
///
/// Runs a task synchronously (blocks the calling thread).
/// Returns result as a JSON string.
#[no_mangle]
pub extern "system" fn Java_com_pcagentloop_AgentSession_nativeRunTask(
    mut env: JNIEnv,
    _obj: JObject,
    ptr: jlong,
    task: JString,
    max_turns: jni::sys::jint,
) -> jstring {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> anyhow::Result<jstring> {
        if ptr == 0 {
            return Err(anyhow::anyhow!("Null AgentSession pointer"));
        }

        let task_str: String = env.get_string(&task)?.into();
        let max_turns_usize = max_turns.max(1) as usize;

        // SAFETY: ptr was created by nativeCreate using Box::into_raw
        let session = unsafe { &mut *(ptr as *mut AgentSession) };

        // Block on the async operation using a new tokio runtime
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let json_result = rt.block_on(session.run_task(
            &task_str,
            max_turns_usize,
            |_chunk| {
                // Streaming chunks are discarded in the sync JNI binding.
                // For streaming support, use a separate callback mechanism.
            },
        ))?;

        let jstr = env.new_string(&json_result)?;
        Ok(jstr.into_raw())
    }));

    match result {
        Ok(Ok(jstr)) => jstr,
        Ok(Err(e)) => {
            let _ = env.throw_new("java/lang/RuntimeException", e.to_string());
            std::ptr::null_mut()
        }
        Err(_) => {
            let _ = env.throw_new("java/lang/RuntimeException", "Panic in nativeRunTask");
            std::ptr::null_mut()
        }
    }
}

/// JNI: com.pcagentloop.AgentSession.nativeDestroy
///
/// Frees the AgentSession. Must be called exactly once when done.
#[no_mangle]
pub extern "system" fn Java_com_pcagentloop_AgentSession_nativeDestroy(
    _env: JNIEnv,
    _obj: JObject,
    ptr: jlong,
) {
    if ptr != 0 {
        // SAFETY: ptr was created by nativeCreate using Box::into_raw and is no longer used after this call
        let _ = std::panic::catch_unwind(|| {
            unsafe { drop(Box::from_raw(ptr as *mut AgentSession)) };
        });
    }
}
