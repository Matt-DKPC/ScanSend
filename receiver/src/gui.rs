//! The desktop window: connect requests (Accept / Decline), settings, paired phones, scan history.

use crate::actions::{SharedOpts, Suffix};
use crate::backend::{self, Status};
use crate::config::Config;
use crate::net::{self, Firewall};
use crate::server::AppState;
use eframe::egui::{self, Color32, RichText};
use std::net::IpAddr;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

// The phone app's default theme (Forest Green), so both sides look like one product.
const ACCENT: Color32 = Color32::from_rgb(61, 220, 151); // text highlights
const FILL: Color32 = Color32::from_rgb(47, 158, 98); // buttons, borders
const INK: Color32 = Color32::from_rgb(3, 21, 11); // text on FILL
const BG: Color32 = Color32::from_rgb(10, 18, 14);
const PANEL: Color32 = Color32::from_rgb(17, 28, 22);
const LINE: Color32 = Color32::from_rgb(33, 51, 41);
const TEXT: Color32 = Color32::from_rgb(230, 239, 233);
const DIM: Color32 = Color32::from_rgb(134, 154, 142);

fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = PANEL;
    v.faint_bg_color = PANEL;
    v.extreme_bg_color = Color32::from_rgb(6, 12, 9);
    v.window_stroke = egui::Stroke::new(1.0, LINE);
    v.window_corner_radius = egui::CornerRadius::same(16);
    v.selection.bg_fill = FILL;
    v.hyperlink_color = ACCENT;
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, LINE);
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
        w.corner_radius = egui::CornerRadius::same(8);
    }
    v
}

/// A rounded panel like the phone app's cards, with a small all-caps title.
fn card<R>(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(PANEL)
        .stroke(egui::Stroke::new(1.0, LINE))
        .corner_radius(12)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title.to_uppercase()).small().color(DIM));
            ui.add_space(6.0);
            add(ui)
        })
        .inner
}

fn primary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).size(16.0).strong().color(INK))
        .fill(FILL)
        .min_size(egui::vec2(120.0, 38.0))
}

/// "#57": the last part of this computer's address, matching the tag phones show.
fn short_tag(ips: &[IpAddr]) -> String {
    match ips.first() {
        Some(IpAddr::V4(v4)) => format!("#{}", v4.octets()[3]),
        _ => String::new(),
    }
}

pub fn run(name: String, cfg: Config, opts: SharedOpts, port: u16) -> Result<(), String> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(format!("ScanSend · {name}"))
        .with_inner_size([860.0, 600.0])
        .with_min_inner_size([640.0, 460.0]);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png")) {
        viewport = viewport.with_icon(icon);
    }
    let native = eframe::NativeOptions { viewport, ..Default::default() };

    eframe::run_native(
        "ScanSend",
        native,
        Box::new(move |cc| {
            cc.egui_ctx.set_theme(egui::Theme::Dark);
            cc.egui_ctx.set_visuals_of(egui::Theme::Dark, visuals());
            let ctx = cc.egui_ctx.clone();
            let ask_ctx = cc.egui_ctx.clone();
            let state = crate::make_state(
                name,
                cfg,
                opts.clone(),
                // A phone wants to connect: flash the taskbar / bounce the dock icon.
                Box::new(move |_| {
                    ask_ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                        egui::UserAttentionType::Critical,
                    ));
                    ask_ctx.request_repaint();
                }),
                Box::new(|_| {}),
                Box::new(move || ctx.request_repaint()),
            );
            backend::spawn(state.clone(), port);
            Ok(Box::new(ReceiverApp::new(state, opts, port, &cc.egui_ctx)))
        }),
    )
    .map_err(|e| e.to_string())
}

struct ReceiverApp {
    state: Arc<AppState>,
    opts: SharedOpts,
    port: u16,
    ips: Vec<IpAddr>,
    ips_checked: Instant,
    qr: Option<(String, egui::TextureHandle)>,
    copied_at: Option<(usize, Instant)>,
    firewall: Option<Firewall>,
    fw_copied: Option<Instant>,
    /// An admin prompt to open the ports is waiting for the user.
    fw_job: Option<mpsc::Receiver<Result<(), String>>>,
    fw_result: Option<Result<(), String>>,
    start_at_login: bool,
    startup_err: Option<String>,
}

impl ReceiverApp {
    fn new(state: Arc<AppState>, opts: SharedOpts, port: u16, ctx: &egui::Context) -> Self {
        let firewall = net::detect_firewall();
        let mut app = ReceiverApp {
            state,
            opts,
            port,
            ips: net::lan_ips(),
            ips_checked: Instant::now(),
            qr: None,
            copied_at: None,
            firewall,
            fw_copied: None,
            fw_job: None,
            fw_result: None,
            start_at_login: crate::startup::is_enabled(),
            startup_err: None,
        };
        // First launch: ask once for admin rights to open the ports, so the
        // phone can get through without the user touching a terminal.
        if let Some(fw) = firewall {
            if fw.ports_open(port) {
                app.fw_result = Some(Ok(()));
            } else if !app.state.cfg.lock().unwrap().firewall_asked {
                app.open_ports(ctx);
            }
        }
        app
    }

    fn open_ports(&mut self, ctx: &egui::Context) {
        let Some(fw) = self.firewall else { return };
        let (tx, rx) = mpsc::channel();
        let (port, ctx) = (self.port, ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(fw.open_ports(port));
            ctx.request_repaint();
        });
        self.fw_job = Some(rx);
        self.fw_result = None;
        let mut cfg = self.state.cfg.lock().unwrap();
        cfg.firewall_asked = true;
        if let Err(e) = cfg.save() {
            eprintln!("could not save settings: {e:#}");
        }
    }

    fn qr_texture(&mut self, ctx: &egui::Context, uri: &str) -> Option<egui::TextureHandle> {
        if self.qr.as_ref().map(|(u, _)| u.as_str()) != Some(uri) {
            let code = qrcode::QrCode::new(uri.as_bytes()).ok()?;
            let w = code.width();
            let quiet = 3;
            let size = w + quiet * 2;
            let mut px = vec![255u8; size * size];
            for (i, c) in code.to_colors().iter().enumerate() {
                if *c == qrcode::Color::Dark {
                    let (x, y) = (i % w + quiet, i / w + quiet);
                    px[y * size + x] = 0;
                }
            }
            let img = egui::ColorImage::from_gray([size, size], &px);
            let tex = ctx.load_texture("pair-qr", img, egui::TextureOptions::NEAREST);
            self.qr = Some((uri.to_string(), tex));
        }
        self.qr.as_ref().map(|(_, t)| t.clone())
    }

    fn save_opts(&self) {
        let o = *self.opts.lock().unwrap();
        let mut cfg = self.state.cfg.lock().unwrap();
        cfg.options = o;
        if let Err(e) = cfg.save() {
            eprintln!("could not save settings: {e:#}");
        }
    }

    /// The computer's name, details and status, across the top of the window.
    fn header(&self, ui: &mut egui::Ui, status: &Status) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&self.state.name).size(28.0).strong().color(TEXT));
                    ui.label(RichText::new(short_tag(&self.ips)).size(20.0).color(DIM));
                });
                let mut sub = vec![net::device_name(), scansend_proto::os_label().to_string()];
                if let Some(ip) = self.ips.first() {
                    sub.push(ip.to_string());
                }
                sub.push(format!("v{}", env!("CARGO_PKG_VERSION")));
                ui.label(RichText::new(sub.join("  ·  ")).size(12.5).color(DIM));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (dot, text, hover) = match status {
                    Status::Starting => (DIM, "Starting…", String::new()),
                    Status::Listening { discovery_ok: true } => {
                        (ACCENT, "Ready", "Phones on this Wi-Fi can find this computer.".to_string())
                    }
                    Status::Listening { discovery_ok: false } => (
                        Color32::YELLOW,
                        "Ready",
                        "Wi-Fi announcements are blocked, so phones may take a few seconds longer to find this computer."
                            .to_string(),
                    ),
                    Status::Failed(msg) => (Color32::LIGHT_RED, "Not receiving", msg.clone()),
                };
                let pill = egui::Frame::new()
                    .fill(PANEL)
                    .stroke(egui::Stroke::new(1.0, LINE))
                    .corner_radius(255)
                    .inner_margin(egui::Margin::symmetric(12, 6))
                    .show(ui, |ui| {
                        // Inside the right-to-left header, so the label goes first to put the dot on its left.
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(text).color(TEXT));
                            let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                            ui.painter().circle_filled(r.center(), 4.5, dot);
                        });
                    });
                if !hover.is_empty() {
                    pill.response.on_hover_text(hover);
                }
            });
        });
    }

    fn pairing_panel(&mut self, ui: &mut egui::Ui) {
        if !crate::SHOW_QR_PAIRING {
            card(ui, "Connect a phone", |ui| {
                ui.label("Open ScanSend on your phone and tap");
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("💻 {}", self.state.name)).size(18.0).strong().color(ACCENT));
                    ui.label(RichText::new(short_tag(&self.ips)).size(15.0).color(DIM));
                });
                ui.add_space(2.0);
                ui.label(RichText::new("You'll be asked here to accept it.").color(DIM));
                self.firewall_hint(ui);
            });
            return;
        }
        ui.heading("Pair a phone");
        ui.add_space(4.0);
        let pin = self.state.current_pin();
        match self.ips.first().copied() {
            Some(ip) => {
                let uri = net::pair_uri(&self.ips, self.port, &pin, &self.state.name);
                if let Some(tex) = self.qr_texture(ui.ctx(), &uri) {
                    ui.add(egui::Image::new(&tex).fit_to_exact_size(egui::vec2(220.0, 220.0)));
                }
                ui.label("Scan this with the ScanSend app,");
                ui.label("or tap this computer in the app and enter:");
                ui.label(RichText::new(format!("{} {}", &pin[..3], &pin[3..])).monospace().size(34.0).strong());
                ui.label(RichText::new(format!("{ip}:{}", self.port)).weak());
                if self.ips.len() > 1 {
                    let others: Vec<String> = self.ips.iter().skip(1).map(|i| i.to_string()).collect();
                    ui.label(RichText::new(format!("also on {}", others.join(", "))).weak().small());
                }
            }
            None => {
                ui.label(RichText::new("No network connection found.").color(Color32::LIGHT_RED));
                ui.label(format!("PIN: {pin}"));
            }
        }
        if ui.button("New PIN").on_hover_text("The PIN changes automatically after each pairing").clicked() {
            self.state.set_new_pin();
        }
        self.firewall_hint(ui);
    }

    /// Until a phone has got through, explain how to open the firewall.
    fn firewall_hint(&mut self, ui: &mut egui::Ui) {
        let Some(fw) = self.firewall else { return };
        if let Some(Ok(r)) = self.fw_job.as_ref().map(|rx| rx.try_recv()) {
            self.fw_result = Some(r);
            self.fw_job = None;
        }
        if self.state.phone_seen.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        ui.add_space(8.0);
        if let Some(Ok(())) = self.fw_result {
            ui.label(RichText::new(format!("✓ {} allows ScanSend", fw.name())).color(ACCENT));
            ui.label(RichText::new("Phone still can't connect? Make sure it's on the same Wi-Fi, not mobile data.").weak().small());
            return;
        }
        egui::Frame::group(ui.style()).stroke(egui::Stroke::new(1.0, Color32::from_rgb(200, 160, 40))).show(ui, |ui| {
            ui.label(RichText::new("Phone says it can't reach this computer?").strong());
            if self.fw_job.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Waiting for you to allow ScanSend through the firewall…");
                });
                return;
            }
            ui.label(format!("{} may be blocking it.", fw.name()));
            if ui.button("Open ports").on_hover_text("Asks for your password once").clicked() {
                self.open_ports(ui.ctx());
            }
            if let Some(Err(e)) = &self.fw_result {
                ui.label(RichText::new(e).color(Color32::LIGHT_RED).small());
            }
            ui.add_space(4.0);
            ui.label(RichText::new("Or run this yourself:").weak().small());
            let cmd = fw.open_command(self.port);
            ui.add(egui::Label::new(RichText::new(&cmd).monospace().small()).wrap().selectable(true));
            let copied = self.fw_copied.is_some_and(|t| t.elapsed() < Duration::from_millis(1500));
            if ui.button(if copied { "Copied ✓" } else { "Copy command" }).clicked() {
                ui.ctx().copy_text(cmd.clone());
                self.fw_copied = Some(Instant::now());
                ui.ctx().request_repaint_after(Duration::from_millis(1600));
            }
            ui.label(RichText::new(fw.how_to_run()).weak().small());
            ui.label(RichText::new("Also make sure the phone is on the same Wi-Fi, not mobile data.").weak().small());
        });
    }

    /// "📱 X wants to connect" with Accept / Decline, one at a time, oldest first.
    fn ask_window(&mut self, ctx: &egui::Context) {
        let Some(a) = self.state.asking().into_iter().next() else { return };
        egui::Modal::new(egui::Id::new(("ask", &a.request_id)))
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .stroke(egui::Stroke::new(2.0, FILL))
                    .corner_radius(16)
                    .inner_margin(22),
            )
            .show(ctx, |ui| {
                ui.set_width(320.0);
                ui.label(RichText::new("WANTS TO CONNECT").small().color(DIM));
                ui.add_space(6.0);
                ui.label(RichText::new(format!("📱 {}", a.device_name)).size(24.0).strong().color(ACCENT));
                ui.label(RichText::new(format!("from {}", a.from)).color(DIM));
                ui.add_space(8.0);
                ui.label("Accept to let this phone send scans to this computer.");
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new(RichText::new("Decline").size(16.0)).min_size(egui::vec2(120.0, 38.0))).clicked() {
                        self.state.answer(&a.request_id, false);
                    }
                    if ui.add(primary_button("Accept")).clicked() {
                        self.state.answer(&a.request_id, true);
                    }
                });
            });
    }

    fn settings_panel(&mut self, ui: &mut egui::Ui) {
        card(ui, "When a scan arrives", |ui| self.scan_options(ui));
        ui.add_space(10.0);
        card(ui, "This computer", |ui| {
            let mut on = self.start_at_login;
            if ui.checkbox(&mut on, "Start ScanSend when I log in").changed() {
                match crate::startup::set(on) {
                    Ok(()) => {
                        self.start_at_login = on;
                        self.startup_err = None;
                    }
                    Err(e) => self.startup_err = Some(e),
                }
            }
            if let Some(e) = &self.startup_err {
                ui.label(RichText::new(e).color(Color32::LIGHT_RED).small());
            }
        });
    }

    fn scan_options(&mut self, ui: &mut egui::Ui) {
        let mut o = *self.opts.lock().unwrap();
        let before = o;
        ui.checkbox(&mut o.type_text, "Type it into the active window");
        ui.add_enabled_ui(o.type_text, |ui| {
            ui.horizontal(|ui| {
                ui.label("   then:");
                egui::ComboBox::from_id_salt("suffix")
                    .selected_text(o.suffix.label())
                    .show_ui(ui, |ui| {
                        for s in [Suffix::Enter, Suffix::Tab, Suffix::None] {
                            ui.selectable_value(&mut o.suffix, s, s.label());
                        }
                    });
            });
        });
        ui.checkbox(&mut o.clipboard, "Copy it to the clipboard");
        if o != before {
            *self.opts.lock().unwrap() = o;
            self.save_opts();
        }
    }

    fn devices_panel(&mut self, ui: &mut egui::Ui) {
        let devices = self.state.cfg.lock().unwrap().devices.clone();
        card(ui, "Phones", |ui| {
            if devices.is_empty() {
                ui.label(RichText::new("None yet").color(DIM));
            }
            for d in devices {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("📱 {}", d.name)).size(15.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Unpair").on_hover_text("This phone will need to connect again").clicked() {
                            if let Err(e) = self.state.unpair(&d.token_sha256) {
                                eprintln!("unpair failed: {e:#}");
                            }
                        }
                    });
                });
            }
        });
    }

    fn scans_panel(&mut self, ui: &mut egui::Ui) {
        let log: Vec<_> = self.state.log.lock().unwrap().iter().cloned().collect();
        ui.horizontal(|ui| {
            ui.label(RichText::new("RECENT SCANS").small().color(DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !log.is_empty() && ui.button("Clear").clicked() {
                    self.state.log.lock().unwrap().clear();
                }
            });
        });
        ui.separator();
        if log.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("Nothing scanned yet").size(18.0).weak());
                ui.label(RichText::new("Click into a text box in any app, then tap Scan on your phone.").weak());
            });
            return;
        }
        let copied = self.copied_at.filter(|(_, t)| t.elapsed() < Duration::from_millis(1200));
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (i, ev) in log.iter().enumerate() {
                egui::Frame::new()
                    .fill(PANEL)
                    .stroke(egui::Stroke::new(1.0, LINE))
                    .corner_radius(10)
                    .inner_margin(10)
                    .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&ev.time).monospace().weak());
                        ui.label(RichText::new(&ev.format).small().color(ACCENT));
                        ui.label(RichText::new(format!("📱 {}", ev.device)).small().color(DIM));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let label = if copied.map(|(c, _)| c) == Some(i) { "Copied ✓" } else { "Copy" };
                            if ui.small_button(label).clicked() {
                                ui.ctx().copy_text(ev.text.clone());
                                self.copied_at = Some((i, Instant::now()));
                            }
                        });
                    });
                    ui.add(egui::Label::new(RichText::new(&ev.text).monospace().size(15.0)).wrap().selectable(true));
                });
                ui.add_space(6.0);
            }
        });
        if copied.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(300));
        }
    }
}

impl eframe::App for ReceiverApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // Pick up IP changes (Wi-Fi reconnect, DHCP) without a restart.
        if self.ips_checked.elapsed() > Duration::from_secs(10) {
            self.ips = net::lan_ips();
            self.ips_checked = Instant::now();
        }
        ctx.request_repaint_after(Duration::from_secs(10));

        let status = self.state.status.lock().unwrap().clone();
        egui::Panel::top("header")
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin { left: 18, right: 18, top: 16, bottom: 12 }))
            .show(ui, |ui| self.header(ui, &status));

        egui::Panel::left("left")
            .resizable(false)
            .exact_size(310.0)
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin { left: 18, right: 8, top: 4, bottom: 12 }))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if let Status::Failed(msg) = &status {
                        card(ui, "Not receiving scans", |ui| {
                            ui.label(RichText::new(msg).color(Color32::LIGHT_RED));
                        });
                    } else {
                        self.pairing_panel(ui);
                    }
                    ui.add_space(10.0);
                    self.devices_panel(ui);
                    ui.add_space(10.0);
                    self.settings_panel(ui);
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin { left: 10, right: 18, top: 4, bottom: 12 }))
            .show(ui, |ui| self.scans_panel(ui));
        self.ask_window(&ctx);
    }
}
