//! Command-line sender, for testing without the menu bar app.
//!
//! ```text
//! annex-mac-cli probe
//! annex-mac-cli run [--addr HOST[:PORT]] [--pin PIN] [--enable 0,1] [--seconds N]
//! ```

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("annex-mac-cli runs on macOS only");
}

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    use std::net::{SocketAddr, ToSocketAddrs};
    use std::time::{Duration, Instant};

    use annex_sender::{Engine, EngineOptions, PairingState};

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_env("ANNEX_LOG").unwrap_or_else(|_| "info".into()))
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opt = |name: &str| -> Option<String> { args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned() };
    match args.first().map(String::as_str) {
        Some("probe") => {
            println!("machine: {}", annex_mac::machine_name());
            println!("computer name: {}", annex_mac::host_name());
            match annex_mac::vdisplay::probe() {
                Ok(()) => println!("virtual display API: ok"),
                Err(e) => println!("virtual display API: {e:#}"),
            }
            let granted = annex_mac::has_screen_recording() || annex_mac::request_screen_recording();
            println!("screen recording permission: {}", if granted { "granted" } else { "not granted" });
            for d in annex_mac::displays::list() {
                println!("display: {} {}x{} {}", d.name, d.width, d.height, d.uuid);
            }
            Ok(())
        }
        Some("run") => {
            if !annex_mac::has_screen_recording() {
                annex_mac::request_screen_recording();
            }
            let mut o = EngineOptions::new(annex_mac::config_path(), annex_mac::host_name());
            if let Some(a) = opt("--addr") {
                let a = if a.contains(':') { a } else { format!("{a}:{}", annex_proto::DEFAULT_CONTROL_PORT) };
                let addr: SocketAddr = a.to_socket_addrs()?.find(|a| a.is_ipv4()).ok_or_else(|| anyhow::anyhow!("bad --addr"))?;
                o.manual_receiver = Some(addr);
            }
            let backend = annex_mac::MacBackend::new()?;
            let engine = Engine::start(backend, o)?;
            let seconds: Option<u64> = opt("--seconds").map(|s| s.parse()).transpose()?;
            let enable: Vec<usize> = opt("--enable")
                .map(|v| v.split(',').map(|i| i.trim().parse()).collect::<Result<_, _>>())
                .transpose()?
                .unwrap_or_default();
            let pin = opt("--pin");
            // The main thread services the main dispatch queue; work runs here.
            std::thread::spawn(move || {
                if let Some(pin) = pin {
                    std::thread::sleep(Duration::from_secs(2));
                    engine.pair(None, pin);
                    loop {
                        std::thread::sleep(Duration::from_millis(200));
                        match engine.status().pairing {
                            PairingState::Done(n) => {
                                println!("paired with {n}");
                                break;
                            }
                            PairingState::Failed(e) => {
                                eprintln!("pairing failed: {e}");
                                std::process::exit(1);
                            }
                            _ => {}
                        }
                    }
                }
                if !enable.is_empty() {
                    let wait = Instant::now() + Duration::from_secs(20);
                    while engine.status().monitors.is_empty() && Instant::now() < wait {
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    let monitors = engine.status().monitors;
                    for &i in &enable {
                        if let Some(m) = monitors.get(i) {
                            engine.set_monitor_prefs(m.id, Some(true), None, None);
                        }
                    }
                }
                let until = seconds.map(|s| Instant::now() + Duration::from_secs(s));
                let mut last = String::new();
                while until.is_none_or(|u| Instant::now() < u) {
                    std::thread::sleep(Duration::from_secs(2));
                    let s = engine.status();
                    let mut line = format!("{:?}", s.state);
                    for st in &s.streams {
                        line += &format!(
                            " | #{} {} {}x{} {} live={} sent={:.0}fps shown={:.0}fps enc={:.1}ms loss={:.2}% e2e={}",
                            st.stream_id,
                            st.monitor_name,
                            st.width,
                            st.height,
                            st.mode.as_str(),
                            st.live,
                            st.fps_sent,
                            st.fps_presented,
                            st.encode_ms,
                            st.loss_percent,
                            st.e2e_ms.map(|v| format!("{v:.1}ms")).unwrap_or("-".into())
                        );
                    }
                    if let Some(m) = &s.message {
                        line += &format!(" | {m}");
                    }
                    if line != last {
                        println!("{line}");
                        last = line;
                    }
                }
                engine.shutdown();
                std::process::exit(0);
            });
            dispatch2::DispatchQueue::main().exec_async(|| {});
            dispatch_main();
        }
        _ => {
            eprintln!("usage: annex-mac-cli probe | run [--addr HOST[:PORT]] [--pin PIN] [--enable 0,1] [--seconds N]");
            std::process::exit(2)
        }
    }
}

#[cfg(target_os = "macos")]
fn dispatch_main() -> ! {
    unsafe extern "C" {
        #[link_name = "dispatch_main"]
        fn dm() -> !;
    }
    unsafe { dm() }
}
