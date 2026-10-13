// No console window on Windows; terminal mode re-attaches to the parent console.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod actions;
mod backend;
mod config;
mod discovery;
mod gui;
mod net;
mod server;
mod startup;

use anyhow::Result;
use clap::Parser;
use qrcode::{render::unicode::Dense1x2, QrCode};
use scansend_proto::DEFAULT_HTTP_PORT;
use std::sync::{Arc, Mutex};

/// Show the old pairing QR code + PIN. Phones now connect by tapping this
/// computer's name (Accept / Decline here); flip this back on if that ever
/// doesn't work on some network. The phone app has the same switch.
pub const SHOW_QR_PAIRING: bool = false;

/// ScanSend receiver: scans from your phone get typed and/or copied here.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Run in the terminal instead of opening a window.
    #[arg(long)]
    terminal: bool,
    /// Name your phone will see (default: a random name like "Flaky USB", kept between runs).
    #[arg(long)]
    name: Option<String>,
    /// HTTP port for pairing and scans.
    #[arg(long, default_value_t = DEFAULT_HTTP_PORT)]
    port: u16,
    /// Don't type scans into the active window (saved for next time).
    #[arg(long)]
    no_type: bool,
    /// Don't copy scans to the clipboard (saved for next time).
    #[arg(long)]
    no_clipboard: bool,
    /// Key pressed after typing a scan (saved for next time).
    #[arg(long, value_enum)]
    suffix: Option<actions::Suffix>,
    /// List paired phones and exit.
    #[arg(long)]
    list: bool,
    /// Unpair every phone and exit.
    #[arg(long)]
    unpair_all: bool,
}

fn print_pairing(name: &str, port: u16, pin: &str) {
    if !SHOW_QR_PAIRING {
        return;
    }
    let ips = net::lan_ips();
    let Some(primary) = ips.first() else {
        println!("  No network connection found. Pairing PIN: {pin}");
        return;
    };
    if let Ok(code) = QrCode::new(net::pair_uri(&ips, port, pin, name).as_bytes()) {
        let art = code
            .render::<Dense1x2>()
            .dark_color(Dense1x2::Light)
            .light_color(Dense1x2::Dark)
            .quiet_zone(true)
            .build();
        println!();
        for line in art.lines() {
            println!("    {line}");
        }
    }
    println!("\n  To pair a phone: open ScanSend and scan this code,");
    println!("  or tap \"{name}\" in the app and enter PIN  {pin}");
    let others: Vec<String> = ips.iter().skip(1).map(|ip| ip.to_string()).collect();
    if others.is_empty() {
        println!("  (address {primary}:{port})\n");
    } else {
        println!("  (address {primary}:{port}; also on {})\n", others.join(", "));
    }
}

#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    // Lets `scansend --terminal` print into the cmd/PowerShell window it was started from.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
#[cfg(not(windows))]
fn attach_console() {}

fn make_state(
    name: String,
    cfg: config::Config,
    opts: actions::SharedOpts,
    on_ask: Box<dyn Fn(&server::Asking) + Send + Sync>,
    on_new_pin: Box<dyn Fn(&str) + Send + Sync>,
    on_change: Box<dyn Fn() + Send + Sync>,
) -> Arc<server::AppState> {
    Arc::new(server::AppState {
        name,
        cfg: Mutex::new(cfg),
        pin: Mutex::new(server::PinState { pin: server::new_pin(), failures: 0 }),
        recent_ids: Mutex::new(Default::default()),
        actions: Mutex::new(actions::spawn(opts)),
        status: Mutex::new(backend::Status::Starting),
        log: Mutex::new(Default::default()),
        phone_seen: Default::default(),
        asking: Mutex::new(Vec::new()),
        on_ask,
        on_new_pin,
        on_change,
    })
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.terminal || args.list || args.unpair_all {
        attach_console();
    }

    let mut cfg = config::Config::load_or_create()?;

    if args.list || args.unpair_all {
        if args.unpair_all {
            let n = cfg.devices.len();
            cfg.devices.clear();
            cfg.save()?;
            println!("Unpaired {n} phone(s).");
        } else if cfg.devices.is_empty() {
            println!("No phones paired yet.");
        } else {
            for d in &cfg.devices {
                println!("{}", d.name);
            }
        }
        println!("Settings file: {}", config::path().display());
        return Ok(());
    }

    // Command-line switches override (and update) the saved settings.
    if args.no_type {
        cfg.options.type_text = false;
    }
    if args.no_clipboard {
        cfg.options.clipboard = false;
    }
    if let Some(s) = args.suffix {
        cfg.options.suffix = s;
    }
    let _ = cfg.save();

    let name = match args.name.clone().or_else(|| cfg.name.clone()) {
        Some(n) => n,
        None => {
            // Never the same as a paired phone's name, so it's clear which side is which.
            let mut n = scansend_proto::funny_name(rand::random());
            while cfg.devices.iter().any(|d| d.name.eq_ignore_ascii_case(&n)) {
                n = scansend_proto::funny_name(rand::random());
            }
            cfg.name = Some(n.clone());
            let _ = cfg.save();
            n
        }
    };
    let opts: actions::SharedOpts = Arc::new(Mutex::new(cfg.options));

    if !args.terminal {
        match gui::run(name.clone(), cfg.clone(), opts.clone(), args.port) {
            Ok(()) => return Ok(()),
            // No display (SSH session, headless box): fall back to terminal mode.
            Err(e) => eprintln!("Couldn't open a window ({e}); running in the terminal instead.\n"),
        }
    }
    run_terminal(name, cfg, opts, args.port)
}

fn run_terminal(name: String, cfg: config::Config, opts: actions::SharedOpts, port: u16) -> Result<()> {
    let o = *opts.lock().unwrap();
    println!("ScanSend receiver \"{name}\"");
    println!(
        "  Each scan will be: {}",
        match (o.type_text, o.clipboard) {
            (true, true) => "typed into the active window + copied to clipboard",
            (true, false) => "typed into the active window",
            (false, true) => "copied to clipboard",
            (false, false) => "printed here only",
        }
    );
    println!("  Phones already paired: {}", cfg.devices.len());

    let state = make_state(
        name.clone(),
        cfg,
        opts,
        Box::new(|a| {
            println!("\n  📱 \"{}\" ({}) wants to connect. Type y + Enter to accept, n to decline.", a.device_name, a.from);
        }),
        Box::new({
            let name = name.clone();
            move |pin| print_pairing(&name, port, pin)
        }),
        Box::new(|| {}),
    );
    print_pairing(&name, port, &state.current_pin());
    if !SHOW_QR_PAIRING {
        println!("\n  To connect a phone: open ScanSend on it and tap 💻 {name}.");
        println!("  You'll be asked here to accept it.\n");
    }
    // Answers "wants to connect" questions, oldest first.
    std::thread::spawn({
        let state = state.clone();
        move || {
            for line in std::io::stdin().lines() {
                let Ok(line) = line else { break };
                let accept = match line.trim().to_ascii_lowercase().as_str() {
                    "y" | "yes" => true,
                    "n" | "no" => false,
                    _ => continue,
                };
                match state.asking().into_iter().next() {
                    Some(a) => state.answer(&a.request_id, accept),
                    None => println!("  (No phone is waiting.)"),
                }
            }
        }
    });
    if let Some(fw) = net::detect_firewall().filter(|f| *f != net::Firewall::Windows) {
        println!("  {} is active. If your phone says it can't reach this computer, run:", fw.name());
        println!("    {}\n", fw.open_command(port));
    }
    println!("  Waiting for scans. Press Ctrl+C to stop.\n");

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        tokio::select! {
            _ = backend::run(state.clone(), port) => {}
            _ = tokio::signal::ctrl_c() => return,
        }
        // run() only returns on failure.
        if let backend::Status::Failed(msg) = &*state.status.lock().unwrap() {
            eprintln!("  ! {msg}");
        }
    });
    Ok(())
}
