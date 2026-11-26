# RustWeb

Desktop client **WhatsApp Web** untuk macOS yang ringan, dibangun dengan **Rust** murni dan **Chromium Embedded Framework (CEF)**.

> Tanpa Tauri, tanpa WKWebView, tanpa auto-updater. Satu akun, satu jendela, langsung jalan.

---

## ✨ Fitur

- 💬 **WhatsApp Web native** — jalankan WhatsApp Web di jendela desktop tersendiri
- 🔔 **Notifikasi native macOS** — notifikasi muncul di Notification Center + indikator *unread* di menu bar
- 📱 **Menu bar (tray) icon** — klik untuk show/hide jendela, klik kanan untuk menu *Show* / *Quit*
- 🎥 **Camera & mikrofon otomatis** — izin media langsung diberikan untuk panggilan video/voice
- 🖥️ **Fullscreen native** — sinkron dengan tombol hijau macOS dan shortcut `⌃⌘F`
- 🔗 **Link eksternal** — otomatis dibuka di browser default, bukan di dalam aplikasi
- ⚡ **Ringan di RAM & CPU** — dengan optimasi flag Chromium (lihat below)
- 🔐 **Session persisten** — login tetap tersimpan di `~/Library/Application Support/RustWeb/profile`

---

## 🛠️ Teknologi

| Komponen | Versi / Keterangan |
|----------|--------------------|
| Bahasa | Rust 2021 |
| Engine | CEF (`cef` crate) 132.3.2 — setara Chromium 132 |
| UI Native | `objc2`, `objc2-app-kit`, `objc2-foundation` (AppKit) |
| Logging | `chrono` |
| Target | macOS Ventura 13.0+ (Intel, x86_64) |

---

## 📦 Install

1. Buka halaman **[Releases](https://github.com/Syuhadak27/RustWeb/releases)**
2. Download `RustWeb-macOS-Ventura-Intel.dmg`
3. Buka DMG, copy `RustWeb.app` ke folder **Applications**
4. **Klik kanan → Open** pada pertama kali (karena aplikasi di-*sign* ad-hoc, belum di-notarize Apple)
5. Scan QR code WhatsApp Web seperti biasa

> 💡 Setelah pertama kali dibuka dengan klik kanan, macOS akan mengingatnya dan kamu bisa membukanya seperti aplikasi biasa.

---

## 🔨 Build dari source

Butuh macOS + Rust toolchain + CEF 132.3.2.

```bash
# Install target
rustup target add x86_64-apple-darwin

# Set path CEF yang sudah diekstrak
export CEF_PATH=/path/ke/cef

# Build release
MACOSX_DEPLOYMENT_TARGET=13.0 cargo build --release --target x86_64-apple-darwin
```

Atau cukup push tag `v*` — GitHub Actions (`.github/workflows/build-macos.yml`) akan otomatis build, sign, buat DMG, dan publish release.

---

## ⚙️ Optimasi RAM & CPU

Beberapa flag Chromium yang dipakai untuk menekan konsumsi resource:

- `max-active-renderer-processes=4` — batasi proses renderer
- `disable-gpu-vsync`, `enable-gpu-rasterization`, `enable-zero-copy` — rendering lebih efisien
- `disable-site-isolation-trials` — kurangi fragmentasi proses
- `disable-features=CalculateNativeWinOcclusion,InterestFeedContentSuggestions`
- `disable-background-timer-throttling` + `disable-renderer-backgrounding` — WebSocket WA tetap hidup saat jendela disembunyikan

---

## 📁 Struktur

```
RustWeb/
├── src/
│   ├── main.rs      # Browser process + AppKit tray/menu/notifikasi
│   └── helper.rs    # Child process CEF (GPU, renderer, plugin, alerts)
├── assets/          # Ikon tray (normal & unread)
├── .github/workflows/build-macos.yml  # CI build & release
├── Cargo.toml
├── Info.plist
├── entitlements.plist
└── README.md
```

---

## 🐞 Logging

Log aplikasi tersimpan di:

```
~/Library/Application Support/RustWeb/launch.log
```

Format: `[YYYY-MM-DD HH:MM:SS] pesan`

---

## 📄 Lisensi

Dirilis di bawah **MIT License** — lihat file [LICENSE](LICENSE).

---

## 🙏 Credits

- [Chromium Embedded Framework](https://bitbucket.org/chromiumembedded/cef) — engine browser
- [`cef` Rust bindings](https://crates.io/crates/cef)
- [`objc2`](https://crates.io/crates/objc2) — bindings Objective-C untuk Rust
