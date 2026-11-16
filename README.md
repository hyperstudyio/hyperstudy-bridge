# HyperStudy Bridge

A unified, high-performance desktop application for bridging HyperStudy web experiments with research hardware devices.

<div align="center">

[![Release](https://img.shields.io/github/v/release/hyperstudyio/hyperstudy-bridge?include_prereleases&style=for-the-badge)](https://github.com/hyperstudyio/hyperstudy-bridge/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/hyperstudyio/hyperstudy-bridge/total?style=for-the-badge)](https://github.com/hyperstudyio/hyperstudy-bridge/releases)
[![License](https://img.shields.io/github/license/hyperstudyio/hyperstudy-bridge?style=for-the-badge)](LICENSE)

### [Download Latest Release](https://github.com/hyperstudyio/hyperstudy-bridge/releases/latest)

| Platform | Download |
|----------|----------|
| **macOS (Apple Silicon)** | [Apple Silicon](#) |
| **macOS (Intel)** | [Intel](#) |
| **Windows** | [Windows Installer](#) |
| **Linux** | [Linux AppImage](#) |

_macOS builds are signed and notarized by Apple. Windows builds are unsigned (see note below)._

</div>

## Overview

HyperStudy Bridge provides a reliable, low-latency communication layer between the HyperStudy web application and various research devices including fNIRS, eye trackers, physiological sensors, and TTL pulse generators. Built with Tauri and Rust for maximum performance and minimal resource usage.

## Features

- **High Performance**: Sub-millisecond latency for time-critical operations
- **Multi-Device Support**: Simultaneous connection to multiple device types
- **Auto-Reconnection**: Resilient connection management with automatic recovery
- **Real-Time Monitoring**: Live status dashboard for all connected devices
- **Secure**: Local-only connections with sandboxed architecture
- **Cross-Platform**: macOS (primary), Windows, and Linux support

## Supported Devices

| Device | Type | Connection | Status |
|--------|------|------------|--------|
| [hyperstudy-ttl](https://github.com/hyperstudyio/hyperstudy-ttl) | TTL Pulse Generator | USB Serial | Supported |
| Kernel Flow2 | fNIRS | TCP Socket | Supported |
| Pupil Labs Neon | Eye Tracker | REST API + LSL | Supported |
| Earable FRENZ Brainband | EEG / biosignals | Bluetooth + LSL | Supported |
| Lab Streaming Layer | Various | LSL Protocol | Supported |
| EyeLink 1000 Plus | Eye Tracker | Ethernet (TCP) | Under development* |

_*EyeLink support is unfinished and turned off in released builds: Bridge answers EyeLink commands with "not supported". See [EyeLink (in development)](#eyelink-in-development)._

## Quick Start

### Installation

#### macOS
1. Download the appropriate `.dmg` for your Mac from the [Download section above](#download-latest-release)
2. Open the DMG and drag HyperStudy Bridge to Applications
3. Launch from Applications folder

#### Windows
1. Download the `.msi` installer from the [Download section above](#download-latest-release)
2. Run the installer and follow the prompts
3. Launch from the Start Menu

> **Note**: Windows may show a SmartScreen warning on first run since the app is not signed.
> Click "More info" → "Run anyway" to proceed.

#### Linux
1. Download the `.AppImage` from the [Download section above](#download-latest-release)
2. Make it executable and run:
   ```bash
   chmod +x HyperStudy-Bridge-*.AppImage
   ./HyperStudy-Bridge-*.AppImage
   ```

### EyeLink (in development)

EyeLink 1000 Plus support is not finished and is compiled out of released builds. To work on it, build with the
`eyelink` Cargo feature (`cargo build --features eyelink`) on a machine with the SR Research
[EyeLink Developers Kit](https://www.sr-research.com/support) installed; the SDK is loaded at runtime.

### Usage

1. **Launch the Bridge**: Start HyperStudy Bridge before your experiment
2. **Connect Devices**: Click "Connect All" or configure individual devices
3. **Verify Status**: Ensure all required devices show green status
4. **Start Experiment**: The bridge will handle all device communication

## Development

### Prerequisites

- [Rust](https://rustup.rs/) (latest stable)
- [Node.js](https://nodejs.org/) (v18+)
- [Tauri CLI](https://tauri.app/v1/guides/getting-started/prerequisites)

### Setup

```bash
# Clone the repository
git clone https://github.com/yourusername/hyperstudy-bridge.git
cd hyperstudy-bridge

# Install dependencies
npm install

# Install Rust dependencies
cd src-tauri
cargo build
cd ..
```

### Development Mode

```bash
# Run in development mode with hot-reload
npm run tauri dev
```

### Testing

```bash
# Run all tests
npm test

# Backend tests only
cd src-tauri && cargo test

# Frontend tests only
npm run test:frontend

# E2E tests
npm run test:e2e
```

### Building

```bash
# Build for current platform
npm run tauri build

# Build for specific platform
npm run tauri build -- --target x86_64-apple-darwin
```

## Architecture

```
┌─────────────────┐     WebSocket      ┌──────────────┐
│   HyperStudy    │◄──────:9000───────►│    Bridge    │
│   Web App       │                     │   Server     │
└─────────────────┘                     └──────┬───────┘
                                                │
                          ┌─────────────────────┼─────────────────────┐
                          │                     │                     │
                    ┌─────▼─────┐         ┌────▼────┐         ┌─────▼─────┐
                    │    TTL    │         │ Kernel  │         │  Pupil    │
                    │  Serial   │         │   TCP   │         │    WS     │
                    └─────┬─────┘         └────┬────┘         └─────┬─────┘
                          │                     │                     │
                    ┌─────▼─────┐         ┌────▼────┐         ┌─────▼─────┐
                    │ TTL Device│         │ Flow2   │         │   Neon    │
                    └───────────┘         └─────────┘         └───────────┘
```

## API Documentation

### WebSocket Protocol

Connect to `ws://localhost:9000` and send JSON messages:

```javascript
// Connect to device
{
  "type": "command",
  "device": "ttl",
  "action": "connect",
  "payload": { "port": "/dev/tty.usbmodem1234" }
}

// Send command
{
  "type": "command",
  "device": "ttl",
  "action": "send",
  "payload": { "command": "PULSE" }
}

// Receive data
{
  "type": "data",
  "device": "kernel",
  "payload": { /* device-specific data */ },
  "timestamp": 1634567890123
}
```

See [API Documentation](docs/api/README.md) for complete protocol specification.

## Configuration

Settings are stored in:
- **macOS**: `~/Library/Application Support/hyperstudy-bridge/`
- **Windows**: `%APPDATA%\hyperstudy-bridge\`
- **Linux**: `~/.config/hyperstudy-bridge/`

Example configuration:
```json
{
  "bridge": {
    "port": 9000,
    "autoConnect": true
  },
  "devices": {
    "ttl": {
      "port": "/dev/tty.usbmodem1234",
      "baudRate": 115200
    },
    "kernel": {
      "ip": "192.168.1.100",
      "port": 6767
    }
  }
}
```

### Saved device credentials

The FRENZ band name and product key (entered under **Configure** on the FRENZ card) are kept in the operating
system's credential store, not in a settings file:

- **macOS**: the login Keychain
- **Windows**: Credential Manager
- **Linux**: the Secret Service (GNOME Keyring or KWallet); without one running, the key cannot be saved

Each entry has the service name `io.hyperstudy.bridge` and the key name (`frenz_device_id`, `frenz_product_key`) as
its account. The name does not depend on the app identifier, so the installed app and a development build share the
same saved key. On macOS, the first time one build reads an entry the other saved, macOS may ask to allow access.

Bridge 0.8 and earlier kept these values in an encrypted Stronghold file, `vault.hold`, in the app data folder.
On first launch, a newer Bridge moves them into the credential store and deletes that file. If the file cannot be
read, it is left in place, a warning is logged, and the product key can be entered again under **Configure**.

## Troubleshooting

### Common Issues

#### Serial Port Access (macOS/Linux)
```bash
# Add user to dialout group (Linux)
sudo usermod -a -G dialout $USER

# Check port permissions (macOS)
ls -la /dev/tty.*
```

#### Port Already in Use
```bash
# Find process using port 9000
lsof -i :9000

# Kill process if needed
kill -9 <PID>
```

#### Device Not Detected
1. Check device is powered on and connected
2. Verify drivers are installed
3. Try unplugging and reconnecting
4. Check device appears in system (Device Manager/System Information)

See [Troubleshooting Guide](docs/troubleshooting.md) for more solutions.

## Performance

| Metric | Target | Typical |
|--------|--------|---------|
| TTL Latency | <1ms | 0.5ms |
| Message Throughput | >1000/sec | 1500/sec |
| Memory Usage | <100MB | 45MB |
| CPU Usage (idle) | <5% | 2% |
| Startup Time | <2sec | 1.2sec |

## Contributing

We welcome contributions! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

### Development Workflow
1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Add tests
5. Submit a pull request

## License

MIT License - see [LICENSE](LICENSE) file for details.

## Support

- **Documentation**: [docs/](docs/)
- **Issues**: [GitHub Issues](https://github.com/yourusername/hyperstudy-bridge/issues)
- **Discussions**: [GitHub Discussions](https://github.com/yourusername/hyperstudy-bridge/discussions)

## Acknowledgments

- HyperStudy team for the core platform
- Tauri team for the excellent framework
- Device manufacturers for their APIs and documentation

## Roadmap

- [x] Core bridge architecture
- [x] TTL pulse generator support
- [x] Kernel Flow2 integration
- [x] Pupil Labs Neon support
- [x] Lab Streaming Layer support
- [ ] Auto-update system
- [ ] Device profiles and presets
- [ ] Data recording and playback
- [ ] Advanced diagnostics tools

---

Built for the research community