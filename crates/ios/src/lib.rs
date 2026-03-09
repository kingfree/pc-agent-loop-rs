//! iOS C FFI bindings for pc-agent-loop-core.
//!
//! # C Header (pc_agent_loop_ios.h)
//! ```c
//! // Opaque handle to AgentSession
//! typedef struct AgentSessionHandle AgentSessionHandle;
//!
//! // Create a new agent session. Returns NULL on error.
//! // Caller owns the returned pointer; free with agent_session_destroy.
//! AgentSessionHandle* agent_session_create(const char* config_json, const char* work_dir);
//!
//! // Run a task. Returns a JSON string; caller must free with agent_string_free.
//! char* agent_session_run_task(AgentSessionHandle* handle, const char* task, int max_turns);
//!
//! // Free a string returned by this library.
//! void agent_string_free(char* s);
//!
//! // Destroy an agent session.
//! void agent_session_destroy(AgentSessionHandle* handle);
//! ```
//!
//! # Swift Usage
//! ```swift
//! import Foundation
//!
//! enum AgentError: Error {
//!     case initFailed
//!     case taskFailed(String)
//! }
//!
//! class AgentSession {
//!     private var ptr: OpaquePointer?
//!
//!     init(configJson: String, workDir: String) throws {
//!         ptr = agent_session_create(configJson, workDir)
//!         if ptr == nil { throw AgentError.initFailed }
//!     }
//!
//!     func runTask(_ task: String, maxTurns: Int32 = 15) throws -> String {
//!         guard let result = agent_session_run_task(ptr, task, maxTurns) else {
//!             throw AgentError.taskFailed("null result")
//!         }
//!         defer { agent_string_free(result) }
//!         return String(cString: result)
//!     }
//!
//!     deinit { agent_session_destroy(ptr) }
//! }
//!
//! // Usage:
//! // let session = try AgentSession(
//! //     configJson: #"{"oai_config":{"apikey":"sk-...","apibase":"https://api.openai.com","model":"gpt-4o"}}"#,
//! //     workDir: FileManager.default.temporaryDirectory.path
//! // )
//! // let result = try session.runTask("List the files in the working directory")
//! // print(result)
//! ```

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use pc_agent_loop_core::AgentSession;

/// Opaque handle to AgentSession, exposed through C FFI.
pub struct AgentSessionHandle(AgentSession);

/// Create a new agent session.
///
/// # Safety
/// - `config_json` and `work_dir` must be valid null-terminated UTF-8 C strings.
/// - Returns null on error (invalid JSON config, missing fields, etc.).
/// - The returned pointer must eventually be passed to `agent_session_destroy`.
#[no_mangle]
pub extern "C" fn agent_session_create(
    config_json: *const c_char,
    work_dir: *const c_char,
) -> *mut AgentSessionHandle {
    let result = std::panic::catch_unwind(|| -> anyhow::Result<*mut AgentSessionHandle> {
        // SAFETY: caller guarantees valid C strings
        let config_str = unsafe { CStr::from_ptr(config_json) }.to_str()
            .map_err(|e| anyhow::anyhow!("Invalid UTF-8 in config_json: {}", e))?;
        let work_dir_str = unsafe { CStr::from_ptr(work_dir) }.to_str()
            .map_err(|e| anyhow::anyhow!("Invalid UTF-8 in work_dir: {}", e))?;

        let session = AgentSession::new(config_str, work_dir_str)?;
        let handle = Box::new(AgentSessionHandle(session));
        Ok(Box::into_raw(handle))
    });

    match result {
        Ok(Ok(ptr)) => ptr,
        _ => std::ptr::null_mut(),
    }
}

/// Run a task synchronously.
///
/// # Safety
/// - `handle` must be a valid pointer returned by `agent_session_create`.
/// - `task` must be a valid null-terminated UTF-8 C string.
/// - The returned string must be freed with `agent_string_free`.
/// - Returns null on error.
#[no_mangle]
pub extern "C" fn agent_session_run_task(
    handle: *mut AgentSessionHandle,
    task: *const c_char,
    max_turns: i32,
) -> *mut c_char {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> anyhow::Result<*mut c_char> {
        if handle.is_null() {
            return Err(anyhow::anyhow!("Null handle"));
        }

        // SAFETY: handle was created by agent_session_create using Box::into_raw
        let session = unsafe { &mut (*handle).0 };

        let task_str = unsafe { CStr::from_ptr(task) }.to_str()
            .map_err(|e| anyhow::anyhow!("Invalid UTF-8 in task: {}", e))?;

        let max_turns_usize = max_turns.max(1) as usize;

        // Build a single-threaded tokio runtime for blocking on the async function
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let json_result = rt.block_on(session.run_task(
            task_str,
            max_turns_usize,
            |_chunk| {
                // Streaming not exposed through sync C FFI.
                // For streaming support, use a callback-based variant.
            },
        ))?;

        let cstring = CString::new(json_result)?;
        Ok(cstring.into_raw())
    }));

    match result {
        Ok(Ok(ptr)) => ptr,
        _ => std::ptr::null_mut(),
    }
}

/// Free a string returned by this library.
///
/// # Safety
/// - `s` must be a pointer returned by `agent_session_run_task` and must not be used after this call.
/// - Passing null is safe and does nothing.
#[no_mangle]
pub extern "C" fn agent_string_free(s: *mut c_char) {
    if !s.is_null() {
        // SAFETY: s was created by CString::into_raw in agent_session_run_task
        unsafe { drop(CString::from_raw(s)) };
    }
}

/// Destroy an agent session.
///
/// # Safety
/// - `handle` must be a valid pointer returned by `agent_session_create` and must not be used after this call.
/// - Passing null is safe and does nothing.
#[no_mangle]
pub extern "C" fn agent_session_destroy(handle: *mut AgentSessionHandle) {
    if !handle.is_null() {
        let _ = std::panic::catch_unwind(|| {
            // SAFETY: handle was created by agent_session_create using Box::into_raw
            unsafe { drop(Box::from_raw(handle)) };
        });
    }
}
