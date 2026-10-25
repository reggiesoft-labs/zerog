# ZeroG

**One mouse and keyboard. Every computer.**

ZeroG lets you seamlessly share your mouse and keyboard across multiple computers on your local network. Move your cursor past the edge of one screen to instantly take control of the computer beside it.

Free, open-source, and runs entirely on your local network with zero cloud accounts or hardware switches.

---

## Downloads

Download the latest version for your platform from the [GitHub Releases](https://github.com/reggiesoft-labs/zerog/releases/latest) page:

| Platform | Installer | Notes |
| :--- | :--- | :--- |
| **Windows** | [ZeroG Setup (.exe)](https://github.com/reggiesoft-labs/zerog/releases/latest) | Windows 10 / 11 (64-bit) |
| **macOS** | [ZeroG Universal (.dmg)](https://github.com/reggiesoft-labs/zerog/releases/latest) | Apple Silicon & Intel (macOS 12+) |
| **Linux (Debian/Ubuntu)** | [ZeroG (.deb)](https://github.com/reggiesoft-labs/zerog/releases/latest) | Ubuntu, Debian, Pop!_OS |
| **Linux (Fedora/RHEL)** | [ZeroG (.rpm)](https://github.com/reggiesoft-labs/zerog/releases/latest) | Fedora, openSUSE, RHEL |
| **Linux (Universal)** | [ZeroG (.AppImage)](https://github.com/reggiesoft-labs/zerog/releases/latest) | Standalone portable bundle |

Also available on the official website: [reggiesoft.com/zerog](https://reggiesoft.com/zerog/).

---

## How It Works

1. **Install ZeroG** on two or more computers connected to the same Wi-Fi or local network.
2. **Open the app** — ZeroG automatically discovers nearby computers on your network.
3. **Arrange your displays** visually in the app to match your desk layout, and cross screen edges seamlessly.

---

## Features

* **Instant Edge Crossing**: Move smoothly between laptops, desktops, and secondary workstations.
* **Visual Display Arrangement**: Drag and drop screens in the app to align your monitors exactly as they sit on your desk.
* **Local & Private**: Communication stays 100% on your local network. No accounts, no tracking, and no external servers.
* **Low Latency**: Lightweight peer-to-peer engine written in Rust for minimal input lag.
* **Broad OS Support**: Native support for macOS, Windows, and Linux (both Wayland and X11).

---

## Platform Permissions

* **macOS**: Requires granting **Accessibility** permissions under *System Settings > Privacy & Security > Accessibility* so ZeroG can capture and move your cursor across edges.
* **Windows**: Works out of the box with standard user privileges.
* **Linux**: Automatically selects native Wayland portal input or X11 depending on your desktop environment.

---

## Support ZeroG & Reggie

ZeroG is completely free and open-source. If it improves your workflow, you can support development and help buy treats for **Reggie** (the Chief Feline Officer of ReggieSoft):

👉 **[Support ZeroG on Polar](https://buy.polar.sh/polar_cl_hkXxCS8cRT69c9Ag7UpdY2aKFBIsfrpCjy7Eq0BD0jE)**

---

<details>
<summary><strong>Building from Source & Advanced Development</strong></summary>

### Prerequisites

* Rust toolchain (stable)
* Node.js 20+ & npm

### Build the Desktop Application

```bash
# Clone the repository
git clone https://github.com/reggiesoft-labs/zerog.git
cd zerog

# Install frontend dependencies and run dev server
cd gui
npm install
npm run tauri dev
```

### Production Bundles

```bash
cd gui
npm run tauri build
```

### Protocol & Architecture

ZeroG communicates peer-to-peer over local UDP using QUIC for reliable, encrypted input transport and mDNS for automatic zero-configuration discovery (`_zerog._udp.local.`).

For architecture notes, internal test helpers, and platform-specific diagnostics, see:
* Architecture overview: `diagrams/architecture.md`
* Peer detection specs: `docs/peer-detection.md`
* Linux compositor validation: `docs/linux-test-matrix.md`

</details>

---

## License

Free and open-source software by [ReggieSoft](https://reggiesoft.com/).
