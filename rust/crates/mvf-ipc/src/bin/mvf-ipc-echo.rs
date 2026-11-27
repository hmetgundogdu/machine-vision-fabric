//! `mvf-ipc-echo`: a fixture worker for protocol benchmarks and fault injection (#32).
//!
//! ```text
//! mvf-ipc-echo [--protocol 1|2] [--transport stdio|fd3] [--fault MODE] [--work-us N]
//! ```
//!
//! Answers every `execute` with a `result` naming the output slot, after `--work-us` of busy work.
//! `--transport fd3` (Unix) speaks on an inherited socket at fd 3 instead of stdio. Faults (v2):
//! `malformed`, `oversized`, `partial`, `crash-mid-frame`, `stale-generation`, `hang`,
//! `bad-magic`, `log-flood`, `print` (writes to stdout like a stray `print()`), all on the 3rd
//! request; `v1-only` ignores a v2 offer and speaks v1 throughout.

use std::io::{BufRead, BufReader, Read, Write};
use std::time::{Duration, Instant};

use mvf_ipc::frame::{Header, Kind, MAX_FRAME, read_frame, write_frame};
use mvf_ipc::msg::{Execute, ResultFlags, ResultMsg};

struct Args {
    protocol: u8,
    fd3: bool,
    fault: Option<String>,
    work_us: u64,
}

fn args() -> Args {
    let mut a = Args {
        // Like the SDKs: v2 only when the engine offers it, with its launch token.
        protocol: if std::env::var("MVF_PROTOCOL").as_deref() == Ok("2")
            && std::env::var_os("MVF_SESSION").is_some_and(|s| !s.is_empty())
        {
            2
        } else {
            1
        },
        fd3: false,
        fault: None,
        work_us: 0,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().unwrap_or_default();
        match k.as_str() {
            "--protocol" => a.protocol = v.parse().unwrap_or(2),
            "--transport" => a.fd3 = v == "fd3",
            "--fault" => a.fault = Some(v),
            "--work-us" => a.work_us = v.parse().unwrap_or(0),
            _ => {}
        }
    }
    // Under the engine a contract cannot pass arguments: a `<binary>.fault` file next to the
    // packaged binary names the fault instead.
    if a.fault.is_none()
        && let Ok(exe) = std::env::current_exe()
        && let Ok(f) = std::fs::read_to_string(exe.with_extension("fault"))
    {
        a.fault = Some(f.trim().to_string());
    }
    if a.fault.as_deref() == Some("v1-only") {
        a.protocol = 1;
    }
    a
}

fn spin(us: u64) {
    if us == 0 {
        return;
    }
    let until = Instant::now() + Duration::from_micros(us);
    while Instant::now() < until {
        std::hint::spin_loop();
    }
}

/// The v2 SDK convention: the protocol keeps private copies of the stdio descriptors; fd 1 then
/// points at stderr (a stray print lands in the logs) and fd 0 reads nothing.
#[cfg(unix)]
fn protected_stdio() -> (Box<dyn Read>, Box<dyn Write>) {
    use std::os::fd::FromRawFd;
    // SAFETY: plain descriptor duplication at start, before any other thread exists.
    unsafe {
        let input = libc::dup(0);
        let output = libc::dup(1);
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY);
        libc::dup2(null, 0);
        libc::close(null);
        libc::dup2(2, 1);
        (
            Box::new(std::fs::File::from_raw_fd(input)),
            Box::new(std::fs::File::from_raw_fd(output)),
        )
    }
}

/// Windows: the protocol keeps duplicates of the standard handles; the process's standard output
/// handle then points at stderr (Rust's `println!` and `WriteFile(GetStdHandle(...))` follow it).
#[cfg(windows)]
fn protected_stdio() -> (Box<dyn Read>, Box<dyn Write>) {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: plain handle duplication at start, before any other thread exists.
    unsafe {
        let dup = |h: HANDLE| -> HANDLE {
            let mut out: HANDLE = std::ptr::null_mut();
            let me = GetCurrentProcess();
            DuplicateHandle(me, h, me, &mut out, 0, 0, DUPLICATE_SAME_ACCESS);
            out
        };
        let input = dup(GetStdHandle(STD_INPUT_HANDLE));
        let output = dup(GetStdHandle(STD_OUTPUT_HANDLE));
        SetStdHandle(STD_OUTPUT_HANDLE, GetStdHandle(STD_ERROR_HANDLE));
        (
            Box::new(std::fs::File::from_raw_handle(input)),
            Box::new(std::fs::File::from_raw_handle(output)),
        )
    }
}

#[cfg(not(any(unix, windows)))]
fn protected_stdio() -> (Box<dyn Read>, Box<dyn Write>) {
    (Box::new(std::io::stdin()), Box::new(std::io::stdout()))
}

#[cfg(unix)]
fn fd3() -> (Box<dyn Read>, Box<dyn Write>) {
    use std::os::fd::FromRawFd;
    // SAFETY: the launcher passed a connected socket at fd 3; we own it from here on.
    let s = unsafe { std::os::unix::net::UnixStream::from_raw_fd(3) };
    let w = s.try_clone().expect("clone fd 3");
    (Box::new(s), Box::new(w))
}

#[cfg(not(unix))]
fn fd3() -> (Box<dyn Read>, Box<dyn Write>) {
    panic!("--transport fd3 is Unix only")
}

fn main() {
    let a = args();
    let (input, mut output): (Box<dyn Read>, Box<dyn Write>) = if a.fd3 {
        fd3()
    } else if a.protocol == 2 {
        protected_stdio()
    } else {
        (Box::new(std::io::stdin()), Box::new(std::io::stdout()))
    };
    // Under the engine (an arena exists) answer as an analyzer: a JSON value, no output frame.
    let analyzer = std::env::var_os("MVF_ARENA_PATH").is_some();
    if a.protocol == 1 {
        v1(input, output, &a);
        return;
    }
    let session = match a.fault.as_deref() {
        Some("wrong-session") => "not-the-launch-token".to_string(),
        _ => std::env::var("MVF_SESSION").unwrap_or_default(),
    };
    let generation: u32 = std::env::var("MVF_GENERATION")
        .ok()
        .and_then(|g| g.parse().ok())
        .unwrap_or(1);
    let hello = serde_json::json!({"protocol": 2, "moduleId": "fixture.echo", "capability": if analyzer { "analyzer" } else { "processor" }, "session": session});
    write_frame(
        &mut output,
        Header::new(Kind::Hello, 0, generation),
        hello.to_string().as_bytes(),
    )
    .expect("hello");
    let mut input = input;
    let mut body = Vec::new();
    let mut n = 0u64;
    loop {
        let h = match read_frame(&mut input, MAX_FRAME, &mut body) {
            Ok(h) => h,
            Err(_) => return,
        };
        match h.kind {
            Kind::Shutdown => return,
            Kind::Ping => {
                let _ = write_frame(
                    &mut output,
                    Header::new(Kind::Pong, h.id, h.generation),
                    &[],
                );
            }
            Kind::Execute => {
                n += 1;
                let exec = Execute::decode(&body).expect("execute");
                spin(a.work_us);
                let result = if analyzer {
                    ResultMsg {
                        flags: 0,
                        out: 0,
                        sequence: 0,
                        tail: Some(
                            serde_json::json!({"value": {"n": n, "sequence": exec.sequence}}),
                        ),
                    }
                } else {
                    ResultMsg {
                        flags: ResultFlags::FRAME,
                        out: exec.out.map(|o| o.0).unwrap_or(0),
                        sequence: 0,
                        tail: None,
                    }
                };
                let mut reply = Header::new(Kind::Result, h.id, h.generation);
                if n == 3
                    && let Some(f) = a.fault.as_deref()
                {
                    match f {
                        "malformed" => {
                            let _ = write_frame(&mut output, reply, b"short");
                            continue;
                        }
                        "oversized" => {
                            reply.length = 8 << 20;
                            let _ = output.write_all(&reply.encode());
                            let _ = output.flush();
                            std::thread::sleep(Duration::from_secs(30));
                            return;
                        }
                        "partial" => {
                            let mut b = reply.encode().to_vec();
                            b.extend_from_slice(&result.encode());
                            let _ = output.write_all(&b[..10]);
                            let _ = output.flush();
                            std::thread::sleep(Duration::from_secs(30));
                            return;
                        }
                        "crash-mid-frame" => {
                            let _ = output.write_all(&reply.encode()[..12]);
                            let _ = output.flush();
                            std::process::exit(3);
                        }
                        "stale-generation" => reply.generation = h.generation.wrapping_sub(1),
                        "hang" => {
                            std::thread::sleep(Duration::from_secs(30));
                            return;
                        }
                        "bad-magic" => {
                            let _ = output.write_all(b"XX\x02\x04garbage-garbage-garbage");
                            let _ = output.flush();
                            std::thread::sleep(Duration::from_secs(30));
                            return;
                        }
                        "log-flood" => {
                            let mut err = std::io::stderr().lock();
                            for i in 0..20_000 {
                                let _ = writeln!(
                                    err,
                                    "{{\"level\":\"info\",\"message\":\"flood {i}\"}}"
                                );
                            }
                        }
                        "print" => {
                            // A stray print on the process's stdout while the protocol runs on
                            // its own channel.
                            println!("hello from a careless library");
                        }
                        _ => {}
                    }
                }
                if write_frame(&mut output, reply, &result.encode()).is_err() {
                    return;
                }
            }
            _ => {}
        }
    }
}

fn v1(input: Box<dyn Read>, mut output: Box<dyn Write>, a: &Args) {
    let analyzer = std::env::var_os("MVF_ARENA_PATH").is_some();
    let _ = writeln!(
        output,
        "{{\"type\":\"hello\",\"protocol\":1,\"moduleId\":\"fixture.echo\",\"capability\":\"{}\"}}",
        if analyzer { "analyzer" } else { "processor" }
    );
    let _ = output.flush();
    for line in BufReader::new(input).lines() {
        let Ok(line) = line else { return };
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        match msg["type"].as_str() {
            Some("shutdown") => return,
            Some("execute") => {
                spin(a.work_us);
                let reply = if analyzer {
                    serde_json::json!({"type": "result", "id": msg["id"], "value": {"sequence": msg["frame"]["sequence"]}})
                } else {
                    serde_json::json!({"type": "result", "id": msg["id"], "frame": {"shm": {"offset": msg["out"]["offset"]}}})
                };
                let _ = writeln!(output, "{reply}");
                let _ = output.flush();
            }
            _ => {}
        }
    }
}
