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
//!     let tunnel = ngrok::builder()
//!           // server protocol
//!           .https()
//!           // the port
//! #         .executable("./ngrok")
//!           .port(3030)
//!           .run()?;
//!
//!     let public_url = tunnel.public_url()?;
//!
//!     Ok(())
//! }
//! ```

use std::fmt::Display;
use std::process::Child;
use std::sync::Arc;
use std::sync::Mutex;
use std::{fmt, io, process::Command, process::Stdio, thread, time::Duration, time::Instant};
use url::Url;

#[derive(Debug)]
enum Error {
    MalformedAPIResponse,

    TunnelNotFound,

    BuilderError(&'static str),

    TunnelProcessExited(String),
}

impl Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::MalformedAPIResponse => write!(f,"Unexpected JSON found in `ngrok`'s JSON API"),
            Error::TunnelNotFound => write!(f, "Expected a matching tunnel but found none under `ngrok`'s JSON API @ http://localhost:4040/api/tunnels"),
            Error::BuilderError(arg) => write!(f, "Builder expected {}", arg),
            Error::TunnelProcessExited(code) => write!(f, "Tunnel exited unexpectedly with exit status {}", code)
        }
    }
}

impl std::error::Error for Error {}

impl From<Error> for io::Error {
    fn from(err: Error) -> Self {
        io::Error::new(io::ErrorKind::Other, err)
    }
}

type Resource = Arc<Mutex<Child>>;

/// A running `ngrok` Tunnel.
#[derive(Debug, Clone)]
pub struct Tunnel {
    pub(crate) proc: Resource,
    /// The tunnel's public URL
    public_url: url::Url,
}

impl AsRef<url::Url> for Tunnel {
    fn as_ref(&self) -> &url::Url {
        &self.public_url
    }
}

impl fmt::Display for Tunnel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.public_url.fmt(f)
    }
}

impl Tunnel {
    /// Build a new `ngrok` Tunnel
    pub fn builder() -> Builder {
        crate::builder()
    }

    /// Determine if the underlying child process has exited
    /// and return the exit error if so.
    pub fn status(&self) -> Result<(), io::Error> {
        let status = { self.proc.lock().unwrap().try_wait()? };

        match status {
            Some(code) => Err(io::Error::from(Error::TunnelProcessExited(
                code.to_string(),
            ))),
            _ => Ok(()),
        }
    }

    /// Retrieve the tunnel's public URL. If the underlying process has terminated,
    /// this will return the exit status
    pub fn public_url(&self) -> Result<&Url, io::Error> {
        self.status()?;
        Ok(&self.public_url)
    }

    /// Retrieve the tunnel's public URL.
    pub fn public_url_unchecked(&self) -> &Url {
        &self.public_url
    }
}

impl Drop for Tunnel {
    /// Stop the Ngrok child process
    fn drop(&mut self) {
        let _result = self.proc.lock().unwrap().kill();
    }
}

/// Build a `ngrok` Tunnel. Use `ngrok::builder()` to create this.
#[derive(Debug, Clone, Default)]
pub struct Builder {
    https: Option<()>,
    port: Option<u16>,
    executable: Option<String>,
}

/// The entry point for starting a `ngrok` tunnel. Only HTTPS is currently supported.
///
/// **Example**
///
/// ```
/// ngrok::builder()
///         .executable("./ngrok")
///         .https()
///         .port(3031)
///         .run()
///         .unwrap();
/// ```
pub fn builder() -> Builder {
    Builder {
        ..Default::default()
    }
}

impl Builder {
    /// Create a new `Builder`
    pub fn new() -> Self {
        Builder {
            ..Default::default()
        }
    }

    /// Set the tunnel protocol to HTTP
    pub fn https(&mut self) -> Self {
        self.https = Some(());
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

    /// Start the `ngrok` child process. Note this is a blocking call
    /// and it will sleep for several seconds.
    // There is a detached thread that waits for either
    // A: the Ngrok instance to drop, which in `impl Drop` sends a message over
    // the channel, or
    // B: the underlying process to quit
    pub fn run(self) -> Result<Tunnel, io::Error> {
        // Prepare for TCP/other
        let _http = self
            .https
            .ok_or(Error::BuilderError(".https() should have been called"))?;

        let port = self
            .port
            .ok_or(Error::BuilderError(".port(port) should have been set"))?;

        let started_at = Instant::now();

        // Start the `ngrok` process
        let proc = Command::new(self.executable.unwrap_or_else(|| "ngrok".to_string()))
            .stdout(Stdio::piped())
            .arg("http")
            .arg(port.to_string())
            .spawn()?;

        // ngrok takes a bit to start up and this is a (probably bad) way to wait
        // for the tunnel to appear:
        let public_url = {
            loop {
                let public_url = find_public_url(port);
                if public_url.is_ok() {
                    break public_url;
                }

                // If 5 seconds have elapsed, mission failed
                if started_at.elapsed().as_secs() > 5 {
                    break public_url;
                }

                // Elsewise try again in 300 millis
                thread::sleep(Duration::from_millis(300));
            }
        }?;

        Ok(Tunnel {
            public_url,
            proc: Arc::new(Mutex::new(proc)),
        })
    }
}

fn find_public_url(port: u16) -> Result<url::Url, io::Error> {
    use serde_json::Value;

    // Retrieve the `tunnel_url`
    let response: Value = ureq::get("http://localhost:4040/api/tunnels")
        .call()
        .into_json()?;

    let tunnels = response
        .get("tunnels")
        .and_then(|tunnels| tunnels.as_array())
        .map(Ok)
        .unwrap_or(Err(Error::MalformedAPIResponse))?;

    // snag both HTTP/HTTPS urls
    fn find_tunnel_url<'a, I: IntoIterator<Item = &'a Value>>(
        scheme: &'static str,
        port: u16,
        iter: I,
    ) -> Result<url::Url, Error> {
        for tunnel in iter {
            let tunnel_url = tunnel.get("public_url").and_then(|url| url.as_str());

            let is_port = tunnel
                .get("config")
                .and_then(|cfg| cfg.get("addr"))
                .and_then(|addr| addr.as_str())
                .map(|addr| addr.contains(&port.to_string()))
                .unwrap_or(false);

            let is_scheme = tunnel_url.map(|url| url.contains(scheme)).unwrap_or(false);

            if is_scheme && is_port {
                return Ok(url::Url::parse(tunnel_url.unwrap())
                    .map_err(|_| Error::MalformedAPIResponse)?);
            }
        }

        Err(Error::TunnelNotFound)
    }

    let public_url = find_tunnel_url("https://", port, tunnels)?;

    Ok(public_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_status_if_proc_killed() {
        let tunnel = builder()
            .executable("./ngrok")
            .https()
            .port(3030)
            .run()
            .unwrap();
        tunnel.proc.lock().unwrap().kill().unwrap();
        std::thread::sleep(Duration::from_millis(2500));
        assert!(tunnel.public_url().is_err())
    }

    #[tokio::test(threaded_scheduler)]
    async fn test_proxy_to_local_server() {
        use warp::Filter;

        let routes = warp::any().map(|| warp::reply());

        let handle =
            tokio::task::spawn(
                async move { warp::serve(routes).run(([127, 0, 0, 1], 3060)).await },
            );

        let tunnel = builder()
            .executable("./ngrok")
            .https()
            .port(3060)
            .run()
            .unwrap();

        let status = ureq::get(tunnel.public_url().unwrap().as_str())
            .call()
            .status();
        assert_eq!(status, 200);

        drop(handle)
    }
}
