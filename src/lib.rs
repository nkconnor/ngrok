use reqwest::blocking::Client;
use serde::Deserialize;
use std::io;
use std::marker::PhantomData;
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
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
pub enum Error {
    #[error(transparent)]
    DeserializationError(#[from] serde_json::Error),
    #[error("Expected a tunnel but found none")]
    TunnelNotFound,
    #[error(transparent)]
    IOError(#[from] io::Error),
    #[error(transparent)]
    RequestError(#[from] reqwest::Error),
}

#[derive(Debug)]
pub struct Ngrok {
    client: Client,
    port: u16,
    stop: Sender<()>,
    exited: Receiver<io::Result<()>>,
    started_at: Instant,
}

/// A ngrok tunnel. It is supposed to be valid
/// for 'a which is tied to the underlying  child process
pub struct Tunnel<'a> {
    url: Url,
    phantom: PhantomData<&'a str>,
}

impl<'a> Tunnel<'a> {
    /// The tunnel's http URL
    fn http(&self) -> Url {
        self.url.clone()
    }

    /// The tunnel's https URL
    fn https(&self) -> Url {
        let mut http = self.url.clone();
        http.set_scheme("https").expect("what could go wrong?");
        http
    }
}

impl Ngrok {
    /// Retrieve the ngrok tunnel
    pub fn tunnel<'a>(&'a self) -> Result<Tunnel<'a>, Error> {
        while self.started_at.elapsed().as_secs() < 4 {
            thread::sleep(Duration::from_secs(1));
        }

        // ensure the process hasn't quit
        match self.exited.try_recv() {
            Err(TryRecvError::Disconnected) => Err(TryRecvError::Disconnected),
            _ => Ok(()),
        }
        .expect("Exit channel remains open because instance has not dropped");

        let response = self
            .client
            .get("http://localhost:4040/api/tunnels")
            .send()?
            .json::<GetTunnels>()?;

        let url = response
            .tunnels
            .into_iter()
            .find(|tunnel| match tunnel.config.addr.port() {
                Some(port) => port == self.port,
                None => false,
            })
            .map(|t| Ok(t.public_url))
            .unwrap_or(Err(Error::TunnelNotFound))?;

        Ok(Tunnel {
            url,
            phantom: PhantomData::default(),
        })
    }
}

impl Drop for Ngrok {
    /// Stop the Ngrok child process
    fn drop(&mut self) -> () {
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

#[derive(Debug, Clone, Default)]
struct NgrokBuilder {
    http: Option<()>,
    port: Option<u16>,
    executable: Option<String>,
}

fn builder() -> NgrokBuilder {
    NgrokBuilder {
        ..Default::default()
    }
}

impl NgrokBuilder {
    fn http(&mut self) -> Self {
        self.http = Some(());
        self.clone()
    }

    fn port(&mut self, port: u16) -> Self {
        self.port = Some(port);
        self.clone()
    }

    fn executable(&mut self, executable: &str) -> Self {
        self.executable = Some(executable.to_string());
        self.clone()
    }

    // TODO
    // This should return the io::Error early and move the proc
    // into the thread..
    fn run(self) -> Result<Ngrok, &'static str> {
        if let NgrokBuilder {
            http: Some(()),
            port: Some(port),
            executable,
        } = self
        {
            let (tx_stop, rx_stop) = channel();
            let (tx_exit, rx_exit) = channel();

            thread::spawn(move || {
                match Command::new(executable.unwrap_or("ngrok".to_string()))
                    .stdout(Stdio::piped())
                    .arg("http")
                    .arg(port.to_string())
                    .spawn()
                {
                    Ok(mut proc) => {
                        loop {
                            // See if process exited
                            match proc.try_wait().map(|_| ()) {
                                Err(e) => {
                                    tx_exit.send(Err(e)).unwrap();
                                    break;
                                }
                                _ => (),
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
                                    //panic!("Channel closed unexpectedly");
                                }
                            };
                        }
                    }
                    Err(err) => tx_exit.send(Err(err)).unwrap(),
                };
            });

            Ok(Ngrok {
                stop: tx_stop,
                exited: rx_exit,
                port,
                client: Client::new(),
                started_at: Instant::now(),
            })
        } else {
            Err("You should have specified http and port")
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn simple() {
        let ngrok = crate::builder().http().port(3030).run().unwrap();
        ngrok.tunnel().unwrap();
    }
}
