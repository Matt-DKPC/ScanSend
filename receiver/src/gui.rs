//! The desktop window: connect requests (Accept / Decline), settings, paired phones, scan history.

use crate::actions::{SharedOpts, Suffix};
use crate::backend::{self, Status};
use crate::config::Config;
use crate::net::{self, Firewall};
use crate::server::AppState;
use eframe::egui::{self, Color32, RichText};
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// The phone app's themes, with the same names and colors, so both sides look like one product.
#[derive(Clone, Copy)]
struct Theme {
    key: &'static str,
    name: &'static str,
    bg: Color32,
    panel: Color32,
    line: Color32,
    text: Color32,
    dim: Color32,
    /// Highlights, buttons and borders.
    accent: Color32,
    /// Text on an accent-filled button.
    ink: Color32,
}

const fn hex(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

#[rustfmt::skip]
const THEMES: [Theme; 7] = [
    Theme { key: "default", name: "Forest Green", bg: hex(0x0a120e), panel: hex(0x111c16), line: hex(0x213329), text: hex(0xe6efe9), dim: hex(0x869a8e), accent: hex(0x2f9e62), ink: hex(0x03150b) },
    Theme { key: "david", name: "David Green", bg: hex(0x0d1210), panel: hex(0x151c19), line: hex(0x26332d), text: hex(0xe8efeb), dim: hex(0x8b9a93), accent: hex(0x2ef27a), ink: hex(0x032010) },
    Theme { key: "wayland", name: "Wayland Blue", bg: hex(0x0c1118), panel: hex(0x141b25), line: hex(0x253244), text: hex(0xe8eef5), dim: hex(0x8a97a8), accent: hex(0x3b9cff), ink: hex(0x03152b) },
    Theme { key: "grape", name: "Grape Purple", bg: hex(0x110d18), panel: hex(0x1a1424), line: hex(0x2e2440), text: hex(0xeee8f5), dim: hex(0x9a8fab), accent: hex(0xa77bff), ink: hex(0x14062b) },
    Theme { key: "ember", name: "Ember Orange", bg: hex(0x140f0a), panel: hex(0x1e1610), line: hex(0x35281c), text: hex(0xf3ece5), dim: hex(0xa89584), accent: hex(0xff8a3d), ink: hex(0x2a1102) },
    Theme { key: "cherry", name: "Cherry Red", bg: hex(0x150b0d), panel: hex(0x201215), line: hex(0x3a2226), text: hex(0xf4e8ea), dim: hex(0xa88d92), accent: hex(0xff4d6a), ink: hex(0x2b0309) },
    Theme { key: "gold", name: "Golden Yellow", bg: hex(0x13110a), panel: hex(0x1d1a10), line: hex(0x353020), text: hex(0xf2efe4), dim: hex(0xa39d86), accent: hex(0xf5c542), ink: hex(0x2a1f00) },
];

/// The chosen theme (an index into THEMES), read wherever the window paints.
static THEME: AtomicUsize = AtomicUsize::new(0);

fn t() -> &'static Theme {
    &THEMES[THEME.load(Ordering::Relaxed)]
}

fn theme_index(key: Option<&str>) -> usize {
    key.and_then(|k| THEMES.iter().position(|t| t.key == k)).unwrap_or(0)
}

fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.panel_fill = t().bg;
    v.window_fill = t().panel;
    v.faint_bg_color = t().panel;
    let [r, g, b, _] = t().bg.to_array();
    v.extreme_bg_color = Color32::from_rgb(r * 3 / 5, g * 3 / 5, b * 3 / 5);
    v.window_stroke = egui::Stroke::new(1.0, t().line);
    v.window_corner_radius = egui::CornerRadius::same(16);
    v.selection.bg_fill = t().accent;
    v.hyperlink_color = t().accent;
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, t().line);
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, t().text);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
        w.corner_radius = egui::CornerRadius::same(8);
    }
    v
}

/// A rounded panel like the phone app's cards, with a small all-caps title.
fn card<R>(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(t().panel)
        .stroke(egui::Stroke::new(1.0, t().line))
        .corner_radius(12)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title.to_uppercase()).small().color(t().dim));
            ui.add_space(6.0);
            add(ui)
        })
        .inner
}

fn primary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).size(16.0).strong().color(t().ink))
        .fill(t().accent)
        .min_size(egui::vec2(120.0, 38.0))
}

/// The settings button: a gear drawn to fill the button, like the phone app's.
fn gear_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::click());
    let th = t();
    let fill = if resp.hovered() { th.line } else { th.panel };
    let p = ui.painter();
    p.rect(rect, 10, fill, egui::Stroke::new(1.0, th.line), egui::StrokeKind::Inside);
    let c = rect.center();
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::TAU / 8.0;
        let (d, n) = (egui::vec2(a.cos(), a.sin()), egui::vec2(-a.sin(), a.cos()));
        let tooth = vec![c + d * 6.5 + n * 2.6, c + d * 11.0 + n * 2.0, c + d * 11.0 - n * 2.0, c + d * 6.5 - n * 2.6];
        p.add(egui::Shape::convex_polygon(tooth, th.text, egui::Stroke::NONE));
    }
    p.circle_filled(c, 8.0, th.text);
    p.circle_filled(c, 3.4, fill);
    resp.on_hover_text("Settings")
}

/// A theme choice drawn in that theme's own colors, so you can see it before picking it.
fn theme_tile(ui: &mut egui::Ui, theme: &Theme, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(132.0, 70.0), egui::Sense::click());
    let p = ui.painter();
    let stroke = if selected {
        egui::Stroke::new(2.0, theme.accent)
    } else {
        egui::Stroke::new(1.0, if resp.hovered() { theme.accent } else { theme.line })
    };
    p.rect(rect, 12, theme.bg, stroke, egui::StrokeKind::Inside);
    let pill = egui::Rect::from_center_size(rect.center() - egui::vec2(0.0, 13.0), egui::vec2(44.0, 14.0));
    p.rect_filled(pill, 7, theme.accent);
    p.text(
        rect.center() + egui::vec2(0.0, 14.0),
        egui::Align2::CENTER_CENTER,
        theme.name,
        egui::FontId::proportional(14.0),
        theme.accent,
    );
    resp.on_hover_text(theme.name)
}

/// Use the computer's own UI font (Segoe UI on Windows, like the phone app uses its system
/// font), with a bold face for headings. Falls back to egui's font if none is found.
fn install_fonts(ctx: &egui::Context) {
    const PAIRS: &[(&str, &str)] = &[
        ("C:\\Windows\\Fonts\\segoeui.ttf", "C:\\Windows\\Fonts\\segoeuib.ttf"),
        ("/System/Library/Fonts/Supplemental/Arial.ttf", "/System/Library/Fonts/Supplemental/Arial Bold.ttf"),
        ("/usr/share/fonts/noto/NotoSans-Regular.ttf", "/usr/share/fonts/noto/NotoSans-Bold.ttf"),
        ("/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf", "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf"),
        ("/usr/share/fonts/google-noto/NotoSans-Regular.ttf", "/usr/share/fonts/google-noto/NotoSans-Bold.ttf"),
        ("/usr/share/fonts/TTF/DejaVuSans.ttf", "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf"),
        ("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"),
    ];
    let mut fonts = egui::FontDefinitions::default();
    let found = PAIRS.iter().find_map(|(r, b)| Some((std::fs::read(r).ok()?, std::fs::read(b).ok()?)));
    let have_ui_font = found.is_some();
    if let Some((regular, bold)) = found {
        fonts.font_data.insert("ui".into(), Arc::new(egui::FontData::from_owned(regular)));
        fonts.font_data.insert("ui-bold".into(), Arc::new(egui::FontData::from_owned(bold)));
        fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "ui".into());
    }
    let mut bold = fonts.families.get(&egui::FontFamily::Proportional).cloned().unwrap_or_default();
    if have_ui_font {
        bold.insert(0, "ui-bold".into());
    }
    fonts.families.insert(egui::FontFamily::Name("bold".into()), bold);
    ctx.set_fonts(fonts);
}

/// Bold text in the UI font (egui's .strong() only brightens the color).
fn bold(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).family(egui::FontFamily::Name("bold".into())).size(size)
}

/// An all-caps section label, like the phone app's "NEARBY COMPUTERS".
fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text.to_uppercase()).size(12.5).color(t().dim).extra_letter_spacing(0.8));
}

/// The rounded, accent-tinted icon box the phone uses on its cards, with a phone inside.
fn phone_icon(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(36.0, 36.0), egui::Sense::hover());
    let th = t();
    let p = ui.painter();
    p.rect_filled(rect, 10, th.accent.gamma_multiply(0.16));
    let body = egui::Rect::from_center_size(rect.center(), egui::vec2(13.0, 21.0));
    p.rect_stroke(body, 3, egui::Stroke::new(1.8, th.text), egui::StrokeKind::Middle);
    let y = rect.center().y;
    p.line_segment([egui::pos2(body.left() + 3.0, y), egui::pos2(body.right() - 3.0, y)], egui::Stroke::new(2.2, th.accent));
}

/// The phone app's "sent" check mark, drawn so it looks the same in every font.
fn check_mark(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
    let c = rect.center();
    let pts = [c + egui::vec2(-6.0, 0.5), c + egui::vec2(-2.0, 4.5), c + egui::vec2(6.0, -4.5)];
    ui.painter().add(egui::Shape::line(pts.to_vec(), egui::Stroke::new(2.4, t().accent)));
}

/// A card like the phone app's: rounded, filled, thin border.
fn row_card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(t().panel)
        .stroke(egui::Stroke::new(1.0, t().line))
        .corner_radius(12)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
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
            THEME.store(theme_index(cfg.theme.as_deref()), Ordering::Relaxed);
            install_fonts(&cc.egui_ctx);
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
    settings_open: bool,
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
            settings_open: false,
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

    /// Like the phone app's header: the ScanSend wordmark, then status and the gear.
    fn top_bar(&mut self, ui: &mut egui::Ui, status: &Status) {
        ui.horizontal(|ui| {
            ui.label(bold("Scan", 22.0).color(t().text));
            ui.add_space(-ui.spacing().item_spacing.x);
            ui.label(bold("Send", 22.0).color(t().accent));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if gear_button(ui).clicked() {
                    self.settings_open = true;
                }
                ui.add_space(4.0);
                let (dot, text, hover) = match status {
                    Status::Starting => (t().dim, "Starting…", String::new()),
                    Status::Listening { discovery_ok: true } => {
                        (t().accent, "Ready", "Phones on this Wi-Fi can find this computer.".to_string())
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
                    .fill(t().panel)
                    .stroke(egui::Stroke::new(1.0, t().line))
                    .corner_radius(255)
                    .inner_margin(egui::Margin::symmetric(12, 6))
                    .show(ui, |ui| {
                        // Inside the right-to-left bar, so the label goes first to put the dot on its left.
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(text).color(t().text));
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

    /// Like the phone app's "📱 Sleepy Dongle / This phone · IP": this computer's name, big and centered.
    fn hero(&mut self, ui: &mut egui::Ui, status: &Status) {
        ui.vertical_centered(|ui| {
            ui.horizontal(|ui| {
                // Center the name + tag as one line.
                let name = bold(format!("💻 {}", self.state.name), 30.0).color(t().accent);
                let tag = RichText::new(format!(" {}", short_tag(&self.ips))).size(20.0).color(t().dim);
                let w = ui.fonts_mut(|f| {
                    let a = f.layout_no_wrap(name.text().to_string(), egui::FontId::new(30.0, egui::FontFamily::Name("bold".into())), t().accent).size().x;
                    let b = f.layout_no_wrap(tag.text().to_string(), egui::FontId::proportional(20.0), t().dim).size().x;
                    a + b
                });
                ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
                ui.label(name);
                ui.add_space(-ui.spacing().item_spacing.x);
                ui.label(tag);
            });
            let mut sub = vec!["This computer".to_string(), net::device_name(), scansend_proto::os_label().to_string()];
            if let Some(ip) = self.ips.first() {
                sub.push(ip.to_string());
            }
            sub.push(format!("v{}", env!("CARGO_PKG_VERSION")));
            ui.label(RichText::new(sub.join(" · ")).size(13.0).color(t().dim));
            ui.add_space(8.0);
            if let Status::Failed(msg) = status {
                ui.label(RichText::new(msg).color(Color32::LIGHT_RED));
            } else if crate::SHOW_QR_PAIRING {
                self.pairing_panel(ui);
            } else {
                ui.label(RichText::new("Open ScanSend on your phone and tap this computer to connect.").color(t().dim));
                if let Some(ip) = self.ips.first() {
                    ui.label(RichText::new(format!("Not listed on the phone? Tap \"Add by address\" there and enter {ip}")).size(13.0).color(t().dim));
                }
                ui.set_max_width(460.0);
                self.firewall_hint(ui);
            }
        });
    }

    fn pairing_panel(&mut self, ui: &mut egui::Ui) {
        if !crate::SHOW_QR_PAIRING {
            card(ui, "Connect a phone", |ui| {
                ui.label("Open ScanSend on your phone and tap");
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("💻 {}", self.state.name)).size(18.0).strong().color(t().accent));
                    ui.label(RichText::new(short_tag(&self.ips)).size(15.0).color(t().dim));
                });
                ui.add_space(2.0);
                ui.label(RichText::new("You'll be asked here to accept it.").color(t().dim));
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
            ui.label(RichText::new(format!("✓ {} allows ScanSend", fw.name())).color(t().accent));
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
                    .fill(t().panel)
                    .stroke(egui::Stroke::new(2.0, t().accent))
                    .corner_radius(16)
                    .inner_margin(22),
            )
            .show(ctx, |ui| {
                ui.set_width(320.0);
                ui.label(RichText::new("WANTS TO CONNECT").small().color(t().dim));
                ui.add_space(6.0);
                ui.label(bold(format!("📱 {}", a.device_name), 24.0).color(t().accent));
                ui.label(RichText::new(format!("from {}", a.from)).color(t().dim));
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

    /// Settings, like the phone app's: what happens to a scan, this computer, themes.
    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let th = *t();
        let mut done = false;
        let modal = egui::Modal::new(egui::Id::new("settings"))
            .frame(
                egui::Frame::new()
                    .fill(th.panel)
                    .stroke(egui::Stroke::new(1.0, th.line))
                    .corner_radius(16)
                    .inner_margin(22),
            )
            .show(ctx, |ui| {
                ui.set_width(430.0);
                ui.label(bold("Settings", 22.0).color(th.text));
                let section = |ui: &mut egui::Ui, title: &str| {
                    ui.add_space(14.0);
                    ui.label(RichText::new(title).small().color(th.dim));
                    ui.add_space(4.0);
                };
                section(ui, "WHEN A SCAN ARRIVES");
                self.scan_options(ui);
                section(ui, "THIS COMPUTER");
                self.startup_option(ui);
                section(ui, "THEMES");
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
                    let current = THEME.load(Ordering::Relaxed);
                    for (i, theme) in THEMES.iter().enumerate() {
                        if theme_tile(ui, theme, i == current).clicked() {
                            self.set_theme(ui.ctx(), i);
                        }
                    }
                });
                ui.add_space(16.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(primary_button("Done")).clicked() {
                        done = true;
                    }
                });
            });
        if done || modal.should_close() {
            self.settings_open = false;
        }
    }

    fn set_theme(&mut self, ctx: &egui::Context, i: usize) {
        THEME.store(i, Ordering::Relaxed);
        ctx.set_visuals_of(egui::Theme::Dark, visuals());
        let mut cfg = self.state.cfg.lock().unwrap();
        cfg.theme = Some(THEMES[i].key.to_string());
        if let Err(e) = cfg.save() {
            eprintln!("could not save settings: {e:#}");
        }
    }

    fn startup_option(&mut self, ui: &mut egui::Ui) {
        {
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
        }
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

    /// The phones that can send here, as cards like the phone app's computer list.
    fn devices_panel(&mut self, ui: &mut egui::Ui) {
        let devices = self.state.cfg.lock().unwrap().devices.clone();
        section_label(ui, "Your phones");
        ui.add_space(4.0);
        if devices.is_empty() {
            row_card(ui, |ui| {
                ui.label(RichText::new("None yet").color(t().dim));
                ui.label(RichText::new(format!("On your phone, tap 💻 {}", self.state.name)).size(13.0).color(t().dim));
            });
        }
        for d in devices {
            row_card(ui, |ui| {
                ui.horizontal(|ui| {
                    phone_icon(ui);
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&d.name).size(16.0).color(t().text));
                        ui.label(RichText::new("Can send scans here").size(12.5).color(t().dim));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Unpair").on_hover_text("This phone will need to connect again").clicked() {
                            if let Err(e) = self.state.unpair(&d.token_sha256) {
                                eprintln!("unpair failed: {e:#}");
                            }
                        }
                    });
                });
            });
            ui.add_space(8.0);
        }
    }

    /// Recent scans, styled like the phone app's list: ✓, the code, a small line under it, Copy.
    fn scans_panel(&mut self, ui: &mut egui::Ui) {
        let log: Vec<_> = self.state.log.lock().unwrap().iter().cloned().collect();
        ui.horizontal(|ui| {
            section_label(ui, "Recent scans");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !log.is_empty() && ui.button("Clear").clicked() {
                    self.state.log.lock().unwrap().clear();
                }
            });
        });
        ui.add_space(4.0);
        if log.is_empty() {
            row_card(ui, |ui| {
                ui.add_space(14.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("Nothing scanned yet").size(17.0).color(t().dim));
                    ui.label(RichText::new("Click into a text box in any app, then scan with your phone.").color(t().dim));
                });
                ui.add_space(14.0);
            });
            return;
        }
        let copied = self.copied_at.filter(|(_, t)| t.elapsed() < Duration::from_millis(1200));
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (i, ev) in log.iter().enumerate() {
                row_card(ui, |ui| {
                    ui.horizontal(|ui| {
                        check_mark(ui);
                        ui.add_space(6.0);
                        ui.vertical(|ui| {
                            ui.set_max_width(ui.available_width() - 80.0);
                            ui.add(egui::Label::new(RichText::new(&ev.text).size(16.0).color(t().text)).wrap().selectable(true));
                            ui.label(RichText::new(format!("{} · {} · 📱 {}", ev.format, ev.time, ev.device)).size(12.5).color(t().dim));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let label = if copied.map(|(c, _)| c) == Some(i) { "Copied ✓" } else { "Copy" };
                            if ui.button(label).clicked() {
                                ui.ctx().copy_text(ev.text.clone());
                                self.copied_at = Some((i, Instant::now()));
                            }
                        });
                    });
                });
                ui.add_space(8.0);
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
        let pad = |l, r, top, b| egui::Frame::new().fill(t().bg).inner_margin(egui::Margin { left: l, right: r, top, bottom: b });
        egui::Panel::top("bar").show_separator_line(false).frame(pad(20, 20, 14, 4)).show(ui, |ui| self.top_bar(ui, &status));
        egui::Panel::top("hero").show_separator_line(false).frame(pad(20, 20, 6, 16)).show(ui, |ui| self.hero(ui, &status));
        egui::Panel::left("phones")
            .show_separator_line(false)
            .resizable(false)
            .exact_size(320.0)
            .frame(pad(20, 10, 0, 16))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.devices_panel(ui));
            });
        egui::CentralPanel::default().frame(pad(10, 20, 0, 16)).show(ui, |ui| self.scans_panel(ui));
        self.settings_window(&ctx);
        self.ask_window(&ctx);
    }
}
