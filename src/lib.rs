//! # Ngrok
//!
//! A minimal and concise [`ngrok`](https://ngrok.com/) wrapper for Rust. The main use case for the library
//! is the ability to open public HTTP tunnels to your development server(s) for
//! integrations tests. TCP support, while not available, should be trivial to support.
//!
//! This has been tested with Linux and assume that it does not work on Windows (contributions
//! welcome).
//!
//! ## Usage
//! ```
//! fn main() -> std::io::Result<()> {
//!     let ngrok = ngrok::builder()
//!           // server protocol
//!           .http()
//!           // the port
//!           .port(3030)
//!           .run()?;
//!
//!     let public: url::Url = ngrok.tunnel()?.http();
//!
//!     Ok(())
//! }
//! ```

use serde::Deserialize;
use std::fmt;
use std::io;
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;
use url::Url;

// NGROK JSON API types
#[derive(Debug, Deserialize)]
struct GetTunnels {
    tunnels: Vec<ApiTunnel>,
}

#[derive(Debug, Deserialize)]
struct Config {
    addr: Url,
}

#[derive(Debug, Deserialize)]
struct ApiTunnel {
    config: Config,
    public_url: Url,
}

#[derive(Error, Debug)]
enum Error {
    #[error("Expected a matching tunnel but found none under `ngrok`'s JSON API @ http://localhost:4040/api/tunnels")]
    TunnelNotFound,

    #[error("Builder expected `{0}`")]
    BuilderError(&'static str),
}

impl From<Error> for io::Error {
    fn from(err: Error) -> Self {
        io::Error::new(io::ErrorKind::Other, err)
    }
}

/// A running `ngrok` process.
#[derive(Debug, Clone)]
pub struct Ngrok {
    /// The host port being tunneled
    port: u16,
    /// Tell the process to exit
    stop: Sender<()>,
    /// The tunnel's public URL
    tunnel_url: url::Url,
    /// The process exited with this result. Sends exactly once
    exited: Arc<Receiver<io::Result<()>>>,
}

/// A ngrok tunnel. It has a lifetime which is attached to the underlying child process.
#[derive(Debug, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct Tunnel<'a> {
    url: &'a Url,
}

impl<'a> Tunnel<'a> {
    /// The tunnel's http URL
    pub fn http(&self) -> Url {
        self.url.clone()
    }

    /// The tunnel's https URL
    pub fn https(&self) -> Url {
        let mut http = self.url.clone();
        http.set_scheme("https").expect("what could go wrong?");
        http
    }
}

impl<'a> From<Tunnel<'a>> for url::Url {
    fn from(tun: Tunnel<'a>) -> Self {
        tun.url.clone()
    }
}

impl AsRef<url::Url> for Tunnel<'_> {
    fn as_ref(&self) -> &url::Url {
        &self.url
    }
}
impl fmt::Display for Tunnel<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.url.fmt(f)
    }
}

impl Ngrok {
    /// Determine if the underlying child process has exited
    /// and return the exit error if so.
    pub fn status(&self) -> Result<(), io::Error> {
        match self.exited.try_recv() {
            Ok(Err(e)) => Err(e),
            _ => Ok(()),
        }
    }

    /// Retrieve the ngrok tunnel. This does not check if the tunnel is still
    /// active.
    pub fn tunnel_unchecked(&self) -> Tunnel<'_> {
        Tunnel {
            url: &self.tunnel_url,
        }
    }

    /// Retrieve the ngrok tunnel. If the underlying process has terminated,
    /// this will return the exit status.
    pub fn tunnel(&self) -> Result<Tunnel<'_>, io::Error> {
        self.status()?;

        Ok(Tunnel {
            url: &self.tunnel_url,
        })
    }
}

impl Drop for Ngrok {
    /// Stop the Ngrok child process
    fn drop(&mut self) {
        // Process already exited, dooh!
        let _result: io::Result<()> = if let Ok(result) = self.exited.try_recv() {
            result
        } else {
            // Send the stop signal
            self.stop.send(()).expect("channel is standing");
            self.exited.recv().expect("channel is standing")
        };
    }
}

/// Build a `Ngrok` process. Use `ngrok::builder()` to create this.
#[derive(Debug, Clone, Default)]
pub struct NgrokBuilder {
    http: Option<()>,
    port: Option<u16>,
    executable: Option<String>,
}

/// The entry point for starting a `ngrok` tunnel. Only HTTP is currently supported.
///
/// **Example**
///
/// ```ignore
/// ngrok::builder()
///         .executable("ngrok")
///         .http()
///         .port(3030)
///         .run()
///         .unwrap();
/// ```
pub fn builder() -> NgrokBuilder {
    NgrokBuilder {
        ..Default::default()
    }
}

impl NgrokBuilder {
    /// Create a new `NgrokBuilder`
    pub fn new() -> Self {
        NgrokBuilder {
            ..Default::default()
        }
    }

    /// Set the tunnel protocol to HTTP
    pub fn http(&mut self) -> Self {
        self.http = Some(());
        self.clone()
    }

    /// Set the tunnel port
    pub fn port(&mut self, port: u16) -> Self {
        self.port = Some(port);
        self.clone()
    }

    /// Set the `ngrok` executable path. By default the builder
    /// assumes `ngrok` is on your path.
    pub fn executable(&mut self, executable: &str) -> Self {
        self.executable = Some(executable.to_string());
        self.clone()
    }

    /// Start the `ngrok` child process
    // There is a detached thread that waits for either
    // A: the Ngrok instance to drop, which in `impl Drop` sends a message over
    // the channel, or
    // B: the underlying process to quit
    pub fn run(self) -> Result<Ngrok, io::Error> {
        // Prepare for TCP/other
        let _http = self
            .http
            .ok_or_else(|| Error::BuilderError(".http() should have been called"))?;

        let port = self
            .port
            .ok_or_else(|| Error::BuilderError(".port(port) should have been set"))?;

        let started_at = Instant::now();

        // Start the `ngrok` process
        let mut proc = Command::new(self.executable.unwrap_or_else(|| "ngrok".to_string()))
            .stdout(Stdio::piped())
            .arg("http")
            .arg(port.to_string())
            .spawn()?;

        // Give it a minute to start up
        while started_at.elapsed().as_secs() < 4 {
            thread::sleep(Duration::from_secs(1));
        }

        // Retrieve the `tunnel_url`
        let response = ureq::get("http://localhost:4040/api/tunnels")
            .call()
            .into_json()?;

        let response: GetTunnels = serde_json::from_value(response)?;

        let tunnel_url = response
            .tunnels
            .into_iter()
            .find(|tunnel| match tunnel.config.addr.port() {
                Some(p) => p == port,
                None => false,
            })
            .map(|t| Ok(t.public_url))
            .unwrap_or(Err(Error::TunnelNotFound))?;

        // Process management
        let (tx_stop, rx_stop) = channel();
        let (tx_exit, rx_exit) = channel();

        thread::spawn(move || {
            loop {
                // See if process exited
                if let Err(e) = proc.try_wait() {
                    tx_exit.send(Err(e)).unwrap();
                    break;
                }

                // If Ngrok::stop is called, kill the process
                match rx_stop.try_recv() {
                    Ok(()) => {
                        tx_exit.send(proc.kill()).unwrap();
                        break;
                    }
                    // Nothing to see here, move on.
                    Err(TryRecvError::Empty) => (),
                    // This would happen if Ngrok was dropped for example.
                    // But if that were the case, then nothing could run on the
                    // channel, right?
                    Err(TryRecvError::Disconnected) => {
                        break;
                    }
                };
            }
        });

        Ok(Ngrok {
            tunnel_url,
            stop: tx_stop,
            exited: Arc::new(rx_exit),
            port,
        })
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn test_display() {
        let url = url::Url::parse("http://localhost/api").unwrap();
        let tunnel = Tunnel { url: &url };
        assert_eq!(format!("{}", url), format!("{}", tunnel));
    }
}
