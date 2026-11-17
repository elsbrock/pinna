use std::{error::Error, io};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetError {
    Timeout,
    Unreachable,
    Tls,
    Protocol,
}

pub fn classify(e: &reqwest::Error) -> NetError {
    if e.is_timeout() {
        return NetError::Timeout;
    }
    let mut source = e.source();
    while let Some(err) = source {
        if err.is::<rustls::Error>() {
            return NetError::Tls;
        }
        // io::Error's source skips the wrapped error and returns its source.
        source = match err.downcast_ref::<io::Error>().and_then(io::Error::get_ref) {
            Some(inner) => Some(inner),
            None => err.source(),
        };
    }
    if e.is_connect() {
        NetError::Unreachable
    } else {
        NetError::Protocol
    }
}
