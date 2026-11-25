# CLAUDE.md - HyperStudy Bridge Development Guide

This file provides comprehensive guidance for Claude Code and other AI assistants working on the HyperStudy Bridge project. It includes architecture specifications, development guidelines, and multi-agent coordination instructions.

## Project Overview

HyperStudy Bridge is a unified, high-performance desktop application that serves as a communication bridge between the HyperStudy web application and various research hardware devices. Built with Tauri (Rust backend) and Svelte 5 (frontend), it provides low-latency, reliable device integration with a clean, minimal UI.

## Core Architecture

### Technology Stack
- **Backend**: Rust with Tokio async runtime
- **Frontend**: Svelte 5 with Runes mode
- **Framework**: Tauri for cross-platform desktop application
- **Communication**: WebSocket server for web app integration
- **Testing**: Rust tests, Vitest, Playwright

### Design Principles
1. **Modularity**: Plugin-based device architecture using Rust traits
2. **Performance**: Sub-millisecond latency for time-critical operations
3. **Reliability**: Automatic reconnection, error recovery, graceful degradation
4. **Simplicity**: Minimal UI showing only essential status information
5. **Extensibility**: Easy addition of new device types without core changes

## Device Specifications

### 1. TTL Pulse Generator ([hyperstudy-ttl](https://github.com/hyperstudyio/hyperstudy-ttl))
- **Connection**: USB Serial (CDC)
- **Protocol**: Simple text commands over serial
- **Critical Requirement**: <1ms command-to-pulse latency
- **Commands**: `PULSE [duration_ms]\n` triggers TTL output, `TIMING\n` queries on-device latency
- **Implementation**: Using `serialport` crate
- **Hardware**: See [hyperstudy-ttl repository](https://github.com/hyperstudyio/hyperstudy-ttl) for firmware and setup

### 2. Kernel Flow2 fNIRS
- **Connection**: TCP socket on port 6767
- **Protocol**: Binary data over TCP
- **Features**: Bidirectional communication, status monitoring
- **Implementation**: Using `tokio::net::TcpStream`

### 3. Pupil Labs Neon Eye Tracker
- **Control**: HTTP REST API of the Neon Companion app (port 8080), using `reqwest` (`devices/pupil.rs`)
- **Discovery**: mDNS / DNS-SD browse for Companion phones (`devices/neon_discovery.rs`)
- **Gaze data**: LSL inlets on the Companion app's gaze and event streams (`devices/lsl/neon.rs`)
- **Features**: Recording control, event markers, gaze streaming

### 4. Lab Streaming Layer
- **Connection**: LSL network protocol, using the `lsl` crate
- **Used by**: Neon gaze (above), the FRENZ brainband (a Python sidecar publishes its streams over LSL; `devices/lsl/frenz.rs`, `devices/lsl/frenz_process.rs`), and study recordings of LSL streams (`recording/`)

### 4a. FRENZ connection: SDK helper or openfrenz
- **Setting**: FRENZ `source` in the device configuration. Absent means the SDK helper (the default); `openfrenz` selects
  the native connection. The two must never hold the band together; the start commands refuse while the other is active.
- **SDK helper**: the Python sidecar above, with the vendor's derived streams (focus, sleep stage, posture), product key needed.
- **openfrenz** (`devices/frenz_native.rs`): Bridge connects over Bluetooth with the `frenz` crate, runs openfrenz's
  built-in models (`eog`, `brow-emg`, `heart`, `head`) and publishes everything as LSL outlets named `<band>_<STREAM>` with
  source id `frenz-<band>-<session uuid>_<STREAM>`, the pattern HyperStudy groups FRENZ streams by. The window draws them in
  `FrenzLivePanel.svelte`. Experimental; the SDK helper stays until openfrenz replaces it.
- **Extra models**: the FRENZ `models` setting lists Hugging Face repositories (`owner/name`) run alongside the
  built-in ones, published as `<band>_<NAME>_openfrenz` and drawn in the live view. Lab models are private in the
  cosanlab Hugging Face org and shared by invitation; the token is `huggingface_token` in the credential store,
  read by the backend (`start_frenz_native`), never passed from the window. A model that cannot be loaded is shown
  on the card and left out.
- **Tests**: `src-tauri/tests/frenz_native.rs` replays a synthetic recording; its `real_band` test (ignored by default)
  connects to a band named in `FRENZ_BAND`.

### 5. EyeLink 1000 Plus Eye Tracker
- **Connection**: Ethernet TCP/IP (default: 100.1.1.1)
- **Protocol**: eyelink_core C library via runtime FFI (libloading)
- **Features**: Gaze data streaming, calibration, EDF recording, event markers
- **Requirement**: SR Research EyeLink Developers Kit (detected at runtime)
- **Implementation**: Runtime dynamic loading via `libloading` crate — no build-time SDK needed

## WebSocket Bridge Protocol

The bridge exposes a WebSocket server on `ws://localhost:9000` for HyperStudy communication.

### Message Format

```typescript
// Client → Bridge
interface BridgeCommand {
  type: "command";
  device: "ttl" | "kernel" | "pupil" | "lsl";
  action: "connect" | "disconnect" | "send" | "configure" | "status";
  payload?: any;
  id?: string;  // For request-response correlation
}

// Bridge → Client
interface BridgeResponse {
  type: "status" | "data" | "error";
  device: "ttl" | "kernel" | "pupil" | "lsl";
  payload: any;
  id?: string;  // Matches request ID for correlation (single-message protocol)
  status?: string;  // Device status for status messages
  timestamp: number;
}
```

### Example Communications

```javascript
// Connect to TTL device
{
  "type": "command",
  "device": "ttl",
  "action": "connect",
  "payload": { "port": "/dev/tty.usbmodem1234" }
}

// Send TTL pulse
{
  "type": "command",
  "device": "ttl",
  "action": "send",
  "payload": { "command": "PULSE" }
}

// Stream data from Kernel
{
  "type": "command",
  "device": "kernel",
  "action": "connect",
  "payload": { "ip": "192.168.1.100" }
}
```

## Multi-Agent Development Strategy

### Agent Roles and Responsibilities

#### 1. Coordinator Agent
- **Primary Role**: Architecture decisions, code review, integration
- **Responsibilities**:
  - Maintain overall system architecture
  - Review PRs from other agents
  - Resolve conflicts between modules
  - Ensure code quality and consistency

#### 2. Backend Agent
- **Primary Role**: Rust backend development
- **Responsibilities**:
  - Core bridge server implementation
  - Device trait system design
  - WebSocket server development
  - State management and message routing
  - Performance optimization

#### 3. Frontend Agent
- **Primary Role**: Svelte UI development
- **Responsibilities**:
  - Status dashboard implementation
  - Device configuration UI
  - Real-time status updates
  - Log viewer and diagnostics
  - UI/UX consistency with HyperStudy

#### 4. DevOps Agent
- **Primary Role**: CI/CD and testing infrastructure
- **Responsibilities**:
  - GitHub Actions workflows
  - Testing framework setup
  - Code signing and notarization
  - Release automation
  - Dependency management

#### 5. Device Specialist Agents
Each device has a dedicated agent:
- **TTL Agent**: Serial communication, pulse timing
- **Kernel Agent**: TCP socket, data streaming
- **Pupil Agent**: Neon REST control, LSL gaze streaming
### Coordination Protocol

1. **Code Review**: Coordinator reviews all module integrations
2. **Communication**: Use PR comments for discussions
3. **Conflict Resolution**: Coordinator has final say on architecture

## Code Style Guidelines

### Rust Backend
```rust
// Use descriptive names
pub struct DeviceConnection {
    id: String,
    status: ConnectionStatus,
}

// Prefer Result types for error handling
pub fn connect(&mut self) -> Result<(), DeviceError> {
    // Implementation
}

// Use async/await for I/O operations
pub async fn send_command(&mut self, cmd: Command) -> Result<Response, Error> {
    // Implementation
}

// Document public APIs
/// Sends a TTL pulse to the connected device.
/// Returns an error if the device is not connected.
pub fn trigger_pulse(&mut self) -> Result<(), TtlError> {
    // Implementation
}
```

### Svelte Frontend
```javascript
// Use Svelte 5 runes
let count = $state(0);
let doubled = $derived(count * 2);

// Prefer named functions for event handlers
function handleConnect() {
  // Implementation
}

// Use TypeScript for type safety
interface DeviceStatus {
  id: string;
  connected: boolean;
  lastSeen: number;
}
```

## Testing Requirements

### Unit Tests
- Minimum 80% code coverage
- Test all device modules independently
- Mock external dependencies

### Integration Tests
- Test device communication protocols
- Verify message routing
- Test reconnection logic

### E2E Tests
- Full application flow
- Multi-device scenarios
- Error recovery testing

## Performance Requirements

1. **TTL Latency**: <1ms from command to pulse
2. **Data Throughput**: >1000 messages/second per device
3. **Memory Usage**: <100MB for application
4. **CPU Usage**: <5% idle, <20% active
5. **Startup Time**: <2 seconds to ready state

## Security Considerations

1. **Local Only**: Bridge only accepts connections from localhost
2. **No Credentials**: No passwords or keys stored in code. Device secrets (the FRENZ band name and product key, and a
   Hugging Face token for private models) live
   in the OS credential store via the `keyring` crate (`src-tauri/src/secrets/`, frontend `src/lib/services/secrets.js`):
   service `io.hyperstudy.bridge`, account = the key name, an allowlist of keys, and values never logged. The service
   name is fixed, so installed and dev builds share the saved key. The old Stronghold `vault.hold` is migrated once
   and deleted (`secrets/stronghold_migration.rs`, removal due 2027-03-01). Tests use keyring's mock store, never the
   real Keychain.
3. **Input Validation**: Sanitize all external inputs
4. **Error Messages**: Don't expose system details in errors
5. **Secure Communication**: Use TLS for production deployments

## Deployment Guidelines

### macOS
1. Code sign with Developer ID certificate
2. Notarize through Apple's service
3. Create DMG installer
4. Test on clean macOS system

### GitHub Actions
1. Use repository secrets for certificates
2. Automate signing and notarization
3. Create releases with binaries
4. Generate update manifests

## File Organization

```
src-tauri/src/
├── devices/
│   ├── mod.rs              # Device trait definition
│   ├── ttl.rs              # TTL pulse generator (tests in ttl_tests.rs)
│   ├── kernel.rs           # Kernel Flow2 (tests in kernel_tests.rs)
│   ├── pupil.rs            # Neon REST control
│   ├── neon_discovery.rs   # Neon mDNS discovery
│   ├── frenz_native.rs     # FRENZ through openfrenz (no SDK helper)
│   ├── mock.rs             # Mock device
│   ├── eyelink/            # EyeLink (behind the `eyelink` feature)
│   └── lsl/                # LSL inlets/outlets, Neon gaze, FRENZ
├── bridge/
│   ├── websocket.rs        # WS server
│   ├── message.rs          # Protocol messages
│   └── state.rs            # App state
├── recording/              # Study recordings of LSL streams
├── storage/                # SQLite log storage
├── commands.rs             # Tauri commands
└── main.rs                 # Entry point
```

## Common Patterns

### Device Module Template
```rust
pub struct DeviceName {
    connection: Option<Connection>,
    config: DeviceConfig,
    status: DeviceStatus,
}

impl Device for DeviceName {
    async fn connect(&mut self) -> Result<(), Error> {
        // Connect logic
    }
    
    async fn disconnect(&mut self) -> Result<(), Error> {
        // Disconnect logic
    }
    
    async fn send(&mut self, data: &[u8]) -> Result<(), Error> {
        // Send data logic
    }
}
```

### State Management
```rust
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct AppState {
    devices: Arc<RwLock<HashMap<String, Box<dyn Device>>>>,
    connections: Arc<RwLock<HashMap<String, WsConnection>>>,
}
```

## Development Workflow

1. **Branch Strategy**: Feature branches off main
2. **Commit Messages**: Conventional commits format. Do not include Co-Authored-By or AI attribution lines.
3. **PR Process**: Requires coordinator review
4. **Testing**: All tests must pass before merge
5. **Documentation**: Update docs with code changes

## Creating Releases

### Version Files
All three of these files must be updated with the new version number:
- `src-tauri/Cargo.toml` - Rust package version
- `package.json` - Node package version
- `src-tauri/tauri.conf.json` - Tauri app version (used by the in-app updater to detect new versions)

### Release Process

1. **Update version numbers** in all three files:
   ```bash
   # Edit src-tauri/Cargo.toml
   version = "X.Y.Z"

   # Edit package.json
   "version": "X.Y.Z"

   # Edit src-tauri/tauri.conf.json
   "version": "X.Y.Z"
   ```

2. **Update Cargo.lock** by running:
   ```bash
   cd src-tauri && cargo check
   ```

3. **Commit the version bump**:
   ```bash
   git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/tauri.conf.json package.json
   git commit -m "chore: Bump version to X.Y.Z"
   ```

4. **Push to main**:
   ```bash
   git push origin main
   ```

5. **Create the GitHub release**:
   ```bash
   gh release create vX.Y.Z --title "vX.Y.Z" --notes "Release notes here" --prerelease
   ```

   Or for a stable release (remove `--prerelease`):
   ```bash
   gh release create vX.Y.Z --title "vX.Y.Z" --notes "Release notes here"
   ```

### What Happens After

Creating a release triggers GitHub Actions workflows:
- **Create Release** - Builds macOS binaries (ARM64 and Intel), signs and notarizes them, then attaches to the release
- **Generate Update Manifest** - Creates `latest.json` for in-app auto-updates and attaches updater artifacts (`.tar.gz`, `.msi.zip`, `.sig` files)
- **Update README Download Links** - Updates download links in README.md
- **CI** - Runs tests on the tagged commit

**Required secrets for releases**: `TAURI_SIGNING_PRIVATE_KEY` must be set in GitHub repository secrets for the in-app updater to work. The private key is stored at `~/.tauri/hyperstudy-bridge.key` locally.

Monitor build progress at: https://github.com/hyperstudyio/hyperstudy-bridge/actions

### Release Notes Template

```markdown
## What's Changed

### New Features
- Feature description

### Bug Fixes
- Fix description

### Technical
- Technical change description
```

## Troubleshooting Guide

### Common Issues

1. **Serial Port Access** (macOS/Linux)
   - User needs to be in dialout/uucp group
   - May need to adjust permissions

2. **WebSocket Connection Refused**
   - Check if port 9000 is available
   - Verify firewall settings

3. **Device Not Found**
   - Ensure device drivers installed
   - Check USB/network connectivity

## AI Assistant Tools and Context

### Context7 MCP Integration
When working with libraries and frameworks in this project, use Context7 to get up-to-date documentation:
- **Usage**: Include "use context7" in prompts when working with Tauri, Svelte 5, Rust crates, etc.
- **Benefits**: Provides current, version-specific documentation and code examples
- **Example**: "use context7 to show me how to implement Tauri commands with async Rust"

### Development Reminders
When implementing features or fixing bugs:
1. Always fetch current library documentation using Context7 for accuracy
2. Verify API compatibility with the exact versions in `package.json` and `Cargo.toml`
3. Use Context7 especially for:
   - Svelte 5 runes syntax (rapidly evolving)
   - Tauri API changes between versions
   - Rust async patterns with Tokio
   - Device-specific SDKs and protocols

## Resources

- [Tauri Documentation](https://tauri.app)
- [Tokio Async Runtime](https://tokio.rs)
- [Svelte 5 Runes](https://svelte.dev/docs/runes)
- [Pupil Labs API](https://docs.pupil-labs.com/neon/real-time-api/)
- [Lab Streaming Layer](https://labstreaminglayer.org)

## Version History

- v0.1.0 - Initial architecture and core modules
- v0.2.0 - Device integration complete
- v0.3.0 - Testing and CI/CD pipeline
- v1.0.0 - Production release

---

*This document should be updated as the project evolves. All agents should refer to this document for architectural decisions and implementation guidelines.*