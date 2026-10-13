// Give scansend.exe the ScanSend icon, so Explorer, the taskbar and the installer's
// shortcuts show it (the window icon alone only appears while it's running).
fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../mobile/src-tauri/icons/icon.ico");
        res.set("ProductName", "ScanSend");
        res.set("FileDescription", "ScanSend");
        res.compile().expect("embed the Windows icon");
    }
}
