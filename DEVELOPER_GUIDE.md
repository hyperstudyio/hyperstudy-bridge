# HyperStudy Bridge Developer Guide

## Table of Contents
1. [Architecture Overview](#architecture-overview)
2. [Development Setup](#development-setup)
3. [Project Structure](#project-structure)
4. [Adding New Devices](#adding-new-devices)
5. [Testing](#testing)
6. [Building and Deployment](#building-and-deployment)
7. [Contributing](#contributing)

## Architecture Overview

HyperStudy Bridge is built with a modular, plugin-based architecture:

```
┌─────────────────┐     WebSocket      ┌──────────────┐
│   HyperStudy    │◄──────────────────►│    Bridge    │
│   Web App       │    ws://localhost   │    Server    │
└─────────────────┘        :9000        └──────┬───────┘
                                                │
                                    ┌───────────┼───────────┐
                                    │           │           │
                              ┌─────▼───┐ ┌────▼────┐ ┌────▼────┐
                              │   TTL   │ │ Kernel  │ │  Pupil  │
                              │ Device  │ │ Device  │ │ Device  │
                              └─────┬───┘ └────┬────┘ └────┬────┘
                                    │          │           │
                              Serial USB   TCP Socket  WebSocket
```

### Technology Stack

**Backend (Rust)**
- Tauri framework for desktop integration
- Tokio for async runtime
- Device traits for modularity
- WebSocket server using tokio-tungstenite

**Frontend (Svelte 5)**
- Runes for reactive state management
- TypeScript for type safety
- Tailwind CSS for styling
- WebSocket client for real-time updates

## Development Setup

### Prerequisites

1. **Install Rust**
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```

2. **Install Node.js** (v18+)
   ```bash
   # Using nvm
   nvm install 18
   nvm use 18
   ```

3. **Install pnpm**
   ```bash
   npm install -g pnpm
   ```

4. **Platform-specific Requirements**

   **macOS:**
   - Xcode Command Line Tools: `xcode-select --install`

   **Windows:**
   - Visual Studio 2022 with C++ build tools
   - WebView2 runtime

   **Linux:**
   - Development packages: `sudo apt-get install libwebkit2gtk-4.0-dev build-essential curl wget libssl-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev`

### Clone and Install

```bash
# Clone repository
git clone https://github.com/your-org/hyperstudy-bridge.git
cd hyperstudy-bridge

# Install dependencies
pnpm install

# Install Rust dependencies
cd src-tauri
cargo build
cd ..
```

### Development Commands

```bash
# Start development server
pnpm tauri dev

# Run frontend only
pnpm dev

# Build for production
pnpm tauri build

# Run tests
pnpm test
cargo test

# Lint and format
pnpm lint
cargo fmt
cargo clippy
```

## Project Structure

```
hyperstudy-bridge/
├── src/                    # Svelte frontend
│   ├── lib/
│   │   ├── components/    # UI components
│   │   ├── stores/        # State management
│   │   └── types/         # TypeScript definitions
│   ├── routes/            # Application routes
│   └── app.html          # HTML template
│
├── src-tauri/            # Rust backend
│   ├── src/
│   │   ├── devices/      # Device modules
│   │   │   ├── mod.rs    # Device trait
│   │   │   ├── ttl/      # TTL implementation
│   │   │   ├── kernel/   # Kernel implementation
│   │   │   ├── pupil/    # Pupil implementation
│   │   │   └── lsl/      # LSL implementation
│   │   ├── bridge/       # Bridge core
│   │   │   ├── mod.rs    # Bridge module
│   │   │   ├── websocket.rs  # WS server
│   │   │   └── state.rs  # App state
│   │   └── main.rs       # Entry point
│   ├── Cargo.toml        # Rust dependencies
│   └── tauri.conf.json   # Tauri configuration
│
├── tests/                # E2E tests
├── package.json          # Node dependencies
└── README.md            # Project documentation
```

## Adding New Devices

### Step 1: Define Device Module

Create a new module in `src-tauri/src/devices/your_device/`:

```rust
// src-tauri/src/devices/your_device/mod.rs

use async_trait::async_trait;
use crate::devices::{Device, DeviceConfig, DeviceError, DeviceInfo, DeviceStatus};

pub struct YourDevice {
    info: DeviceInfo,
    connection: Option<YourConnection>,
    config: YourConfig,
}

#[async_trait]
impl Device for YourDevice {
    async fn connect(&mut self) -> Result<(), DeviceError> {
        // Implement connection logic
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), DeviceError> {
        // Implement disconnection logic
        Ok(())
    }

    async fn send(&mut self, data: &[u8]) -> Result<(), DeviceError> {
        // Implement send logic
        Ok(())
    }

    fn get_info(&self) -> DeviceInfo {
        self.info.clone()
    }

    fn get_status(&self) -> DeviceStatus {
        // Return current status
        DeviceStatus::Connected
    }

    fn configure(&mut self, config: DeviceConfig) -> Result<(), DeviceError> {
        // Apply device-specific settings from config.custom_settings
        Ok(())
    }
}
```

### Step 2: Register Device Type

Update `src-tauri/src/devices/mod.rs`:

```rust
pub enum DeviceType {
    Ttl,
    Kernel,
    Pupil,
    Lsl,
    YourDevice,  // Add your device
}

pub fn create_device(device_type: DeviceType) -> Box<dyn Device> {
    match device_type {
        DeviceType::YourDevice => Box::new(YourDevice::new()),
        // ... other devices
    }
}
```

### Step 3: Add WebSocket Handlers

Update `src-tauri/src/bridge/websocket.rs`:

```rust
async fn handle_command(command: BridgeCommand, state: Arc<AppState>) -> BridgeResponse {
    match command.device.as_str() {
        "your_device" => handle_your_device_command(command, state).await,
        // ... other devices
    }
}

async fn handle_your_device_command(
    command: BridgeCommand,
    state: Arc<AppState>
) -> BridgeResponse {
    // Implement command handling
}
```

### Step 4: Create Frontend Component

Create `src/lib/components/devices/YourDeviceCard.svelte`:

```svelte
<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import type { YourDeviceConfig } from '$lib/types';

  let { device, onConnect, onDisconnect } = $props();

  const dispatch = createEventDispatcher();

  function handleConfigure() {
    dispatch('configure', { device });
  }
</script>

<div class="device-card">
  <h3>{device.name}</h3>
  <p>Status: {device.connected ? 'Connected' : 'Disconnected'}</p>

  {#if device.connected}
    <button onclick={onDisconnect}>Disconnect</button>
  {:else}
    <button onclick={onConnect}>Connect</button>
  {/if}

  <button onclick={handleConfigure}>Configure</button>
</div>
```

### Step 5: Add Tests

Create `src-tauri/src/devices/your_device/tests.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_connection() {
        let mut device = YourDevice::new();
        assert!(device.connect().await.is_ok());
        assert_eq!(device.get_status().await, DeviceStatus::Connected);
    }

    #[tokio::test]
    async fn test_send_command() {
        let mut device = YourDevice::new();
        device.connect().await.unwrap();

        let result = device.send(b"TEST").await;
        assert!(result.is_ok());
    }
}
```

## Testing

### Unit Tests

```bash
# Run Rust tests
cd src-tauri
cargo test

# Run with coverage
cargo tarpaulin --out Html

# Run frontend tests
pnpm test:unit
```

### Integration Tests

```bash
# Run integration tests
cargo test --test '*'

# Run specific test
cargo test test_websocket_connection
```

### E2E Tests

```bash
# Run Playwright tests
pnpm test:e2e

# Run in headed mode
pnpm test:e2e --headed

# Run specific test file
pnpm test:e2e tests/device-connection.spec.ts
```

### Performance Testing

```rust
// Benchmark TTL latency
#[bench]
fn bench_ttl_pulse(b: &mut Bencher) {
    let device = TtlDevice::new();
    b.iter(|| {
        device.send(b"PULSE").await
    });
}
```

## Building and Deployment

### Local Build

```bash
# Debug build
pnpm tauri build --debug

# Release build
pnpm tauri build

# Build for specific platform
pnpm tauri build --target x86_64-apple-darwin
```

### Code Signing (macOS)

1. **Get Developer ID Certificate**
   - Enroll in Apple Developer Program
   - Create Developer ID Application certificate

2. **Configure Tauri**
   ```json
   // tauri.conf.json
   {
     "tauri": {
       "bundle": {
         "macOS": {
           "signingIdentity": "Developer ID Application: Your Name",
           "providerShortName": "YOURTEAMID"
         }
       }
     }
   }
   ```

3. **Sign and Notarize**
   ```bash
   # Build and sign
   pnpm tauri build

   # Notarize
   xcrun notarytool submit path/to/app.dmg \
     --apple-id your@email.com \
     --team-id YOURTEAMID \
     --password app-specific-password
   ```

### CI/CD with GitHub Actions

```yaml
# .github/workflows/release.yml
name: Release
on:
  push:
    tags:
      - 'v*'

jobs:
  release:
    strategy:
      matrix:
        platform: [macos-latest, windows-latest, ubuntu-latest]

    runs-on: ${{ matrix.platform }}

    steps:
      - uses: actions/checkout@v3

      - name: Setup Node
        uses: actions/setup-node@v3
        with:
          node-version: 18

      - name: Setup Rust
        uses: actions-rs/toolchain@v1
        with:
          toolchain: stable

      - name: Install dependencies
        run: pnpm install

      - name: Build
        run: pnpm tauri build
        env:
          TAURI_PRIVATE_KEY: ${{ secrets.TAURI_PRIVATE_KEY }}
          APPLE_CERTIFICATE: ${{ secrets.APPLE_CERTIFICATE }}
          APPLE_CERTIFICATE_PASSWORD: ${{ secrets.APPLE_CERTIFICATE_PASSWORD }}

      - name: Upload artifacts
        uses: actions/upload-artifact@v3
        with:
          name: ${{ matrix.platform }}
          path: src-tauri/target/release/bundle/
```

## Contributing

### Development Workflow

1. **Fork and Clone**
   ```bash
   git clone https://github.com/your-username/hyperstudy-bridge.git
   cd hyperstudy-bridge
   git remote add upstream https://github.com/original/hyperstudy-bridge.git
   ```

2. **Create Feature Branch**
   ```bash
   git checkout -b feature/your-feature
   ```

3. **Make Changes**
   - Write code following style guidelines
   - Add tests for new functionality
   - Update documentation

4. **Commit with Conventional Commits**
   ```bash
   git commit -m "feat: add new device support"
   git commit -m "fix: resolve connection timeout"
   git commit -m "docs: update API documentation"
   ```

5. **Push and Create PR**
   ```bash
   git push origin feature/your-feature
   ```

### Code Style

**Rust:**
- Use `rustfmt` for formatting
- Follow Rust API guidelines
- Document public APIs
- Use descriptive variable names

**TypeScript/Svelte:**
- Use ESLint configuration
- Follow Svelte 5 best practices
- Use TypeScript strict mode
- Component names in PascalCase

### Testing Requirements

- Minimum 80% code coverage
- All tests must pass
- Add tests for bug fixes
- Performance benchmarks for critical paths

### Documentation

- Update API docs for new endpoints
- Add JSDoc comments for functions
- Update CHANGELOG.md
- Include examples in docs

## Debugging

### Debug Mode

```bash
# Enable debug logging
RUST_LOG=debug pnpm tauri dev

# Frontend debugging
pnpm dev --debug
```

### DevTools

1. **Open DevTools in Tauri**
   - Right-click in app window
   - Select "Inspect Element"

2. **Rust Debugging**
   ```toml
   # .vscode/launch.json
   {
     "type": "lldb",
     "request": "launch",
     "name": "Debug Tauri",
     "cargo": {
       "args": ["build", "--manifest-path=./src-tauri/Cargo.toml"]
     }
   }
   ```

### Common Issues

**Issue: Device not detected**
```rust
// Add debug logging
log::debug!("Scanning for devices: {:?}", ports);
```

**Issue: WebSocket connection failed**
```javascript
// Check WebSocket state
console.log('WebSocket state:', ws.readyState);
```

**Issue: High memory usage**
```bash
# Profile memory
cargo build --release
valgrind --tool=massif ./target/release/hyperstudy-bridge
```

## Performance Optimization

### Profiling

```bash
# CPU profiling
cargo build --release
perf record -g ./target/release/hyperstudy-bridge
perf report

# Memory profiling
heaptrack ./target/release/hyperstudy-bridge
```

### Optimization Tips

1. **Use async/await properly**
   ```rust
   // Good: Concurrent operations
   let (result1, result2) = tokio::join!(
       operation1(),
       operation2()
   );

   // Bad: Sequential operations
   let result1 = operation1().await;
   let result2 = operation2().await;
   ```

2. **Minimize allocations**
   ```rust
   // Use pre-allocated buffers
   let mut buffer = Vec::with_capacity(1024);
   ```

3. **Cache frequently accessed data**
   ```rust
   use once_cell::sync::Lazy;
   static CACHE: Lazy<HashMap<String, Data>> = Lazy::new(HashMap::new);
   ```

## Security Considerations

1. **Input Validation**
   ```rust
   fn validate_command(cmd: &str) -> Result<(), Error> {
       if cmd.len() > MAX_COMMAND_LENGTH {
           return Err(Error::InvalidInput);
       }
       // Additional validation
   }
   ```

2. **Secure Communication**
   - Use TLS for production WebSocket
   - Validate all incoming messages
   - Sanitize error messages

3. **Resource Limits**
   - Implement rate limiting
   - Set maximum buffer sizes
   - Timeout long-running operations

## Resources

- [Tauri Documentation](https://tauri.app/v1/guides/)
- [Svelte 5 Documentation](https://svelte.dev/docs)
- [Tokio Tutorial](https://tokio.rs/tokio/tutorial)
- [WebSocket Protocol](https://datatracker.ietf.org/doc/html/rfc6455)

## Support

- GitHub Issues: Bug reports and feature requests
- Discord: Real-time help and discussions
- Email: dev@hyperstudy.io

---

For user-facing documentation, see the [User Guide](USER_GUIDE.md).
For API specifications, see the [API Documentation](API_DOCUMENTATION.md).