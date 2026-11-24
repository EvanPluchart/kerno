//! The tunnel through the system's `ssh` on Windows: not yet (K64, D-131).
//!
//! On Unix the tunnel forwards a Unix socket and asks its questions over a
//! second one; neither std nor tokio has Unix sockets on Windows, and
//! Windows' OpenSSH forwards none. [`super::OpenSshEnv::detect`] finds no
//! OpenSSH there, so the dialog offers the built-in client, and a connection
//! saved elsewhere with "system OpenSSH" gets [`super::NOT_FOUND`], which
//! says so. Nothing here ever runs `ssh`.

use std::path::Path;

use super::{NOT_FOUND, OpenSshRequest};
use crate::ssh::SshError;

/// The stream a running tunnel would give: none is ever made on Windows.
pub(crate) type LocalStream = tokio::net::TcpStream;

/// A running tunnel: none can exist on Windows (no value of this type).
#[derive(Debug)]
pub enum Forward {}

impl Forward {
    /// Refuses, saying that the system's OpenSSH is not used on Windows yet.
    pub async fn start(_request: OpenSshRequest<'_>) -> Result<Self, SshError> {
        Err(SshError::Other(NOT_FOUND.into()))
    }

    pub fn dir(&self) -> &Path {
        match *self {}
    }

    pub fn socket(&self) -> &Path {
        match *self {}
    }

    pub fn target(&self) -> (&str, u16) {
        match *self {}
    }

    pub fn is_closed(&self) -> bool {
        match *self {}
    }

    pub fn forward_mark(&self) -> u64 {
        match *self {}
    }

    pub async fn forward_failure(&self, _mark: u64) -> Option<String> {
        match *self {}
    }

    pub(crate) async fn connect(&self) -> std::io::Result<LocalStream> {
        match *self {}
    }
}

/// Kerno run by `ssh` as its askpass program: never on Windows, where no
/// tunnel runs `ssh`.
pub fn answer_askpass() -> Option<i32> {
    None
}
