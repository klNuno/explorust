//! `explorust-index`: the MFT/USN index service. See the crate docs.
//!
//! - `--service`: run under the service control manager (how the installed service starts).
//! - `--console`: run the same index in the foreground, for tests (elevated).
//! - `--install` / `--uninstall`: register (auto start, LocalSystem) and start, or stop and delete,
//!   the service `explorust-index` for this exe; `explorust-guard --install` and `--restore` call them.
//! - `--query <text>`: ask the running service and print the hits and timings.

#[cfg(any(target_os = "windows", test))]
mod hydration;
#[cfg(target_os = "windows")]
mod log;
#[cfg(target_os = "windows")]
mod metadata;
#[cfg(target_os = "windows")]
mod pipe;
#[cfg(any(target_os = "windows", test))]
mod retry;
#[cfg(target_os = "windows")]
mod server;
#[cfg(target_os = "windows")]
mod service;
#[cfg(target_os = "windows")]
mod volume;

#[cfg(target_os = "windows")]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("--service") => service::dispatch(),
        Some("--console") => {
            server::run(None);
            0
        }
        Some("--install") => service::install(),
        Some("--uninstall") => service::uninstall(),
        Some("--query") => query(&args[1..].join(" ")),
        _ => {
            println!("usage: explorust-index --service | --console | --install | --uninstall | --query <text>");
            2
        }
    };
    std::process::exit(code as i32);
}

#[cfg(target_os = "windows")]
fn query(q: &str) -> u32 {
    let mut c = fsindex::client::Client::default();
    let t = std::time::Instant::now();
    match c.query(q, 24) {
        Ok(r) => {
            println!("{} hits in {:.2} ms (service {} us, {} names)", r.hits.len(), t.elapsed().as_secs_f64() * 1000.0, r.micros, r.names);
            for h in r.hits {
                println!("{} {}{} {}", h.rank, if h.folder { "d" } else { "f" }, if h.noisy { "n" } else { " " }, h.path);
            }
            0
        }
        Err(e) => {
            println!("query failed: {e}");
            1
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {}
