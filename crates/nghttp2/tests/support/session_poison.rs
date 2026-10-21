//! Native allocation-failure scenarios for session and callback-context operations.
use super::*;
use crate as nghttp2;
use crate::{nv, stream::SessionStreams, submit::SessionSubmit};

fn request() -> [HeaderField<'static>; 4] {
    [
        nv!(":method", "POST"),
        nv!(":scheme", "https"),
        nv!(":authority", "example.com"),
        nv!(":path", "/"),
    ]
}

#[test]
fn integration_context_submits_control_frames_and_borrows_stream_views() -> Result<()> {
    let mut client = ClientSession::new_default(None, None)?;
    // SAFETY: the owner keeps native state live and is not driven during the context's use.
    let ctx = unsafe { SessionCtx::from_raw(client.raw()) }.expect("live context");
    SessionSubmit::submit_settings(&ctx, &[]).expect("settings through context");
    SessionSubmit::submit_ping(&ctx, FrameFlags::NONE, Some(b"ctx-ping"))
        .expect("ping through context");
    assert_eq!(
        SessionStreams::root_stream(&ctx).expect("root stream").id(),
        0
    );
    assert!(ctx.find_stream(99).is_none());
    let mut wire = Vec::new();
    loop {
        let bytes = client.mem_send()?;
        if bytes.is_empty() {
            break;
        }
        wire.extend_from_slice(bytes);
    }
    assert!(wire.windows(8).any(|bytes| bytes == b"ctx-ping"));
    Ok(())
}

#[test]
fn integration_scenario_native_resume_oom_poisons_owner_and_both_context_interfaces() -> Result<()>
{
    for interface in 0..3 {
        let allocator = CustomAllocator::new();
        let mem = ptr::from_ref(allocator.as_raw()).cast_mut();
        // SAFETY: the allocator outlives the session, including native destruction.
        let mut client =
            unsafe { Session::client_with_raw_mem(EventCollector::new(), None, Some(mem)) }?;
        let mut streams = Vec::new();
        for _ in 0..5 {
            streams.push(client.submit_request(&request(), Some(DataProvider::deferred()))?);
            while !client.mem_send()?.is_empty() {}
        }
        // Native DATA scheduling starts with four queue slots. Deferred streams
        // are removed from that queue; resuming the fifth requires it to grow.
        for &id in &streams[..4] {
            client.resume_data(id)?;
        }
        assert!(allocator.set_limit(0));
        let result = match interface {
            0 => client.resume_data(streams[4]),
            _ => {
                // SAFETY: client owns the live state; no native drive overlaps this context.
                let mut ctx = unsafe { SessionCtx::from_raw(client.raw()) }.expect("live context");
                if interface == 1 {
                    SessionCtx::resume_data(&mut ctx, streams[4])
                } else {
                    SessionSubmit::resume_data(&ctx, streams[4])
                }
            }
        };
        let failure = Error::from_native(ffi::NGHTTP2_ERR_NOMEM);
        assert_eq!(result, Err(failure));
        assert!(allocator.set_limit(usize::MAX));
        assert_eq!(client.resume_data(streams[0]), Err(failure));
        assert_eq!(client.submit_ping(false, None), Err(failure));
        // SAFETY: client remains live; rejected operations do not dispatch callbacks.
        let mut ctx = unsafe { SessionCtx::from_raw(client.raw()) }.expect("live context");
        assert_eq!(SessionCtx::resume_data(&mut ctx, streams[0]), Err(failure));
        assert_eq!(SessionSubmit::resume_data(&ctx, streams[0]), Err(failure));
        assert_eq!(
            SessionSubmit::submit_ping(&ctx, FrameFlags::NONE, None),
            Err(failure)
        );
        assert_eq!(client.mem_send(), Err(failure));
        assert_eq!(client.mem_recv(&[]), Err(failure));
        assert_eq!(client.send(), Err(failure));
        assert_eq!(client.recv(), Err(failure));
        drop(client);
        assert_eq!(allocator.stats().expect("tracked state").live_blocks, 0);
    }
    Ok(())
}

struct FailingContext {
    allocator: Rc<CustomAllocator>,
    failure: Option<Error>,
}
impl SessionHandler for FailingContext {
    fn on_frame_recv(
        &mut self,
        ctx: &mut SessionCtx<'_>,
        _: &crate::callbacks::Frame,
    ) -> crate::callbacks::CallbackResult {
        if self.failure.is_some() {
            return Ok(());
        }
        assert!(self.allocator.set_limit(0));
        let failure = SessionSubmit::submit_ping(ctx, FrameFlags::NONE, None)
            .expect_err("native allocation failure");
        assert_eq!(failure, Error::from_native(ffi::NGHTTP2_ERR_NOMEM));
        assert!(self.allocator.set_limit(usize::MAX));
        assert_eq!(
            SessionSubmit::submit_ping(ctx, FrameFlags::NONE, None),
            Err(failure)
        );
        assert_eq!(ctx.submit_ping(false, None), Err(failure));
        assert_eq!(SessionSubmit::resume_data(ctx, 99), Err(failure));
        assert_eq!(ctx.resume_data(99), Err(failure));
        self.failure = Some(failure);
        Ok(())
    }
}
#[test]
fn integration_scenario_ufcs_fatal_submission_is_latched_before_callback_returns() -> Result<()> {
    let allocator = Rc::new(CustomAllocator::new());
    let mem = ptr::from_ref(allocator.as_raw()).cast_mut();
    // SAFETY: both handler and this scope retain the external allocator through native deletion.
    let mut server = unsafe {
        Session::server_with_raw_mem(
            FailingContext {
                allocator: allocator.clone(),
                failure: None,
            },
            None,
            Some(mem),
        )
    }?;
    let mut client = ClientSession::new_default(None, None)?;
    client.submit_settings(&[])?;
    let mut saw_error = false;
    for _ in 0..4 {
        let wire = client.mem_send()?.to_vec();
        if wire.is_empty() {
            break;
        }
        if let Err(error) = server.mem_recv(&wire) {
            assert_eq!(Some(error), server.handler().failure);
            saw_error = true;
            break;
        }
    }
    assert!(
        saw_error,
        "outer driver must report the callback's latched failure"
    );
    let failure = server.handler().failure.expect("callback ran");
    assert_eq!(server.mem_send(), Err(failure));
    assert_eq!(server.mem_recv(&[]), Err(failure));
    assert_eq!(server.send(), Err(failure));
    assert_eq!(server.recv(), Err(failure));
    drop(server);
    assert_eq!(allocator.stats().expect("tracked state").live_blocks, 0);
    Ok(())
}
