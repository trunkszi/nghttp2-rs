//! Error context preserves native codes and typed causes at application boundaries.
use anyhow::{Context, Result, ensure};
use nghttp2::{
    Error, ServerSession,
    callbacks::{CallbackError, CallbackResult},
    error::NgHttp2Error,
    hpack::{HpDeflater, HpackError},
    nv::{HeaderField, InvalidHeader},
    sys,
};

fn request_on_server() -> Result<()> {
    let mut server = ServerSession::new_default(None, None).context("create server session")?;
    server
        .submit_request(&[], None)
        .context("submit client request on server")?;
    Ok(())
}

#[test]
fn integration_scenario_context_keeps_native_failure_available_for_typed_handling() -> Result<()> {
    let error = request_on_server().expect_err("server cannot submit a client request");
    ensure!(error.to_string() == "submit client request on server");
    let typed = error
        .downcast_ref::<Error>()
        .context("missing session error")?;
    ensure!(*typed == Error::from_native(sys::NGHTTP2_ERR_PROTO));
    ensure!(!typed.is_fatal());
    let native = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<NgHttp2Error>())
        .context("missing native source in error chain")?;
    ensure!(native.raw() == sys::NGHTTP2_ERR_PROTO);
    ensure!(format!("{error:#}").contains(&typed.to_string()));
    Ok(())
}

#[test]
fn unit_scenario_context_preserves_header_hpack_and_callback_error_types() -> Result<()> {
    let field = HeaderField::new(b"Invalid-Name", b"value");
    let error = field
        .validate()
        .context("validate request header")
        .expect_err("uppercase name");
    ensure!(error.downcast_ref::<InvalidHeader>() == Some(&InvalidHeader::Name));
    ensure!(format!("{error:#}") == "validate request header: invalid header name");
    let error = HpDeflater::new(usize::MAX)
        .context("create bounded HPACK encoder")
        .err()
        .context("oversized table unexpectedly succeeded")?;
    ensure!(
        error.downcast_ref::<HpackError>() == Some(&HpackError(sys::NGHTTP2_ERR_INVALID_ARGUMENT))
    );
    ensure!(format!("{error:#}") == "create bounded HPACK encoder: HPACK error -501");
    let callback: CallbackResult = Err(CallbackError::Failure);
    let error = callback
        .context("handle received frame")
        .expect_err("callback failure");
    ensure!(error.downcast_ref::<CallbackError>() == Some(&CallbackError::Failure));
    ensure!(format!("{error:#}") == "handle received frame: native callback processing failed");
    Ok(())
}
