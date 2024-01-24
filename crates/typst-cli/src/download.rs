// Acknowledgement:
// Closely modelled after rustup's [`DownloadTracker`].
// https://github.com/rust-lang/rustup/blob/master/src/cli/download_tracker.rs

use std::collections::VecDeque;
use std::io::{self, ErrorKind, Read, Stderr, Write};
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use reqwest::blocking::{Client, Response};
use reqwest::{Certificate, StatusCode};

/// Keep track of this many download speed samples.
const SPEED_SAMPLES: usize = 5;

/// Lazily loads a custom CA certificate if present, but if there's an error
/// loading certificate, it just uses the default configuration.
static CERT: Lazy<Option<Certificate>> = Lazy::new(|| {
    let path = crate::ARGS.cert.as_ref()?;
    let pem = std::fs::read(path).ok()?;
    reqwest::Certificate::from_pem(&pem).ok()
});

/// Download binary data and display its progress.
pub fn download_with_progress(url: &str) -> io::Result<Vec<u8>> {
    let response = download(url)?;
    RemoteReader::from_response(response).download()
}

/// Download from a URL.
pub fn download(url: &str) -> io::Result<Response> {
    let response =
        download_inner(url).map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;
    let status = response.status();
    if status.is_success() {
        Ok(response)
    } else if status == StatusCode::NOT_FOUND {
        Err(io::ErrorKind::NotFound.into())
    } else {
        Err(io::Error::new(io::ErrorKind::Other, format!("{status}")))
    }
}

/// Internal download implementation.
fn download_inner(url: &str) -> reqwest::Result<Response> {
    let mut builder =
        Client::builder().user_agent(concat!("typst/", env!("CARGO_PKG_VERSION")));

    // Get the network proxy config from the environment.
    if let Some(proxy) = env_proxy::for_url_str(url)
        .to_url()
        .and_then(|url| reqwest::Proxy::all(url).ok())
    {
        builder = builder.proxy(proxy);
    }

    // Apply a custom CA certificate if present.
    if let Some(cert) = &*CERT {
        builder = builder.add_root_certificate(cert.clone());
    }

    builder.build()?.get(url).send()
}

/// A wrapper around [`ureq::Response`] that reads the response body in chunks
/// over a websocket and displays statistics about its progress.
///
/// Downloads will _never_ fail due to statistics failing to print, print errors
/// are silently ignored.
struct RemoteReader {
    response: Response,
    content_len: Option<usize>,
    total_downloaded: usize,
    downloaded_this_sec: usize,
    downloaded_last_few_secs: VecDeque<usize>,
    start_time: Instant,
    last_print: Option<Instant>,
    displayed_charcount: Option<usize>,
    stderr: Stderr,
}

impl RemoteReader {
    /// Wraps a [`ureq::Response`] and prepares it for downloading.
    ///
    /// The 'Content-Length' header is used as a size hint for read
    /// optimization, if present.
    pub fn from_response(response: Response) -> Self {
        let content_len: Option<usize> = response
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|header| header.to_str().ok()?.parse().ok());

        Self {
            response,
            content_len,
            total_downloaded: 0,
            downloaded_this_sec: 0,
            downloaded_last_few_secs: VecDeque::with_capacity(SPEED_SAMPLES),
            start_time: Instant::now(),
            last_print: None,
            displayed_charcount: None,
            stderr: io::stderr(),
        }
    }

    /// Download the bodies content as raw bytes while attempting to print
    /// download statistics to standard error. Download progress gets displayed
    /// and updated every second.
    ///
    /// These statistics will never prevent a download from completing, errors
    /// are silently ignored.
    pub fn download(mut self) -> io::Result<Vec<u8>> {
        let mut buffer = vec![0; 8192];
        let mut data = match self.content_len {
            Some(content_len) => Vec::with_capacity(content_len),
            None => Vec::with_capacity(8192),
        };

        loop {
            let read = match self.response.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => n,
                // If the data is not yet ready but will be available eventually
                // keep trying until we either get an actual error, receive data
                // or an Ok(0).
                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };

            data.extend(&buffer[..read]);

            let last_printed = match self.last_print {
                Some(prev) => prev,
                None => {
                    let current_time = Instant::now();
                    self.last_print = Some(current_time);
                    current_time
                }
            };
            let elapsed = Instant::now().saturating_duration_since(last_printed);

            self.total_downloaded += read;
            self.downloaded_this_sec += read;

            if elapsed >= Duration::from_secs(1) {
                if self.downloaded_last_few_secs.len() == SPEED_SAMPLES {
                    self.downloaded_last_few_secs.pop_back();
                }

                self.downloaded_last_few_secs.push_front(self.downloaded_this_sec);
                self.downloaded_this_sec = 0;

                if let Some(n) = self.displayed_charcount {
                    self.erase_chars(n);
                }

                self.display();
                let _ = write!(self.stderr, "\r");
                self.last_print = Some(Instant::now());
            }
        }

        self.display();
        let _ = writeln!(self.stderr);

        Ok(data)
    }

    /// Compile and format several download statistics and make an attempt at
    /// displaying them on standard error.
    fn display(&mut self) {
        let sum: usize = self.downloaded_last_few_secs.iter().sum();
        let len = self.downloaded_last_few_secs.len();
        let speed = if len > 0 { sum / len } else { self.content_len.unwrap_or(0) };

        let total = as_time_unit(self.total_downloaded, false);
        let speed_h = as_time_unit(speed, true);
        let elapsed =
            time_suffix(Instant::now().saturating_duration_since(self.start_time));

        let output = match self.content_len {
            Some(content_len) => {
                let percent = (self.total_downloaded as f64 / content_len as f64) * 100.;
                let remaining = content_len - self.total_downloaded;

                format!(
                    "{} / {} ({:3.0} %) {} in {} ETA: {}",
                    total,
                    as_time_unit(content_len, false),
                    percent,
                    speed_h,
                    elapsed,
                    time_suffix(Duration::from_secs(if speed == 0 {
                        0
                    } else {
                        (remaining / speed) as u64
                    }))
                )
            }
            None => format!("Total: {total} Speed: {speed_h} Elapsed: {elapsed}"),
        };

        let _ = write!(self.stderr, "{output}");

        self.displayed_charcount = Some(output.chars().count());
    }

    /// Erase each previously printed character and add a carriage return
    /// character, clearing the line for the next `display()` update.
    fn erase_chars(&mut self, count: usize) {
        let _ = write!(self.stderr, "{}", " ".repeat(count));
        let _ = write!(self.stderr, "\r");
    }
}

/// Append a unit-of-time suffix.
fn time_suffix(duration: Duration) -> String {
    let secs = duration.as_secs();
    match format_dhms(secs) {
        (0, 0, 0, s) => format!("{s:2.0}s"),
        (0, 0, m, s) => format!("{m:2.0}m {s:2.0}s"),
        (0, h, m, s) => format!("{h:2.0}h {m:2.0}m {s:2.0}s"),
        (d, h, m, s) => format!("{d:3.0}d {h:2.0}h {m:2.0}m {s:2.0}s"),
    }
}

/// Format the total amount of seconds into the amount of days, hours, minutes
/// and seconds.
fn format_dhms(sec: u64) -> (u64, u8, u8, u8) {
    let (mins, sec) = (sec / 60, (sec % 60) as u8);
    let (hours, mins) = (mins / 60, (mins % 60) as u8);
    let (days, hours) = (hours / 24, (hours % 24) as u8);
    (days, hours, mins, sec)
}

/// Format a given size as a unit of time. Setting `include_suffix` to true
/// appends a '/s' (per second) suffix.
fn as_time_unit(size: usize, include_suffix: bool) -> String {
    const KI: f64 = 1024.0;
    const MI: f64 = KI * KI;
    const GI: f64 = KI * KI * KI;

    let size = size as f64;

    let suffix = if include_suffix { "/s" } else { "" };

    if size >= GI {
        format!("{:5.1} GiB{}", size / GI, suffix)
    } else if size >= MI {
        format!("{:5.1} MiB{}", size / MI, suffix)
    } else if size >= KI {
        format!("{:5.1} KiB{}", size / KI, suffix)
    } else {
        format!("{size:3.0} B{suffix}")
    }
}
