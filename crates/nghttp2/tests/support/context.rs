use super::*;

#[test]
fn null_pointer_yields_no_context() {
    let raw: *mut nghttp2_session = core::ptr::null_mut();
    // SAFETY: `from_raw` explicitly accepts a null pointer and reports it as
    // `None`.
    let ctx = unsafe { SessionCtx::from_raw(raw) };
    assert!(ctx.is_none());
}

#[test]
fn integration_native_handle_round_trips_live_session() {
    use nghttp2_sys::{
        nghttp2_session_callbacks_del, nghttp2_session_callbacks_new, nghttp2_session_client_new,
        nghttp2_session_del,
    };
    let mut callbacks = core::ptr::null_mut();
    let mut raw = core::ptr::null_mut();
    // SAFETY: both output slots are writable; the callback table is initialized
    // before constructing a session, and both returned owners are released below.
    unsafe {
        assert_eq!(nghttp2_session_callbacks_new(&mut callbacks), 0);
        assert_eq!(
            nghttp2_session_client_new(&mut raw, callbacks, core::ptr::null_mut()),
            0
        );
    }
    {
        // SAFETY: the native allocation remains live throughout this scope.
        let ctx = unsafe { SessionCtx::from_raw(raw) }.expect("live session");
        // SAFETY: the native allocation remains live throughout this scope.
        let handle = unsafe { SessionPtr::from_raw(NonNull::new(raw).expect("live session")) };
        assert_eq!(handle.as_ptr(), raw);
        assert_eq!(ctx.as_ptr(), raw);
        assert_eq!(format!("{ctx:?}"), format!("SessionCtx({raw:p})"));
    }
    // SAFETY: scoped handles have expired; both pointers are uniquely owned.
    unsafe {
        nghttp2_session_del(raw);
        nghttp2_session_callbacks_del(callbacks);
    }
}
